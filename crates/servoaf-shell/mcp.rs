/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! servoaf: the MCP control server, attached to servoshell.
//!
//! [`McpHost`] exposes servoshell's windows, tabs and dialogs to
//! `servoaf_core`, so the agent and the person act on the same browser: tabs
//! the agent opens appear in the tab strip, dialogs it answers close in the UI.

use std::rc::{Rc, Weak};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use servo::{EmbedderControlId, EventLoopWaker, WebView, WebViewId};
use servoaf_core::host::{ControlSlot, Host};
use servoaf_core::mcp::http::{self, HttpOptions};
use servoaf_core::{Browser, BrowserConfig};
use url::Url;

use crate::running_app_state::RunningAppState;
use crate::window::{ServoShellWindow, TopLevelWebViewCreationRequest};

/// MCP settings from the command line.
#[derive(Clone, Debug)]
pub struct McpPreferences {
    /// `None` disables the server.
    pub http: Option<HttpOptions>,
    pub config: BrowserConfig,
}

impl Default for McpPreferences {
    fn default() -> Self {
        Self {
            http: None,
            config: BrowserConfig::default(),
        }
    }
}

struct McpHost {
    state: Weak<RunningAppState>,
}

impl McpHost {
    fn windows(&self) -> Vec<Rc<ServoShellWindow>> {
        self.state
            .upgrade()
            .map(|state| state.windows().clone())
            .unwrap_or_default()
    }

    fn main_window(&self) -> Option<Rc<ServoShellWindow>> {
        let state = self.state.upgrade()?;
        state
            .focused_window()
            .or_else(|| state.windows().first().cloned())
    }
}

impl Host for McpHost {
    fn webviews(&self) -> Vec<WebView> {
        self.windows()
            .iter()
            .flat_map(|window| window.webviews().into_iter().map(|(_, webview)| webview))
            .collect()
    }

    fn active_webview(&self) -> Option<WebViewId> {
        self.main_window()?
            .active_webview()
            .map(|webview| webview.id())
    }

    fn create_webview(&self, url: Url, foreground: bool) -> Option<WebView> {
        let state = self.state.upgrade()?;
        let window = self.main_window()?;
        let webview =
            window.create_toplevel_webview(state, TopLevelWebViewCreationRequest::WithUrl(url));
        if foreground {
            window.activate_webview(webview.id());
        }
        Some(webview)
    }

    fn activate_webview(&self, id: WebViewId) {
        for window in self.windows() {
            if window.webview_by_id(id).is_some() {
                window.activate_webview(id);
            }
        }
    }

    fn close_webview(&self, id: WebViewId) {
        for window in self.windows() {
            window.close_webview(id);
        }
    }

    fn visit_dialogs(
        &self,
        visit: &mut dyn FnMut(WebViewId, Option<EmbedderControlId>, ControlSlot<'_>),
    ) {
        for window in self.windows() {
            window.platform_window().visit_mcp_dialogs(visit);
            window.set_needs_update();
        }
    }

    fn request_repaint(&self) {
        for window in self.windows() {
            window.set_needs_update();
            window.set_needs_repaint();
        }
    }
}

/// Wakes the event loop when the next tool timer is due.
struct Alarm {
    next: Mutex<Option<Instant>>,
    changed: Condvar,
}

impl Alarm {
    fn start(waker: Box<dyn EventLoopWaker>) -> Arc<Self> {
        let alarm = Arc::new(Alarm {
            next: Mutex::new(None),
            changed: Condvar::new(),
        });
        let thread_alarm = alarm.clone();
        std::thread::Builder::new()
            .name("mcp-alarm".into())
            .spawn(move || {
                let mut next = thread_alarm.next.lock().unwrap();
                loop {
                    match *next {
                        None => next = thread_alarm.changed.wait(next).unwrap(),
                        Some(deadline) => {
                            let now = Instant::now();
                            if now >= deadline {
                                *next = None;
                                waker.wake();
                            } else {
                                next = thread_alarm
                                    .changed
                                    .wait_timeout(next, deadline - now)
                                    .unwrap()
                                    .0;
                            }
                        },
                    }
                }
            })
            .expect("spawn mcp-alarm thread");
        alarm
    }

    fn set(&self, deadline: Option<Instant>) {
        *self.next.lock().unwrap() = deadline;
        self.changed.notify_one();
    }
}

/// The MCP server attached to a running servoshell.
pub(crate) struct Mcp {
    pub(crate) browser: Rc<Browser>,
    alarm: Arc<Alarm>,
}

impl Mcp {
    /// Starts the server if enabled. Call once `state` is in its `Rc`.
    pub(crate) fn start(
        state: &Rc<RunningAppState>,
        preferences: &McpPreferences,
        waker: Box<dyn EventLoopWaker>,
    ) -> Option<Self> {
        let options = preferences.http.clone()?;
        let (sender, receiver) = crossbeam_channel::unbounded();
        let http_waker = waker.clone();
        match http::start(options.clone(), sender, Arc::new(move || http_waker.wake())) {
            Ok(address) => {
                let auth = if options.token.is_some() {
                    " (bearer token required)"
                } else {
                    ""
                };
                eprintln!(
                    "servoaf: MCP server listening on http://{address}{}{auth}",
                    http::MCP_PATH
                );
                if !address.ip().is_loopback() && options.token.is_none() {
                    eprintln!(
                        "servoaf: WARNING: MCP is reachable from the network without a token"
                    );
                }
            },
            Err(error) => {
                eprintln!("servoaf: MCP server disabled: {error}");
                return None;
            },
        }
        let host = Rc::new(McpHost {
            state: Rc::downgrade(state),
        });
        let browser = Browser::new(
            host,
            state.servo().clone(),
            preferences.config.clone(),
            servoaf_core::tools::registry(),
            receiver,
        );
        Some(Self {
            browser,
            alarm: Alarm::start(waker),
        })
    }

    /// Runs MCP work; call after `Servo::spin_event_loop`.
    pub(crate) fn pump(&self) {
        let deadline = self.browser.pump();
        // Never sleep through pending work for long.
        let deadline =
            deadline.map(|deadline| deadline.max(Instant::now() + Duration::from_millis(1)));
        self.alarm.set(deadline);
    }
}
