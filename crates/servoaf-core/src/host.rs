// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! What servoaf-core needs from the browser it is embedded in.
//!
//! The browser shell (servoshell on desktop, the Android app later) owns the
//! windows, the tabs (one `WebView` each) and the dialogs a person sees. The
//! MCP tools act on that same state through this trait, so whatever the agent
//! does is visible in the UI and vice versa.

use servo::{
    AlertDialog, ColorPicker, ConfirmDialog, ContextMenu, EmbedderControl, EmbedderControlId, FilePicker,
    PromptDialog, SelectElement, SimpleDialog, WebView, WebViewId,
};
use url::Url;

/// Access to an open dialog. Answering it takes the control out of the slot;
/// the shell then forgets the (now empty) dialog.
pub enum ControlSlot<'a> {
    Alert(&'a mut Option<AlertDialog>),
    Confirm(&'a mut Option<ConfirmDialog>),
    Prompt(&'a mut Option<PromptDialog>),
    Select(&'a mut Option<SelectElement>),
    Color(&'a mut Option<ColorPicker>),
    ContextMenu(&'a mut Option<ContextMenu>),
    File(&'a mut Option<FilePicker>),
}

impl ControlSlot<'_> {
    /// Whether the dialog is still waiting for an answer.
    pub fn is_open(&self) -> bool {
        match self {
            ControlSlot::Alert(slot) => slot.is_some(),
            ControlSlot::Confirm(slot) => slot.is_some(),
            ControlSlot::Prompt(slot) => slot.is_some(),
            ControlSlot::Select(slot) => slot.is_some(),
            ControlSlot::Color(slot) => slot.is_some(),
            ControlSlot::ContextMenu(slot) => slot.is_some(),
            ControlSlot::File(slot) => slot.is_some(),
        }
    }

    /// Dialogs that pause the page's scripts until answered.
    pub fn blocks_page(&self) -> bool {
        matches!(
            self,
            ControlSlot::Alert(_) | ControlSlot::Confirm(_) | ControlSlot::Prompt(_) | ControlSlot::File(_)
        )
    }

    pub fn kind(&self) -> &'static str {
        match self {
            ControlSlot::Alert(_) => "alert",
            ControlSlot::Confirm(_) => "confirm",
            ControlSlot::Prompt(_) => "prompt",
            ControlSlot::Select(_) => "select",
            ControlSlot::Color(_) => "color-picker",
            ControlSlot::ContextMenu(_) => "context-menu",
            ControlSlot::File(_) => "file-picker",
        }
    }
}

pub trait Host {
    /// Top-level web views (tabs) in tab order, across windows.
    fn webviews(&self) -> Vec<WebView>;

    /// The tab the person (or agent) is looking at in the focused window.
    fn active_webview(&self) -> Option<WebViewId>;

    /// Opens a tab like the "+" button / address bar would.
    fn create_webview(&self, url: Url, foreground: bool) -> Option<WebView>;

    fn activate_webview(&self, id: WebViewId);

    fn close_webview(&self, id: WebViewId);

    /// Calls `visit` for every open dialog, oldest first per tab, with Servo's
    /// control id when the shell knows it (`None` for alert/confirm/prompt,
    /// of which a tab can only show one at a time). `visit` may answer a
    /// dialog by taking its control out of the slot.
    fn visit_dialogs(&self, visit: &mut dyn FnMut(WebViewId, Option<EmbedderControlId>, ControlSlot<'_>));

    /// The cursor overlay or a tab's state changed; redraw when convenient.
    fn request_repaint(&self) {}
}

/// Dialog storage for shells that have no dialog UI of their own (headless):
/// controls wait here until an agent answers them.
#[derive(Default)]
pub struct DialogStore {
    entries: Vec<(WebViewId, EmbedderControlId, Stored)>,
}

enum Stored {
    Alert(Option<AlertDialog>),
    Confirm(Option<ConfirmDialog>),
    Prompt(Option<PromptDialog>),
    Select(Option<SelectElement>),
    Color(Option<ColorPicker>),
    ContextMenu(Option<ContextMenu>),
    File(Option<FilePicker>),
}

impl DialogStore {
    pub fn add(&mut self, webview: WebViewId, control: EmbedderControl) {
        let id = control.id();
        let stored = match control {
            EmbedderControl::SimpleDialog(SimpleDialog::Alert(dialog)) => Stored::Alert(Some(dialog)),
            EmbedderControl::SimpleDialog(SimpleDialog::Confirm(dialog)) => Stored::Confirm(Some(dialog)),
            EmbedderControl::SimpleDialog(SimpleDialog::Prompt(dialog)) => Stored::Prompt(Some(dialog)),
            EmbedderControl::SelectElement(control) => Stored::Select(Some(control)),
            EmbedderControl::ColorPicker(control) => Stored::Color(Some(control)),
            EmbedderControl::ContextMenu(control) => Stored::ContextMenu(Some(control)),
            EmbedderControl::FilePicker(control) => Stored::File(Some(control)),
            // Only relevant for on-screen keyboards.
            EmbedderControl::InputMethod(_) => return,
        };
        self.entries.push((webview, id, stored));
    }

    /// The page withdrew a control (or the tab closed).
    pub fn remove(&mut self, id: EmbedderControlId) {
        self.entries.retain(|(_, entry_id, _)| *entry_id != id);
    }

    pub fn remove_webview(&mut self, webview: WebViewId) {
        self.entries.retain(|(entry_webview, _, _)| *entry_webview != webview);
    }

    pub fn visit(&mut self, visit: &mut dyn FnMut(WebViewId, Option<EmbedderControlId>, ControlSlot<'_>)) {
        for (webview, id, stored) in self.entries.iter_mut() {
            let slot = match stored {
                Stored::Alert(slot) => ControlSlot::Alert(slot),
                Stored::Confirm(slot) => ControlSlot::Confirm(slot),
                Stored::Prompt(slot) => ControlSlot::Prompt(slot),
                Stored::Select(slot) => ControlSlot::Select(slot),
                Stored::Color(slot) => ControlSlot::Color(slot),
                Stored::ContextMenu(slot) => ControlSlot::ContextMenu(slot),
                Stored::File(slot) => ControlSlot::File(slot),
            };
            visit(*webview, Some(*id), slot);
        }
        // Forget answered dialogs.
        self.entries.retain(|(_, _, stored)| match stored {
            Stored::Alert(slot) => slot.is_some(),
            Stored::Confirm(slot) => slot.is_some(),
            Stored::Prompt(slot) => slot.is_some(),
            Stored::Select(slot) => slot.is_some(),
            Stored::Color(slot) => slot.is_some(),
            Stored::ContextMenu(slot) => slot.is_some(),
            Stored::File(slot) => slot.is_some(),
        });
    }
}
