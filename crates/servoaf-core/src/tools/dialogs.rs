// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! get-dialogs and respond-dialog.
//!
//! Servo hands page dialogs (alert/confirm/prompt), `<select>` popups, context
//! menus, file and color pickers to the embedder instead of drawing them. They
//! are kept here until the agent answers them. Blocking dialogs pause the
//! page's script until answered.

use std::path::PathBuf;
use std::rc::Rc;

use serde_json::{Value, json};
use servo::{ContextMenuItem, EmbedderControl, RgbColor, SelectElementOptionOrOptgroup, SimpleDialog};

use super::{TAB_ID_ARG, resolve_tab};
use crate::browser::{Browser, Dialog};
use crate::mcp::registry::{Args, SchemaBuilder, ToolRegistry, ToolResult, int_arg, string_arg};

fn describe(dialog: &Dialog) -> Value {
    let mut info = json!({"dialog_id": dialog.id, "tab_id": dialog.tab_id});
    match &dialog.control {
        EmbedderControl::SimpleDialog(simple) => {
            let kind = match simple {
                SimpleDialog::Alert(_) => "alert",
                SimpleDialog::Confirm(_) => "confirm",
                SimpleDialog::Prompt(_) => "prompt",
            };
            info["type"] = kind.into();
            info["message"] = simple.message().into();
            info["blocking"] = true.into();
            if let SimpleDialog::Prompt(prompt) = simple {
                info["default_value"] = prompt.current_value().into();
            }
        },
        EmbedderControl::SelectElement(select) => {
            info["type"] = "select".into();
            let mut options = Vec::new();
            for entry in select.options() {
                match entry {
                    SelectElementOptionOrOptgroup::Option(option) => options
                        .push(json!({"option_id": option.id, "label": option.label, "disabled": option.is_disabled})),
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
        EmbedderControl::ContextMenu(menu) => {
            info["type"] = "context-menu".into();
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
        EmbedderControl::FilePicker(picker) => {
            info["type"] = "file-picker".into();
            info["blocking"] = true.into();
            info["multiple"] = picker.allow_select_multiple().into();
            let patterns: Vec<&str> = picker.filter_patterns().iter().map(|p| p.0.as_str()).collect();
            info["accept"] = patterns.into();
        },
        EmbedderControl::ColorPicker(picker) => {
            info["type"] = "color-picker".into();
            if let Some(color) = picker.current_color() {
                info["color"] = format!("#{:02x}{:02x}{:02x}", color.red, color.green, color.blue).into();
            }
        },
        EmbedderControl::InputMethod(_) => info["type"] = "input-method".into(),
    }
    info
}

async fn get_dialogs(browser: Rc<Browser>, args: Args) -> ToolResult {
    let tab_filter = int_arg(&args, TAB_ID_ARG);
    let dialogs: Vec<Value> = browser
        .dialogs
        .borrow()
        .iter()
        .filter(|dialog| tab_filter.is_none_or(|tab| tab == dialog.tab_id as i64))
        .map(describe)
        .collect();
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

fn respond(dialog: Dialog, args: &Args) -> Result<String, Box<(Dialog, String)>> {
    let action = string_arg(args, "action").unwrap_or("accept");
    let accept = match action {
        "accept" => true,
        "dismiss" => false,
        other => return Err(Box::new((dialog, format!("Unknown action: {other} (use accept or dismiss)")))),
    };
    let Dialog { id, tab_id, control } = dialog;
    match control {
        EmbedderControl::SimpleDialog(SimpleDialog::Alert(alert)) => alert.confirm(),
        EmbedderControl::SimpleDialog(SimpleDialog::Confirm(confirm)) => {
            if accept {
                confirm.confirm()
            } else {
                confirm.dismiss()
            }
        },
        EmbedderControl::SimpleDialog(SimpleDialog::Prompt(mut prompt)) => {
            if accept {
                if let Some(text) = string_arg(args, "text") {
                    prompt.set_current_value(text);
                }
                prompt.confirm();
            } else {
                prompt.dismiss();
            }
        },
        EmbedderControl::SelectElement(mut select) => {
            if accept && let Some(ids) = args.get("option_ids").and_then(Value::as_array) {
                let ids: Vec<usize> = ids.iter().filter_map(Value::as_u64).map(|id| id as usize).collect();
                select.select(ids);
            }
            // Dismissing keeps the current selection.
            select.submit();
        },
        EmbedderControl::ContextMenu(menu) => {
            let wanted = string_arg(args, "item");
            let action = wanted.filter(|_| accept).and_then(|wanted| {
                menu.items().iter().find_map(|item| match item {
                    ContextMenuItem::Item { label, action, enabled: true }
                        if label.eq_ignore_ascii_case(wanted) =>
                    {
                        Some(*action)
                    },
                    _ => None,
                })
            });
            match (action, wanted.filter(|_| accept)) {
                (Some(action), _) => menu.select(action),
                (None, Some(wanted)) => {
                    let message = format!("No enabled menu item labelled {wanted:?}");
                    return Err(Box::new((
                        Dialog { id, tab_id, control: EmbedderControl::ContextMenu(menu) },
                        message,
                    )));
                },
                (None, None) => menu.dismiss(),
            }
        },
        EmbedderControl::FilePicker(mut picker) => {
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
        EmbedderControl::ColorPicker(mut picker) => {
            if accept && let Some(color) = string_arg(args, "text").and_then(parse_color) {
                picker.select(Some(color));
            }
            picker.submit();
        },
        EmbedderControl::InputMethod(_) => {},
    }
    Ok(format!("{action}ed dialog {id}"))
}

async fn respond_dialog(browser: Rc<Browser>, args: Args) -> ToolResult {
    let id = match int_arg(&args, "dialog_id") {
        Some(id) => id as u32,
        None => {
            // The oldest dialog of the tab.
            let tab = match resolve_tab(&browser, &args) {
                Ok(tab) => tab,
                Err(error) => return ToolResult::error(error),
            };
            let found = browser.dialogs.borrow().iter().find(|d| d.tab_id == tab.id).map(|d| d.id);
            match found {
                Some(id) => id,
                None => return ToolResult::error(format!("Tab {} has no open dialog", tab.id)),
            }
        },
    };
    let Some(dialog) = browser.take_dialog(id) else {
        return ToolResult::error(format!("No dialog with id {id}; see get-dialogs"));
    };
    let tab_id = dialog.tab_id;
    match respond(dialog, &args) {
        Ok(message) => ToolResult::json(json!({"dialog_id": id, "tab_id": tab_id, "result": message})),
        Err(error) => {
            let (dialog, message) = *error;
            browser.dialogs.borrow_mut().insert(0, dialog);
            ToolResult::error(message)
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
