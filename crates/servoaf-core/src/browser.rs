// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! The browser: Servo, its tabs (one `WebView` each), pending page dialogs,
//! and the bridge from MCP requests to tool futures on the main thread.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use euclid::Scale;
use servo::{
    DeviceIndependentPixel, DeviceIntRect, DeviceIntSize, DevicePixel, DevicePoint, EmbedderControl,
    EmbedderControlId, LoadStatus, RenderingContext, ScreenGeometry, Servo, SimpleDialog, UserContentManager,
    UserScript, WebView, WebViewBuilder, WebViewDelegate,
};
use url::Url;

use crate::input::human_path::Point;
use crate::mcp::http::McpRequest;
use crate::mcp::protocol::McpProtocol;
use crate::mcp::registry::ToolRegistry;
use crate::runtime::{AsyncLock, Runtime, poll_until, timeout};

pub type TabId = u32;
/// There is a single window; get-tabs reports it so the result has the same
/// shape as chromeaf's.
pub const WINDOW_ID: u32 = 1;

#[derive(Clone, Debug)]
pub struct BrowserConfig {
    /// Accept alerts and dismiss confirm/prompt dialogs immediately instead of
    /// leaving them for respond-dialog.
    pub auto_dismiss_dialogs: bool,
    /// Draw the agent's pointer on top of the page (headed mode).
    pub cursor_overlay: bool,
    /// Ship a small built-in user script so the browser matches the one its
    /// user agent claims to be: define `navigator.webdriver = false` (the
    /// WebDriver spec requires the property; Servo does not implement it yet,
    /// and its absence lets automation-detection libraries flag the browser).
    /// On by default; pages see a normal, non-automated Firefox-family browser.
    pub mimic_real_browser: bool,
    pub tool_timeout: Duration,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            auto_dismiss_dialogs: false,
            cursor_overlay: true,
            mimic_real_browser: true,
            tool_timeout: Duration::from_secs(120),
        }
    }
}

pub struct Tab {
    pub id: TabId,
    webview: RefCell<Option<WebView>>,
    /// Set from our own navigations and Servo's load status.
    pub loading: Cell<bool>,
    /// Incremented whenever Servo reports a load start.
    pub load_starts: Cell<u64>,
    /// Synthetic pointer position, in CSS px of the viewport.
    pub cursor: Cell<Option<Point>>,
    pub input_lock: AsyncLock,
    /// get-clickables id -> Servo node id, valid until the next navigation.
    pub clickables: RefCell<HashMap<u32, String>>,
    pub crashed: RefCell<Option<String>>,
}

impl Tab {
    pub fn webview(&self) -> Result<WebView, String> {
        self.webview.borrow().clone().ok_or_else(|| format!("Tab {} was closed", self.id))
    }

    pub fn is_closed(&self) -> bool {
        self.webview.borrow().is_none()
    }

    pub fn title(&self) -> String {
        self.webview.borrow().as_ref().and_then(WebView::page_title).unwrap_or_default()
    }

    pub fn url(&self) -> String {
        self.webview.borrow().as_ref().and_then(WebView::url).map(String::from).unwrap_or_default()
    }

    /// Marks the tab as loading before we start a navigation, so waiting does
    /// not see the previous page's "complete" state.
    pub fn begin_navigation(&self) -> u64 {
        self.loading.set(true);
        self.clickables.borrow_mut().clear();
        self.load_starts.get()
    }

    /// Waits until the page has loaded.
    ///
    /// With `navigation` (the load-start count before we navigated), a
    /// navigation that never reports a load (same-document) counts as done
    /// after a short while. Without it, a load that starts within a short
    /// grace period (say, right after clicking a link) is waited for too.
    pub async fn wait_for_load(&self, navigation: Option<u64>, limit: Duration) -> bool {
        let begin = Instant::now();
        let before = navigation.unwrap_or_else(|| self.load_starts.get());
        let grace =
            if navigation.is_some() { Duration::from_millis(1500) } else { Duration::from_millis(500) };
        let finished = timeout(
            limit,
            poll_until(|| {
                if self.is_closed() {
                    return Some(());
                }
                let started = self.load_starts.get() > before;
                let waited = begin.elapsed() > grace;
                if !self.loading.get() && (started || waited) {
                    return Some(());
                }
                if navigation.is_some() && !started && waited {
                    self.loading.set(false);
                    return Some(());
                }
                None
            }),
        )
        .await
        .is_ok();
        finished && !self.is_closed()
    }
}

