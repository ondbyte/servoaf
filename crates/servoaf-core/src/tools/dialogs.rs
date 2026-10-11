// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! get-dialogs and respond-dialog.
//!
//! Servo hands page dialogs (alert/confirm/prompt), `<select>` popups, context
//! menus, file and color pickers to the browser shell, which shows them to the
//! person. The agent sees and answers the same dialogs. Blocking dialogs pause
//! the page's scripts until answered.

use std::path::PathBuf;
use std::rc::Rc;

use serde_json::{Value, json};
use servo::{ContextMenuItem, RgbColor, SelectElementOptionOrOptgroup};

use super::{TAB_ID_ARG, resolve_tab};
use crate::browser::Browser;
use crate::host::ControlSlot;
use crate::mcp::registry::{Args, SchemaBuilder, ToolRegistry, ToolResult, int_arg, string_arg};

fn describe(id: u32, tab_id: u32, slot: &ControlSlot<'_>) -> Value {
    let mut info = json!({"dialog_id": id, "tab_id": tab_id, "type": slot.kind()});
    if slot.blocks_page() {
        info["blocking"] = true.into();
    }
    match slot {
        ControlSlot::Alert(Some(dialog)) => info["message"] = dialog.message().into(),
        ControlSlot::Confirm(Some(dialog)) => info["message"] = dialog.message().into(),
        ControlSlot::Prompt(Some(dialog)) => {
            info["message"] = dialog.message().into();
            info["default_value"] = dialog.current_value().into();
        },
        ControlSlot::Select(Some(select)) => {
            let mut options = Vec::new();
            for entry in select.options() {
                match entry {
                    SelectElementOptionOrOptgroup::Option(option) => options.push(
                        json!({"option_id": option.id, "label": option.label, "disabled": option.is_disabled}),
                    ),
                    SelectElementOptionOrOptgroup::Optgroup { label, options: group } => {
                        for option in group {
                            options.push(json!({
                                "option_id": option.id, "label": option.label,
                                "disabled": option.is_disabled, "group": label,
                            }));
                        }
                    },
                }
            }
            info["options"] = options.into();
            info["selected"] = select.selected_options().into();
            info["multiple"] = select.allow_select_multiple().into();
        },
        ControlSlot::ContextMenu(Some(menu)) => {
            let items: Vec<Value> = menu
                .items()
                .iter()
                .filter_map(|item| match item {
                    ContextMenuItem::Item { label, enabled, .. } => {
                        Some(json!({"label": label, "enabled": enabled}))
                    },
                    ContextMenuItem::Separator => None,
                })
                .collect();
            info["items"] = items.into();
        },
        ControlSlot::File(Some(picker)) => {
            info["multiple"] = picker.allow_select_multiple().into();
            let patterns: Vec<&str> = picker.filter_patterns().iter().map(|p| p.0.as_str()).collect();
            info["accept"] = patterns.into();
        },
        ControlSlot::Color(Some(picker)) => {
            if let Some(color) = picker.current_color() {
                info["color"] = format!("#{:02x}{:02x}{:02x}", color.red, color.green, color.blue).into();
            }
        },
        _ => {},
    }
    info
}

async fn get_dialogs(browser: Rc<Browser>, args: Args) -> ToolResult {
    let tab_filter = int_arg(&args, TAB_ID_ARG);
    let mut dialogs = Vec::new();
    browser.visit_dialogs(|id, tab_id, slot| {
        if tab_filter.is_none_or(|tab| tab == tab_id as i64) {
            dialogs.push(describe(id, tab_id, &slot));
        }
    });
    ToolResult::json(json!({"count": dialogs.len(), "dialogs": dialogs}))
}

fn parse_color(text: &str) -> Option<RgbColor> {
    let hex = text.trim().strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(RgbColor { red: channel(0)?, green: channel(2)?, blue: channel(4)? })
}

