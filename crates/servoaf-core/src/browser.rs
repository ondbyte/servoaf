// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! The MCP side of the browser: per-tab automation state on top of the tabs
//! the shell (see [`Host`]) owns, page dialogs, and the bridge from MCP
//! requests to tool futures on the main thread.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use servo::{EmbedderControl, EmbedderControlId, LoadStatus, Servo, SimpleDialog, WebView, WebViewId};
use url::Url;

use crate::host::{ControlSlot, Host};
use crate::input::human_path::Point;
use crate::mcp::http::McpRequest;
use crate::mcp::protocol::McpProtocol;
use crate::mcp::registry::ToolRegistry;
use crate::runtime::{AsyncLock, Runtime, poll_until, timeout};

pub type TabId = u32;
/// get-tabs reports the tabs of all windows under one window id, so results
/// have the same shape as chromeaf's.
pub const WINDOW_ID: u32 = 1;

#[derive(Clone, Debug)]
pub struct BrowserConfig {
    /// Accept alerts and dismiss confirm/prompt dialogs immediately instead of
    /// leaving them for the person or respond-dialog.
    pub auto_dismiss_dialogs: bool,
    /// Draw the agent's pointer on top of the page.
    pub cursor_overlay: bool,
    /// Ship a small built-in user script defining `navigator.webdriver =
    /// false`, the value real browsers report (Servo leaves it unset, which
    /// automation-detection libraries flag).
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

/// Defines `navigator.webdriver` as a read-only `false`, matching real
/// browsers and the WebDriver specification. Install it as a user script in
/// the shell's `UserContentManager` when `mimic_real_browser` is set.
pub const WEBDRIVER_SHIM: &str = r#"(function () {
  try {
    var proto = window.Navigator && window.Navigator.prototype;
    if (!proto) return;
    var d = Object.getOwnPropertyDescriptor(proto, 'webdriver');
    if (d && typeof d.get === 'function') return; // already a getter
    Object.defineProperty(proto, 'webdriver', { get: function () { return false; }, configurable: true });
  } catch (e) {}
})();"#;