/// A page-initiated control (dialog, popup, picker) waiting for an answer.
pub struct Dialog {
    pub id: u32,
    pub tab_id: TabId,
    pub control: EmbedderControl,
}

pub struct Browser {
    servo: Servo,
    rendering_context: Rc<dyn RenderingContext>,
    hidpi_scale: Cell<Scale<f32, DeviceIndependentPixel, DevicePixel>>,
    pub config: BrowserConfig,
    tabs: RefCell<Vec<Rc<Tab>>>,
    active: Cell<Option<TabId>>,
    next_tab_id: Cell<TabId>,
    needs_paint: Cell<bool>,
    title_changed: Cell<bool>,
    pub dialogs: RefCell<Vec<Dialog>>,
    next_dialog_id: Cell<u32>,
    /// Built-in user content (the webdriver-correctness script), shared by all
    /// tabs. `None` when `mimic_real_browser` is off.
    user_content: RefCell<Option<Rc<UserContentManager>>>,
    myself: Weak<Browser>,
    runtime: Runtime,
    protocol: McpProtocol<Browser>,
    requests: Receiver<McpRequest>,
}

/// Defines `navigator.webdriver` as a read-only `false`, matching real
/// browsers and the WebDriver specification. Servo does not implement the
/// property, and its absence is a signal automation-detection libraries use.
/// Shipped as a built-in user script that runs before page scripts; it is the
/// browser behaving like the one its user agent claims to be, not the MCP
/// layer acting on a page.
const WEBDRIVER_SHIM: &str = r#"(function () {
  try {
    var proto = window.Navigator && window.Navigator.prototype;
    if (!proto) return;
    var d = Object.getOwnPropertyDescriptor(proto, 'webdriver');
    if (d && typeof d.get === 'function') return; // already a getter
    Object.defineProperty(proto, 'webdriver', { get: function () { return false; }, configurable: true });
  } catch (e) {}
})();"#;

impl Browser {
    pub fn new(
        servo: Servo,
        rendering_context: Rc<dyn RenderingContext>,
        hidpi_scale: Scale<f32, DeviceIndependentPixel, DevicePixel>,
        config: BrowserConfig,
        registry: ToolRegistry<Browser>,
        requests: Receiver<McpRequest>,
    ) -> Rc<Self> {
        let protocol = McpProtocol::new(Rc::new(registry), config.tool_timeout);
        Rc::new_cyclic(|myself| Self {
            servo,
            rendering_context,
            hidpi_scale: Cell::new(hidpi_scale),
            config,
            tabs: Default::default(),
            active: Cell::new(None),
            next_tab_id: Cell::new(1),
            needs_paint: Cell::new(true),
            title_changed: Cell::new(true),
            dialogs: Default::default(),
            next_dialog_id: Cell::new(1),
            user_content: RefCell::new(None),
            myself: myself.clone(),
            runtime: Runtime::default(),
            protocol,
            requests,
        })
    }

    pub fn servo(&self) -> &Servo {
        &self.servo
    }

    pub fn rendering_context(&self) -> &Rc<dyn RenderingContext> {
        &self.rendering_context
    }

    fn delegate(&self) -> Rc<dyn WebViewDelegate> {
        Rc::new(Delegate(self.myself.clone()))
    }

    /// The shared built-in user content manager, created lazily (it needs the
    /// live `Servo`). `None` when `mimic_real_browser` is off.
    fn user_content(&self) -> Option<Rc<UserContentManager>> {
        if !self.config.mimic_real_browser {
            return None;
        }
        if self.user_content.borrow().is_none() {
            let manager = Rc::new(UserContentManager::new(&self.servo));
            manager.add_script(Rc::new(UserScript::new(WEBDRIVER_SHIM.to_owned(), None)));
            *self.user_content.borrow_mut() = Some(manager);
        }
        self.user_content.borrow().clone()
    }

    /// One turn of the embedder loop: let Servo run, accept MCP requests and
    /// advance tool calls. Returns when the loop should wake up next.
    pub fn pump(&self) -> Option<Instant> {
        self.servo.spin_event_loop();
        while let Ok(request) = self.requests.try_recv() {
            let Some(browser) = self.myself.upgrade() else { break };
            self.runtime.spawn(async move {
                let reply = browser.protocol.handle_message(browser.clone(), &request.body).await;
                let _ = request.reply.send(reply);
            });
        }
        self.runtime.poll()
    }