/// Answers the dialog in `slot` (taking its control) or explains why not,
/// leaving it open.
fn respond(slot: ControlSlot<'_>, args: &Args) -> Result<String, String> {
    let action = string_arg(args, "action").unwrap_or("accept");
    let accept = match action {
        "accept" => true,
        "dismiss" => false,
        other => return Err(format!("Unknown action: {other} (use accept or dismiss)")),
    };
    let gone = || "The dialog was already answered".to_owned();
    match slot {
        ControlSlot::Alert(slot) => slot.take().ok_or_else(gone)?.confirm(),
        ControlSlot::Confirm(slot) => {
            let confirm = slot.take().ok_or_else(gone)?;
            if accept { confirm.confirm() } else { confirm.dismiss() }
        },
        ControlSlot::Prompt(slot) => {
            let mut prompt = slot.take().ok_or_else(gone)?;
            if accept {
                if let Some(text) = string_arg(args, "text") {
                    prompt.set_current_value(text);
                }
                prompt.confirm();
            } else {
                prompt.dismiss();
            }
        },
        ControlSlot::Select(slot) => {
            let mut select = slot.take().ok_or_else(gone)?;
            if accept && let Some(ids) = args.get("option_ids").and_then(Value::as_array) {
                select.select(ids.iter().filter_map(Value::as_u64).map(|id| id as usize).collect());
            }
            // Dismissing keeps the current selection.
            select.submit();
        },
        ControlSlot::ContextMenu(slot) => {
            let menu = slot.as_ref().ok_or_else(gone)?;
            let wanted = string_arg(args, "item").filter(|_| accept);
            let chosen = wanted.and_then(|wanted| {
                menu.items().iter().find_map(|item| match item {
                    ContextMenuItem::Item { label, action, enabled: true }
                        if label.eq_ignore_ascii_case(wanted) =>
                    {
                        Some(*action)
                    },
                    _ => None,
                })
            });
            match (chosen, wanted) {
                (Some(action), _) => slot.take().ok_or_else(gone)?.select(action),
                (None, Some(wanted)) => return Err(format!("No enabled menu item labelled {wanted:?}")),
                (None, None) => slot.take().ok_or_else(gone)?.dismiss(),
            }
        },
        ControlSlot::File(slot) => {
            let mut picker = slot.take().ok_or_else(gone)?;
            let paths: Vec<PathBuf> = args
                .get("paths")
                .and_then(Value::as_array)
                .map(|paths| paths.iter().filter_map(Value::as_str).map(PathBuf::from).collect())
                .unwrap_or_default();
            if accept && !paths.is_empty() {
                picker.select(&paths);
                picker.submit();
            } else {
                picker.dismiss();
            }
        },
        ControlSlot::Color(slot) => {
            let mut picker = slot.take().ok_or_else(gone)?;
            if accept && let Some(color) = string_arg(args, "text").and_then(parse_color) {
                picker.select(Some(color));
            }
            picker.submit();
        },
    }
    Ok(format!("{action}ed"))
}

async fn respond_dialog(browser: Rc<Browser>, args: Args) -> ToolResult {
    let wanted_id = int_arg(&args, "dialog_id").map(|id| id as u32);
    let wanted_tab = if wanted_id.is_none() {
        match resolve_tab(&browser, &args) {
            Ok(tab) => Some(tab.id),
            Err(error) => return ToolResult::error(error),
        }
    } else {
        None
    };
    let mut outcome: Option<(u32, u32, Result<String, String>)> = None;
    browser.visit_dialogs(|id, tab_id, slot| {
        let matches = match wanted_id {
            Some(wanted) => id == wanted,
            // The oldest dialog of the tab.
            None => Some(tab_id) == wanted_tab,
        };
        if outcome.is_none() && matches {
            outcome = Some((id, tab_id, respond(slot, &args)));
        }
    });
    browser.request_paint();
    match outcome {
        Some((id, tab_id, Ok(message))) => ToolResult::json(
            json!({"dialog_id": id, "tab_id": tab_id, "result": format!("{message} dialog {id}")}),
        ),
        Some((_, _, Err(message))) => ToolResult::error(message),
        None => match (wanted_id, wanted_tab) {
            (Some(id), _) => ToolResult::error(format!("No dialog with id {id}; see get-dialogs")),
            (None, Some(tab)) => ToolResult::error(format!("Tab {tab} has no open dialog")),
            _ => ToolResult::error("No dialog to answer"),
        },
    }
}

pub fn register(registry: &mut ToolRegistry<Browser>) {
    registry.register(
        "get-dialogs",
        "Lists dialogs and popups the page opened and that wait for an answer: alert, confirm, \
         prompt (these block the page's scripts), <select> option lists, context menus, file and \
         color pickers.",
        SchemaBuilder::new().integer(TAB_ID_ARG, "Only dialogs of this tab.").build(),
        get_dialogs,
    );
    registry.register(
        "respond-dialog",
        "Answers a dialog from get-dialogs. accept: OK an alert/confirm/prompt (with `text` for a \
         prompt), choose `option_ids` in a <select>, pick the context-menu `item` by label, give \
         `paths` to a file picker, or a `text` color like #ff0000 to a color picker. dismiss: Cancel \
         / close it.",
        SchemaBuilder::new()
            .integer("dialog_id", "Dialog to answer. Defaults to the oldest dialog of the tab.")
            .integer(TAB_ID_ARG, "Tab whose oldest dialog to answer when dialog_id is not given.")
            .string_enum("action", "accept (default) or dismiss.", &["accept", "dismiss"])
            .string("text", "Prompt answer, or color (#rrggbb) for a color picker.")
            .integer_array("option_ids", "Option ids to select in a <select> popup.")
            .string("item", "Context menu item label to activate.")
            .add_string_array("paths", "Files to choose in a file picker.")
            .build(),
        respond_dialog,
    );
}
