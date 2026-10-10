// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! move-cursor, click, rightclick, double-click, scroll, type-text, press-key,
//! get-cursor-position.

use std::rc::Rc;
use std::time::Duration;

use serde_json::json;

use super::page::{Bounds, clickable_bounds};
use super::{TAB_ID_ARG, TAB_ID_DESCRIPTION, rect_json, resolve_visible_tab};
use crate::browser::{Browser, Tab};
use crate::input::human_path::{PathOptions, Point};
use crate::input::{self, Button};
use crate::mcp::registry::{Args, SchemaBuilder, ToolRegistry, ToolResult, int_arg, number_arg, string_arg};

const CLICKABLE_ID_ARG: &str = "clickable_id";

/// Where to point, from explicit coordinates or a clickable.
struct Target {
    point: Point,
    /// For Fitts's law.
    width: f64,
    element: Option<Bounds>,
}

/// People don't hit the exact center; aim somewhere in the middle area.
fn human_aim_point(bounds: &Bounds) -> Point {
    let spread = |size: f64| {
        let amount = (size / 2.0 * 0.35).min(12.0);
        (rand::random::<f64>() + rand::random::<f64>() - 1.0) * amount
    };
    Point::new(
        bounds.x + bounds.width / 2.0 + spread(bounds.width),
        bounds.y + bounds.height / 2.0 + spread(bounds.height),
    )
}

async fn resolve_target(browser: &Browser, tab: &Tab, args: &Args) -> Result<Target, String> {
    let (width, height) = browser.viewport_size(tab);
    if let Some(id) = int_arg(args, CLICKABLE_ID_ARG) {
        let id = u32::try_from(id).map_err(|_| format!("Invalid clickable_id {id}"))?;
        let bounds = clickable_bounds(browser, tab, id).await?;
        let visible = bounds.visible_part(width, height).ok_or_else(|| {
            format!(
                "Clickable {id} is outside the viewport (top={:.0}, viewport height={height:.0}). Use scroll first.",
                bounds.y
            )
        })?;
        return Ok(Target {
            point: human_aim_point(&visible),
            width: visible.width.min(visible.height).max(1.0),
            element: Some(bounds),
        });
    }
    let (Some(x), Some(y)) = (number_arg(args, "x"), number_arg(args, "y")) else {
        return Err("Provide either clickable_id or both x and y".into());
    };
    if x < 0.0 || y < 0.0 || x >= width || y >= height {
        return Err(format!(
            "Point ({x}, {y}) is outside the viewport ({width:.0}x{height:.0}). Scroll it into view first."
        ));
    }
    Ok(Target { point: Point::new(x, y), width: 24.0, element: None })
}

fn path_options(args: &Args, target: &Target) -> PathOptions {
    PathOptions {
        target_width: target.width,
        duration: int_arg(args, "duration_ms").map(|ms| Duration::from_millis(ms.clamp(0, 10_000) as u64)),
        ..Default::default()
    }
}

fn done(browser: &Browser, tab: &Tab, action: &str, element: Option<Bounds>) -> ToolResult {
    let mut result = json!({"action": action});
    if let Some(cursor) = tab.cursor.get() {
        result["cursor"] = json!({"x": cursor.x, "y": cursor.y});
    }
    if let Some(bounds) = element {
        result["element_bounds"] = rect_json(bounds.x, bounds.y, bounds.width, bounds.height);
    }
    let _ = browser;
    ToolResult::json(result)
}

#[derive(Clone, Copy)]
enum Pointer {
    Move,
    Click,
    RightClick,
    DoubleClick,
}

async fn pointer_tool(browser: Rc<Browser>, args: Args, action: Pointer) -> ToolResult {
    let result: Result<ToolResult, String> = async {
        let tab = resolve_visible_tab(&browser, &args)?;
        let target = resolve_target(&browser, &tab, &args).await?;
        let options = path_options(&args, &target);
        let name = match action {
            Pointer::Move => {
                input::move_to(&browser, &tab, target.point, options).await?;
                "move-cursor"
            },
            Pointer::Click => {
                let button = match string_arg(&args, "button") {
                    Some("right") => Button::Right,
                    Some("middle") => Button::Middle,
                    _ => Button::Left,
                };
                input::click(&browser, &tab, target.point, button, 1, options).await?;
                "click"
            },
            Pointer::RightClick => {
                input::click(&browser, &tab, target.point, Button::Right, 1, options).await?;
                "rightclick"
            },
            Pointer::DoubleClick => {
                input::click(&browser, &tab, target.point, Button::Left, 2, options).await?;
                "double-click"
            },
        };
        Ok(done(&browser, &tab, name, target.element))
    }
    .await;
    result.into()
}

async fn scroll(browser: Rc<Browser>, args: Args) -> ToolResult {
    let result: Result<ToolResult, String> = async {
        let tab = resolve_visible_tab(&browser, &args)?;
        let dx = number_arg(&args, "delta_x").unwrap_or(0.0).clamp(-20_000.0, 20_000.0);
        let dy = number_arg(&args, "delta_y").unwrap_or(0.0).clamp(-20_000.0, 20_000.0);
        if dx == 0.0 && dy == 0.0 {
            return Err("Provide delta_x and/or delta_y (pixels)".into());
        }
        // Scroll where the pointer is unless asked otherwise; keep it inside
        // the viewport so the wheel lands on the page.
        let (width, height) = browser.viewport_size(&tab);
        let cursor = input::cursor_position(&browser, &tab);
        let at = Point::new(
            number_arg(&args, "x").unwrap_or(cursor.x).clamp(1.0, (width - 2.0).max(1.0)),
            number_arg(&args, "y").unwrap_or(cursor.y).clamp(1.0, (height - 2.0).max(1.0)),
        );
        input::scroll(&browser, &tab, at, dx, dy).await?;
        Ok(done(&browser, &tab, "scroll", None))
    }
    .await;
    result.into()
}