    // Tabs ---------------------------------------------------------------

    pub fn tabs(&self) -> Vec<Rc<Tab>> {
        self.tabs.borrow().clone()
    }

    pub fn tab(&self, id: TabId) -> Option<Rc<Tab>> {
        self.tabs.borrow().iter().find(|tab| tab.id == id).cloned()
    }

    pub fn active_tab(&self) -> Option<Rc<Tab>> {
        self.active.get().and_then(|id| self.tab(id))
    }

    pub fn active_tab_id(&self) -> Option<TabId> {
        self.active.get()
    }

    fn tab_for_webview(&self, webview: &WebView) -> Option<Rc<Tab>> {
        self.tabs
            .borrow()
            .iter()
            .find(|tab| tab.webview.borrow().as_ref().is_some_and(|w| w.id() == webview.id()))
            .cloned()
    }

    pub fn create_tab(&self, url: Url, foreground: bool) -> Rc<Tab> {
        let mut builder = WebViewBuilder::new(&self.servo, self.rendering_context.clone())
            .url(url)
            .hidpi_scale_factor(self.hidpi_scale.get())
            .delegate(self.delegate());
        if let Some(manager) = self.user_content() {
            builder = builder.user_content_manager(manager);
        }
        self.add_tab(builder.build(), foreground)
    }

    fn add_tab(&self, webview: WebView, foreground: bool) -> Rc<Tab> {
        let id = self.next_tab_id.get();
        self.next_tab_id.set(id + 1);
        let tab = Rc::new(Tab {
            id,
            webview: RefCell::new(Some(webview.clone())),
            loading: Cell::new(true),
            load_starts: Cell::new(0),
            cursor: Cell::new(None),
            input_lock: AsyncLock::default(),
            clickables: Default::default(),
            crashed: Default::default(),
        });
        self.tabs.borrow_mut().push(tab.clone());
        if foreground || self.active.get().is_none() {
            self.activate(id);
        } else {
            webview.hide();
        }
        tab
    }

    pub fn activate(&self, id: TabId) -> bool {
        if self.tab(id).is_none() {
            return false;
        }
        for tab in self.tabs.borrow().iter() {
            if let Some(webview) = tab.webview.borrow().as_ref() {
                if tab.id == id {
                    webview.show();
                    webview.set_focused(true);
                } else {
                    webview.set_focused(false);
                    webview.hide();
                }
            }
        }
        self.active.set(Some(id));
        self.needs_paint.set(true);
        self.title_changed.set(true);
        true
    }

    pub fn close_tab(&self, id: TabId) -> bool {
        let (tab, index) = {
            let mut tabs = self.tabs.borrow_mut();
            let Some(index) = tabs.iter().position(|tab| tab.id == id) else {
                return false;
            };
            (tabs.remove(index), index)
        };
        self.dialogs.borrow_mut().retain(|dialog| dialog.tab_id != id);
        // Dropping the last reference to the WebView closes it.
        tab.webview.borrow_mut().take();
        if self.active.get() == Some(id) {
            self.active.set(None);
            let next = {
                let tabs = self.tabs.borrow();
                tabs.get(index).or_else(|| tabs.last()).map(|tab| tab.id)
            };
            if let Some(next) = next {
                self.activate(next);
            }
        }
        self.needs_paint.set(true);
        self.title_changed.set(true);
        true
    }

    // Geometry -----------------------------------------------------------

    pub fn hidpi_scale(&self) -> Scale<f32, DeviceIndependentPixel, DevicePixel> {
        self.hidpi_scale.get()
    }

    pub fn set_hidpi_scale(&self, scale: Scale<f32, DeviceIndependentPixel, DevicePixel>) {
        self.hidpi_scale.set(scale);
        for tab in self.tabs.borrow().iter() {
            if let Some(webview) = tab.webview.borrow().as_ref() {
                webview.set_hidpi_scale_factor(scale);
            }
        }
    }

    pub fn resize(&self, size: dpi::PhysicalSize<u32>) {
        self.rendering_context.resize(size);
        for tab in self.tabs.borrow().iter() {
            if let Some(webview) = tab.webview.borrow().as_ref() {
                webview.resize(size);
            }
        }
        self.needs_paint.set(true);
    }

    /// Viewport size of a tab in CSS px.
    pub fn viewport_size(&self, tab: &Tab) -> (f64, f64) {
        let Ok(webview) = tab.webview() else {
            return (0.0, 0.0);
        };
        let size = webview.size();
        let scale = webview.device_pixels_per_css_pixel().get().max(0.01) as f64;
        (size.width as f64 / scale, size.height as f64 / scale)
    }