/// Automation state for one tab. The `WebView` itself belongs to the shell;
/// holding it here would keep a tab the person closed alive.
pub struct Tab {
    pub id: TabId,
    pub webview_id: WebViewId,
    host: Rc<dyn Host>,
    closed: Cell<bool>,
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
        if self.closed.get() {
            return Err(format!("Tab {} was closed", self.id));
        }
        self.host
            .webviews()
            .into_iter()
            .find(|webview| webview.id() == self.webview_id)
            .ok_or_else(|| format!("Tab {} was closed", self.id))
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }

    pub fn title(&self) -> String {
        self.webview().ok().and_then(|webview| webview.page_title()).unwrap_or_default()
    }

    pub fn url(&self) -> String {
        self.webview().ok().and_then(|webview| webview.url()).map(String::from).unwrap_or_default()
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

type DialogKey = (WebViewId, Option<EmbedderControlId>);

pub struct Browser {
    host: Rc<dyn Host>,
    servo: Servo,
    pub config: BrowserConfig,
    tabs: RefCell<Vec<Rc<Tab>>>,
    next_tab_id: Cell<TabId>,
    /// Stable MCP ids for dialogs (Servo's control ids aren't plain numbers).
    dialog_ids: RefCell<Vec<(DialogKey, u32)>>,
    next_dialog_id: Cell<u32>,
    pending_replies: crate::dom::PendingReplies,
    myself: Weak<Browser>,
    runtime: Runtime,
    protocol: McpProtocol<Browser>,
    requests: Receiver<McpRequest>,
}

impl Browser {
    pub fn new(
        host: Rc<dyn Host>,
        servo: Servo,
        config: BrowserConfig,
        registry: ToolRegistry<Browser>,
        requests: Receiver<McpRequest>,
    ) -> Rc<Self> {
        let protocol = McpProtocol::new(Rc::new(registry), config.tool_timeout);
        Rc::new_cyclic(|myself| Self {
            host,
            servo,
            config,
            tabs: Default::default(),
            next_tab_id: Cell::new(1),
            dialog_ids: Default::default(),
            next_dialog_id: Cell::new(1),
            pending_replies: Default::default(),
            myself: myself.clone(),
            runtime: Runtime::default(),
            protocol,
            requests,
        })
    }

    pub fn servo(&self) -> &Servo {
        &self.servo
    }

    pub fn pending_replies(&self) -> &crate::dom::PendingReplies {
        &self.pending_replies
    }

    /// Accepts MCP requests and advances tool calls. Call from the shell's
    /// event loop after `Servo::spin_event_loop`. Returns when the loop should
    /// wake up next (`None`: only when woken).
    pub fn pump(&self) -> Option<Instant> {
        while let Ok(request) = self.requests.try_recv() {
            let Some(browser) = self.myself.upgrade() else { break };
            self.runtime.spawn(async move {
                let reply = browser.protocol.handle_message(browser.clone(), &request.body).await;
                let _ = request.reply.send(reply);
            });
        }
        let deadline = self.runtime.poll();
        self.pending_replies.poll();
        if self.pending_replies.is_empty() {
            return deadline;
        }
        // Keep checking parked replies until the script thread has answered.
        let soon = Instant::now() + Duration::from_millis(50);
        Some(deadline.map_or(soon, |deadline| deadline.min(soon)))
    }

    // Tabs ---------------------------------------------------------------

    /// Brings the tab list in line with the shell's web views: new tabs (from
    /// the UI, window.open, ...) get ids, closed ones are dropped.
    fn sync_tabs(&self) -> Vec<Rc<Tab>> {
        let webviews = self.host.webviews();
        let mut tabs = self.tabs.borrow_mut();
        tabs.retain(|tab| {
            let open = webviews.iter().any(|webview| webview.id() == tab.webview_id);
            if !open {
                tab.closed.set(true);
            }
            open
        });
        let mut ordered = Vec::with_capacity(webviews.len());
        for webview in &webviews {
            let tab = match tabs.iter().find(|tab| tab.webview_id == webview.id()) {
                Some(tab) => tab.clone(),
                None => {
                    let id = self.next_tab_id.get();
                    self.next_tab_id.set(id + 1);
                    Rc::new(Tab {
                        id,
                        webview_id: webview.id(),
                        host: self.host.clone(),
                        closed: Cell::new(false),
                        loading: Cell::new(webview.load_status() != LoadStatus::Complete),
                        load_starts: Cell::new(0),
                        cursor: Cell::new(None),
                        input_lock: AsyncLock::default(),
                        clickables: Default::default(),
                        crashed: Default::default(),
                    })
                },
            };
            ordered.push(tab);
        }
        *tabs = ordered.clone();
        ordered
    }

    pub fn tabs(&self) -> Vec<Rc<Tab>> {
        self.sync_tabs()
    }

    pub fn tab(&self, id: TabId) -> Option<Rc<Tab>> {
        self.sync_tabs().into_iter().find(|tab| tab.id == id)
    }

    fn tab_for_webview(&self, webview_id: WebViewId) -> Option<Rc<Tab>> {
        self.sync_tabs().into_iter().find(|tab| tab.webview_id == webview_id)
    }

    pub fn active_tab(&self) -> Option<Rc<Tab>> {
        let active = self.host.active_webview()?;
        self.tab_for_webview(active)
    }

    pub fn active_tab_id(&self) -> Option<TabId> {
        self.active_tab().map(|tab| tab.id)
    }

    pub fn create_tab(&self, url: Url, foreground: bool) -> Option<Rc<Tab>> {
        let webview = self.host.create_webview(url, foreground)?;
        let tab = self.tab_for_webview(webview.id())?;
        tab.loading.set(true);
        Some(tab)
    }

    pub fn activate(&self, id: TabId) -> bool {
        let Some(tab) = self.tab(id) else { return false };
        self.host.activate_webview(tab.webview_id);
        true
    }

    pub fn close_tab(&self, id: TabId) -> bool {
        let Some(tab) = self.tab(id) else { return false };
        self.host.close_webview(tab.webview_id);
        tab.closed.set(true);
        self.sync_tabs();
        true
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

    pub fn request_paint(&self) {
        self.host.request_repaint();
    }

    /// The agent's pointer in the active tab: (tab's web view, CSS px).
    pub fn cursor_overlay(&self) -> Option<(WebViewId, Point)> {
        if !self.config.cursor_overlay {
            return None;
        }
        let tab = self.active_tab()?;
        Some((tab.webview_id, tab.cursor.get()?))
    }

    // Events the shell forwards from its WebViewDelegate ---------------------

    pub fn notify_load_status_changed(&self, webview: WebViewId, status: LoadStatus) {
        let Some(tab) = self.tab_for_webview(webview) else { return };
        match status {
            LoadStatus::Started => {
                tab.loading.set(true);
                tab.load_starts.set(tab.load_starts.get() + 1);
                tab.clickables.borrow_mut().clear();
            },
            LoadStatus::Complete => tab.loading.set(false),
            _ => {},
        }
    }

    pub fn notify_url_changed(&self, webview: WebViewId) {
        if let Some(tab) = self.tab_for_webview(webview) {
            tab.clickables.borrow_mut().clear();
        }
    }

    pub fn notify_crashed(&self, webview: WebViewId, reason: String) {
        log::error!("tab crashed: {reason}");
        if let Some(tab) = self.tab_for_webview(webview) {
            *tab.crashed.borrow_mut() = Some(reason);
            tab.loading.set(false);
        }
    }

    /// Lets automation handle a control before the shell shows it. Returns it
    /// back unless `auto_dismiss_dialogs` answered it.
    pub fn intercept_control(&self, control: EmbedderControl) -> Option<EmbedderControl> {
        match control {
            EmbedderControl::SimpleDialog(dialog) if self.config.auto_dismiss_dialogs => {
                match dialog {
                    SimpleDialog::Alert(alert) => alert.confirm(),
                    SimpleDialog::Confirm(confirm) => confirm.dismiss(),
                    SimpleDialog::Prompt(prompt) => prompt.dismiss(),
                }
                None
            },
            control => Some(control),
        }
    }

    // Dialogs ----------------------------------------------------------------

    fn dialog_id(&self, control: DialogKey) -> u32 {
        let mut ids = self.dialog_ids.borrow_mut();
        if let Some((_, id)) = ids.iter().find(|(known, _)| *known == control) {
            return *id;
        }
        let id = self.next_dialog_id.get();
        self.next_dialog_id.set(id + 1);
        ids.push((control, id));
        id
    }

    /// Calls `visit(dialog_id, tab_id, slot)` for every open dialog.
    pub fn visit_dialogs(&self, mut visit: impl FnMut(u32, TabId, ControlSlot<'_>)) {
        let tabs = self.sync_tabs();
        let mut open = Vec::new();
        self.host.visit_dialogs(&mut |webview, control, slot| {
            if !slot.is_open() {
                return;
            }
            let Some(tab) = tabs.iter().find(|tab| tab.webview_id == webview) else { return };
            let key = (webview, control);
            let id = self.dialog_id(key);
            open.push(key);
            visit(id, tab.id, slot);
        });
        // Forget ids of dialogs that are gone.
        self.dialog_ids.borrow_mut().retain(|(control, _)| open.contains(control));
    }

    /// A dialog that pauses the tab's scripts (alert/confirm/prompt, file
    /// picker), as "type dialog_id N".
    pub fn blocking_dialog(&self, tab_id: TabId) -> Option<String> {
        let mut found = None;
        self.visit_dialogs(|id, tab, slot| {
            if found.is_none() && tab == tab_id && slot.blocks_page() {
                found = Some(format!("{} dialog_id {id}", slot.kind()));
            }
        });
        found
    }

    /// Closes popups (context menus, `<select>` lists) of a tab, like clicking
    /// elsewhere does. Blocking dialogs stay.
    pub fn dismiss_popups(&self, tab_id: TabId) {
        self.visit_dialogs(|_, tab, slot| {
            if tab != tab_id {
                return;
            }
            // Dropping a control sends Servo its default answer (dismiss /
            // keep the current selection).
            match slot {
                ControlSlot::ContextMenu(menu) => drop(menu.take()),
                ControlSlot::Select(select) => drop(select.take()),
                _ => {},
            }
        });
        self.host.request_repaint();
    }
}