async fn type_text(browser: Rc<Browser>, args: Args) -> ToolResult {
    let result: Result<ToolResult, String> = async {
        let tab = resolve_visible_tab(&browser, &args)?;
        let text = string_arg(&args, "text").ok_or("text must be a string")?;
        let min_ms = int_arg(&args, "min_delay_ms").unwrap_or(45).clamp(0, 2000);
        let max_ms = int_arg(&args, "max_delay_ms").unwrap_or(160).clamp(min_ms, 4000);
        tab.webview()?.set_focused(true);
        input::type_text(
            &tab,
            text,
            Duration::from_millis(min_ms as u64),
            Duration::from_millis(max_ms as u64),
        )
        .await?;
        Ok(done(&browser, &tab, "type-text", None))
    }
    .await;
    result.into()
}

async fn press_key(browser: Rc<Browser>, args: Args) -> ToolResult {
    let result: Result<ToolResult, String> = async {
        let tab = resolve_visible_tab(&browser, &args)?;
        let key = string_arg(&args, "key").ok_or("key must be a string")?;
        tab.webview()?.set_focused(true);
        input::press_key(&tab, key).await?;
        Ok(done(&browser, &tab, "press-key", None))
    }
    .await;
    result.into()
}

async fn get_cursor_position(browser: Rc<Browser>, args: Args) -> ToolResult {
    let tab = match super::resolve_tab(&browser, &args) {
        Ok(tab) => tab,
        Err(error) => return ToolResult::error(error),
    };
    let cursor = input::cursor_position(&browser, &tab);
    let (width, height) = browser.viewport_size(&tab);
    ToolResult::json(json!({
        "x": cursor.x,
        "y": cursor.y,
        "viewport": {"width": width.round(), "height": height.round()},
    }))
}

fn pointer_schema() -> SchemaBuilder {
    SchemaBuilder::new()
        .number("x", "Viewport x in CSS pixels.")
        .number("y", "Viewport y in CSS pixels.")
        .integer(CLICKABLE_ID_ARG, "Target an element from get-clickables instead of x/y.")
        .integer(
            "duration_ms",
            "Override the movement duration (default: human-like, from distance and target size).",
        )
        .integer(TAB_ID_ARG, TAB_ID_DESCRIPTION)
}

pub fn register(registry: &mut ToolRegistry<Browser>) {
    registry.register(
        "move-cursor",
        "Moves the mouse pointer to a point or element like a person would (curved path, natural \
         acceleration, slight tremor). Fires real mousemove/hover events.",
        pointer_schema().build(),
        |browser, args| pointer_tool(browser, args, Pointer::Move),
    );
    registry.register(
        "click",
        "Moves the pointer to the target and clicks it (press, short hold, release).",
        pointer_schema()
            .string_enum("button", "Mouse button (default left).", &["left", "right", "middle"])
            .build(),
        |browser, args| pointer_tool(browser, args, Pointer::Click),
    );
    registry.register(
        "rightclick",
        "Moves the pointer to the target and right-clicks (opens the context menu if the page doesn't \
         handle it; see get-dialogs).",
        pointer_schema().build(),
        |browser, args| pointer_tool(browser, args, Pointer::RightClick),
    );
    registry.register(
        "double-click",
        "Moves the pointer to the target and double-clicks.",
        pointer_schema().build(),
        |browser, args| pointer_tool(browser, args, Pointer::DoubleClick),
    );
    registry.register(
        "scroll",
        "Scrolls with the mouse wheel. Positive delta_y scrolls down. The wheel is turned where the \
         pointer is unless x/y are given.",
        SchemaBuilder::new()
            .number("delta_x", "Horizontal pixels (positive = right).")
            .number("delta_y", "Vertical pixels (positive = down).")
            .number("x", "Pointer x for the wheel events.")
            .number("y", "Pointer y for the wheel events.")
            .integer(TAB_ID_ARG, TAB_ID_DESCRIPTION)
            .build(),
        scroll,
    );
    registry.register(
        "type-text",
        "Types text into the focused element one key at a time with human timing. Click the field \
         first to focus it. \\n presses Enter.",
        SchemaBuilder::new()
            .required_string("text", "Text to type.")
            .integer("min_delay_ms", "Minimum delay between keys (default 45).")
            .integer("max_delay_ms", "Maximum delay between keys (default 160).")
            .integer(TAB_ID_ARG, TAB_ID_DESCRIPTION)
            .build(),
        type_text,
    );
    registry.register(
        "press-key",
        "Presses a key or chord, e.g. \"Enter\", \"Escape\", \"ArrowDown\", \"Tab\", \"Backspace\", \
         \"Control+a\", \"Shift+Tab\". Names are DOM KeyboardEvent.key values.",
        SchemaBuilder::new()
            .required_string("key", "Key or chord.")
            .integer(TAB_ID_ARG, TAB_ID_DESCRIPTION)
            .build(),
        press_key,
    );
    registry.register(
        "get-cursor-position",
        "Returns the pointer position used for synthetic input in the tab.",
        SchemaBuilder::new().integer(TAB_ID_ARG, TAB_ID_DESCRIPTION).build(),
        get_cursor_position,
    );
}