    // Painting -------------------------------------------------------------

    pub fn wants_paint(&self) -> bool {
        self.needs_paint.get()
    }

    pub fn request_paint(&self) {
        self.needs_paint.set(true);
    }

    /// Paints the active tab into the rendering context if a new frame is
    /// ready. Returns whether anything was painted (the caller presents it).
    pub fn paint_if_needed(&self) -> bool {
        if !self.needs_paint.replace(false) {
            return false;
        }
        if let Some(webview) = self.active_tab().and_then(|tab| tab.webview().ok()) {
            webview.paint();
        }
        true
    }

    /// Where to draw the agent's pointer, in device pixels.
    pub fn cursor_overlay_point(&self) -> Option<DevicePoint> {
        if !self.config.cursor_overlay {
            return None;
        }
        let tab = self.active_tab()?;
        let cursor = tab.cursor.get()?;
        let scale = tab.webview().ok()?.device_pixels_per_css_pixel().get();
        Some(DevicePoint::new(cursor.x as f32 * scale, cursor.y as f32 * scale))
    }

    /// Returns the new window title if it changed.
    pub fn take_window_title(&self) -> Option<String> {
        if !self.title_changed.replace(false) {
            return None;
        }
        let title = self.active_tab().map(|tab| tab.title()).unwrap_or_default();
        Some(if title.is_empty() { "servoaf".into() } else { format!("{title} — servoaf") })
    }

    // Dialogs ----------------------------------------------------------------

    fn add_dialog(&self, tab_id: TabId, control: EmbedderControl) {
        let id = self.next_dialog_id.get();
        self.next_dialog_id.set(id + 1);
        self.dialogs.borrow_mut().push(Dialog { id, tab_id, control });
    }

    /// A dialog that pauses the tab's scripts (alert/confirm/prompt, file
    /// picker), as "type dialog_id".
    pub fn blocking_dialog(&self, tab_id: TabId) -> Option<String> {
        self.dialogs.borrow().iter().find(|dialog| dialog.tab_id == tab_id).and_then(|dialog| {
            let kind = match &dialog.control {
                EmbedderControl::SimpleDialog(SimpleDialog::Alert(_)) => "alert",
                EmbedderControl::SimpleDialog(SimpleDialog::Confirm(_)) => "confirm",
                EmbedderControl::SimpleDialog(SimpleDialog::Prompt(_)) => "prompt",
                EmbedderControl::FilePicker(_) => "file picker",
                _ => return None,
            };
            Some(format!("{kind} dialog_id {}", dialog.id))
        })
    }

    pub fn take_dialog(&self, id: u32) -> Option<Dialog> {
        let mut dialogs = self.dialogs.borrow_mut();
        let index = dialogs.iter().position(|dialog| dialog.id == id)?;
        Some(dialogs.remove(index))
    }

    /// Closes popups (context menus, `<select>` lists) of a tab, like clicking
    /// elsewhere does. Blocking dialogs stay.
    pub fn dismiss_popups(&self, tab_id: TabId) {
        let popups: Vec<Dialog> = {
            let mut dialogs = self.dialogs.borrow_mut();
            let (popups, rest) = std::mem::take(&mut *dialogs).into_iter().partition(|dialog| {
                dialog.tab_id == tab_id
                    && matches!(
                        dialog.control,
                        EmbedderControl::ContextMenu(_) | EmbedderControl::SelectElement(_)
                    )
            });
            *dialogs = rest;
            popups
        };
        // Dropping a control sends Servo its default response (dismiss / keep
        // the current selection).
        drop(popups);
    }
}

struct Delegate(Weak<Browser>);

impl Delegate {
    fn with_tab(&self, webview: &WebView, f: impl FnOnce(&Browser, &Rc<Tab>)) {
        if let Some(browser) = self.0.upgrade()
            && let Some(tab) = browser.tab_for_webview(webview)
        {
            f(&browser, &tab);
        }
    }
}

impl WebViewDelegate for Delegate {
    /// Report a plausible desktop screen and window rectangle. Without this,
    /// `window.screen.width/height`, `window.outerWidth/outerHeight` and
    /// `availWidth/Height` are all 0 — a reliable "headless" tell that
    /// automation-detection libraries key on. The size is derived from the
    /// viewport so it is always at least as large as the content, and rounded
    /// up to a common desktop resolution.
    fn screen_geometry(&self, webview: WebView) -> Option<ScreenGeometry> {
        let view = webview.size();
        let (view_w, view_h) = (view.width.max(1.0) as i32, view.height.max(1.0) as i32);
        // A real screen is at least as big as the window and looks like a
        // common desktop resolution.
        let screen_w = view_w.max(1920);
        let screen_h = view_h.max(1080);
        let size = DeviceIntSize::new(screen_w, screen_h);
        // Leave room for a taskbar so availHeight < height, as on a real desktop.
        let available_size = DeviceIntSize::new(screen_w, (screen_h - 40).max(view_h));
        // The window (with OS decorations) is the viewport plus a title bar and
        // toolbar, placed a little in from the top-left.
        let window_rect = DeviceIntRect::from_origin_and_size(
            servo::DeviceIntPoint::new(0, 0),
            DeviceIntSize::new(view_w, (view_h + 74).min(screen_h)),
        );
        Some(ScreenGeometry { size, available_size, window_rect })
    }

    fn notify_new_frame_ready(&self, webview: WebView) {
        self.with_tab(&webview, |browser, tab| {
            if browser.active.get() == Some(tab.id) {
                browser.needs_paint.set(true);
            }
        });
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        self.with_tab(&webview, |browser, tab| {
            match status {
                LoadStatus::Started => {
                    tab.loading.set(true);
                    tab.load_starts.set(tab.load_starts.get() + 1);
                    tab.clickables.borrow_mut().clear();
                },
                LoadStatus::Complete => tab.loading.set(false),
                _ => {},
            }
            browser.title_changed.set(true);
        });
    }

    fn notify_url_changed(&self, webview: WebView, _url: Url) {
        self.with_tab(&webview, |_, tab| tab.clickables.borrow_mut().clear());
    }

    fn notify_page_title_changed(&self, webview: WebView, _title: Option<String>) {
        self.with_tab(&webview, |browser, _| browser.title_changed.set(true));
    }

    fn notify_closed(&self, webview: WebView) {
        // window.close() from the page.
        self.with_tab(&webview, |browser, tab| {
            browser.close_tab(tab.id);
        });
    }

    fn notify_crashed(&self, webview: WebView, reason: String, _backtrace: Option<String>) {
        log::error!("tab crashed: {reason}");
        self.with_tab(&webview, |_, tab| {
            *tab.crashed.borrow_mut() = Some(reason);
            tab.loading.set(false);
        });
    }

    fn request_create_new(&self, parent: WebView, request: servo::CreateNewWebViewRequest) {
        // window.open() and target=_blank links open a new foreground tab.
        let Some(browser) = self.0.upgrade() else { return };
        let mut builder = request
            .builder(browser.rendering_context.clone())
            .hidpi_scale_factor(browser.hidpi_scale.get())
            .delegate(parent.delegate());
        if let Some(manager) = browser.user_content() {
            builder = builder.user_content_manager(manager);
        }
        browser.add_tab(builder.build(), true);
    }

    fn show_embedder_control(&self, webview: WebView, control: EmbedderControl) {
        let Some(browser) = self.0.upgrade() else { return };
        let Some(tab) = browser.tab_for_webview(&webview) else { return };
        match control {
            // Only relevant for on-screen keyboards.
            EmbedderControl::InputMethod(_) => {},
            EmbedderControl::SimpleDialog(dialog) if browser.config.auto_dismiss_dialogs => match dialog {
                SimpleDialog::Alert(alert) => alert.confirm(),
                SimpleDialog::Confirm(confirm) => confirm.dismiss(),
                SimpleDialog::Prompt(prompt) => prompt.dismiss(),
            },
            control => browser.add_dialog(tab.id, control),
        }
    }

    fn hide_embedder_control(&self, _webview: WebView, control_id: EmbedderControlId) {
        let Some(browser) = self.0.upgrade() else { return };
        let mut dialogs = browser.dialogs.borrow_mut();
        // Keep the control object alive only while the page shows it.
        let hidden: Vec<Dialog> = {
            let (hidden, rest) = std::mem::take(&mut *dialogs)
                .into_iter()
                .partition(|dialog| dialog.control.id() == control_id);
            *dialogs = rest;
            hidden
        };
        drop(dialogs);
        // The page withdrew it; Servo ignores the default answer sent on drop.
        drop(hidden);
    }
}
