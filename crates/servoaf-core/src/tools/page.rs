// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! get-whole-page-as-html, get-page-text, get-clickables.

use std::collections::HashSet;
use std::rc::Rc;

use serde_json::{Value, json};

use super::{TAB_ID_ARG, TAB_ID_DESCRIPTION, rect_json, resolve_tab};
use crate::browser::{Browser, Tab};
use crate::dom;
use crate::mcp::registry::{Args, SchemaBuilder, ToolRegistry, ToolResult, bool_arg, int_arg};

const DEFAULT_MAX_HTML_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_MAX_TEXT_CHARS: usize = 1024 * 1024;
const DEFAULT_MAX_CLICKABLES: usize = 500;
const MAX_LABEL_CHARS: usize = 150;
/// Elements examined for `cursor: pointer` (script-driven widgets).
const MAX_POINTER_SCAN: usize = 1500;
/// Upper bound on elements whose geometry is queried.
const MAX_CANDIDATES: usize = 5000;
/// Geometry is queried in chunks of this many elements.
const CHUNK: usize = 150;
/// Elements at the end of the page that are always examined.
const TAIL: usize = 60;

/// Things a person would consider clicking or typing into. Mirrors chromeaf's
/// PageAgent heuristics.
const INTERACTIVE_SELECTOR: &str = "a[href], area[href], button, select, textarea, summary, details, \
option, video, audio, input:not([type=hidden]), [onclick], [contenteditable]:not([contenteditable=false]), \
[tabindex]:not([tabindex='-1']), [role=button], [role=checkbox], [role=combobox], [role=link], \
[role=listbox], [role=menuitem], [role=menuitemcheckbox], [role=menuitemradio], [role=option], \
[role=radio], [role=searchbox], [role=slider], [role=spinbutton], [role=switch], [role=tab], \
[role=textbox], [role=treeitem]";

fn limit(args: &Args, key: &str, default: usize) -> usize {
    int_arg(args, key).filter(|value| *value > 0).map_or(default, |value| value as usize)
}

fn truncate_bytes(text: &mut String, max: usize) -> bool {
    if text.len() <= max {
        return false;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    true
}

fn shorten(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_LABEL_CHARS {
        return collapsed;
    }
    let mut short: String = collapsed.chars().take(MAX_LABEL_CHARS).collect();
    short.push('…');
    short
}

async fn get_html(browser: Rc<Browser>, args: Args) -> ToolResult {
    let tab = match resolve_tab(&browser, &args) {
        Ok(tab) => tab,
        Err(error) => return ToolResult::error(error),
    };
    let mut html = match dom::page_source(&browser, &tab).await {
        Ok(html) => html,
        Err(error) => return ToolResult::error(error),
    };
    if !html.is_empty() {
        html.insert_str(0, "<!DOCTYPE html>\n");
    }
    let max = limit(&args, "max_bytes", DEFAULT_MAX_HTML_BYTES);
    let truncated = truncate_bytes(&mut html, max);
    let mut result = ToolResult::text(html);
    if truncated {
        result.add_text(format!("[truncated to {max} bytes]"));
    }
    result
}

async fn get_text(browser: Rc<Browser>, args: Args) -> ToolResult {
    let tab = match resolve_tab(&browser, &args) {
        Ok(tab) => tab,
        Err(error) => return ToolResult::error(error),
    };
    let result: Result<String, String> = async {
        let mut root = dom::find_css(&browser, &tab, "body").await?;
        if root.is_empty() {
            root = dom::find_css(&browser, &tab, "html").await?;
        }
        match root.first() {
            Some(element) => dom::text(&browser, &tab, element).await,
            None => Ok(String::new()),
        }
    }
    .await;
    match result {
        Ok(text) => {
            let max = limit(&args, "max_chars", DEFAULT_MAX_TEXT_CHARS);
            ToolResult::text(if text.chars().count() > max { text.chars().take(max).collect() } else { text })
        },
        Err(error) => ToolResult::error(error),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Bounds {
    fn contains(&self, other: &Bounds) -> bool {
        other.x >= self.x - 0.5
            && other.y >= self.y - 0.5
            && other.x + other.width <= self.x + self.width + 0.5
            && other.y + other.height <= self.y + self.height + 0.5
    }

    /// The part inside a `width` x `height` viewport, if any.
    pub fn visible_part(&self, width: f64, height: f64) -> Option<Bounds> {
        let left = self.x.max(0.0);
        let top = self.y.max(0.0);
        let right = (self.x + self.width).min(width);
        let bottom = (self.y + self.height).min(height);
        (right - left >= 1.0 && bottom - top >= 1.0).then_some(Bounds {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }
}

/// Viewport-relative bounds of a clickable from the last get-clickables.
pub async fn clickable_bounds(browser: &Browser, tab: &Tab, id: u32) -> Result<Bounds, String> {
    let gone = || format!("Clickable {id} no longer exists; call get-clickables again.");
    let element = tab.clickables.borrow().get(&id).cloned().ok_or_else(gone)?;
    let rect = dom::rect(browser, tab, &element).await.map_err(|_| gone())?;
    let (scroll_x, scroll_y) = dom::scroll_offset(browser, tab).await;
    Ok(Bounds {
        x: rect.origin.x - scroll_x,
        y: rect.origin.y - scroll_y,
        width: rect.size.width,
        height: rect.size.height,
    })
}

struct Candidate {
    element: String,
    bounds: Bounds,
}

async fn collect_clickables(
    browser: &Browser,
    tab: &Tab,
    include_offscreen: bool,
    max_count: usize,
) -> Result<Value, String> {
    let (viewport_width, viewport_height) = browser.viewport_size(tab);
    let (scroll_x, scroll_y) = dom::scroll_offset(browser, tab).await;
    let interactive: Vec<String> = dom::find_css(browser, tab, INTERACTIVE_SELECTOR).await?;
    let interactive_set: HashSet<&String> = interactive.iter().collect();

    // Servo resolves each element reference by walking the document, so a
    // per-element query costs time proportional to the element's position.
    // Keep the number of elements examined bounded.
    //
    // Script-driven widgets often only reveal themselves through the pointer
    // cursor; look for those among the first elements of the page.
    let mut scan = dom::find_css(browser, tab, "body *").await.unwrap_or_default();
    scan.truncate(MAX_POINTER_SCAN);
    let cursors = dom::css_values(browser, tab, &scan, "cursor").await?;
    let scanned: HashSet<&String> = scan.iter().collect();
    let pointer: HashSet<&String> =
        scan.iter().zip(&cursors).filter(|(_, c)| *c == "pointer").map(|(e, _)| e).collect();

    // `scan` is a prefix of document order, so this list is in document order.
    let mut ordered: Vec<String> = scan
        .iter()
        .filter(|element| interactive_set.contains(element) || pointer.contains(element))
        .cloned()
        .collect();
    ordered.extend(interactive.iter().filter(|e| !scanned.contains(e)).cloned());
    ordered.truncate(MAX_CANDIDATES);

    // Layout mostly follows document order: in viewport mode stop once a
    // whole chunk is below the fold, then still check the end of the page,
    // where fixed banners and dialogs usually live.
    let mut sized: Vec<Candidate> = Vec::new();
    let mut examined = 0;
    let push_rects = |elements: &[String],
                      rects: Vec<dom::DomResult<euclid::default::Rect<f64>>>,
                      sized: &mut Vec<Candidate>| {
        let mut all_below = true;
        for (element, rect) in elements.iter().zip(rects) {
            let Ok(rect) = rect else { continue };
            if rect.size.width < 1.0 || rect.size.height < 1.0 {
                continue; // display:none, empty
            }
            let bounds = Bounds {
                x: rect.origin.x - scroll_x,
                y: rect.origin.y - scroll_y,
                width: rect.size.width,
                height: rect.size.height,
            };
            all_below &= bounds.y > viewport_height;
            sized.push(Candidate { element: element.clone(), bounds });
        }
        all_below
    };
    for chunk in ordered.chunks(CHUNK) {
        let rects = dom::rects(browser, tab, chunk).await?;
        examined += chunk.len();
        let all_below = push_rects(chunk, rects, &mut sized);
        if !include_offscreen && all_below && examined >= CHUNK {
            break;
        }
    }
    if examined < ordered.len() {
        let tail_start = ordered.len().saturating_sub(TAIL).max(examined);
        let tail = &ordered[tail_start..];
        let rects = dom::rects(browser, tab, tail).await?;
        push_rects(tail, rects, &mut sized);
    }

    // Cursor is inherited: only report the outermost pointer element, and
    // not the parts of a link or button (approximated by geometric
    // containment; document order puts ancestors first).
    let mut kept: Vec<Candidate> = Vec::new();
    let mut regions: Vec<Bounds> = Vec::new();
    for candidate in sized {
        if !interactive_set.contains(&candidate.element)
            && regions.iter().any(|r| r.contains(&candidate.bounds))
        {
            continue;
        }
        regions.push(candidate.bounds);
        kept.push(candidate);
    }

    if !include_offscreen {
        kept.retain(|c| c.bounds.visible_part(viewport_width, viewport_height).is_some());
    }

    // Disabled or invisible elements can't be clicked.
    let ids: Vec<String> = kept.iter().map(|c| c.element.clone()).collect();
    let disabled = dom::attributes(browser, tab, &ids, "disabled").await?;
    let aria_disabled = dom::attributes(browser, tab, &ids, "aria-disabled").await?;
    let visibility = dom::css_values(browser, tab, &ids, "visibility").await?;
    let pointer_events = dom::css_values(browser, tab, &ids, "pointer-events").await?;
    let mut index = 0;
    kept.retain(|_| {
        let ok = disabled[index].is_none()
            && aria_disabled[index].as_deref() != Some("true")
            && visibility[index] != "hidden"
            && pointer_events[index] != "none";
        index += 1;
        ok
    });
    kept.truncate(max_count);

    let ids: Vec<String> = kept.iter().map(|c| c.element.clone()).collect();
    let tags = dom::tag_names(browser, tab, &ids).await?;
    let attribute = |name: &'static str| dom::attributes(browser, tab, &ids, name);
    let roles = attribute("role").await?;
    let hrefs = attribute("href").await?;
    let types = attribute("type").await?;
    let dom_ids = attribute("id").await?;
    let aria_labels = attribute("aria-label").await?;
    let values = attribute("value").await?;
    let placeholders = attribute("placeholder").await?;
    let titles = attribute("title").await?;
    let names = attribute("name").await?;
    let alts = attribute("alt").await?;
    let texts = dom::texts(browser, tab, &ids).await?;

    let mut cache = std::collections::HashMap::new();
    let mut list = Vec::new();
    for (i, candidate) in kept.iter().enumerate() {
        let id = i as u32 + 1;
        cache.insert(id, candidate.element.clone());
        let tag = tags[i].clone().unwrap_or_default();
        let non_empty =
            |value: &Option<String>| value.as_deref().filter(|v| !v.trim().is_empty()).map(shorten);
        let input_type = types[i].as_deref().unwrap_or_default().to_ascii_lowercase();
        let label = non_empty(&aria_labels[i]).or_else(|| {
            if tag == "input" {
                // Never report a password's value.
                let value = if input_type == "password" { None } else { non_empty(&values[i]) };
                value
                    .or_else(|| non_empty(&placeholders[i]))
                    .or_else(|| non_empty(&titles[i]))
                    .or_else(|| non_empty(&names[i]))
            } else {
                texts[i]
                    .as_ref()
                    .ok()
                    .map(|text| shorten(text))
                    .filter(|text| !text.is_empty())
                    .or_else(|| non_empty(&titles[i]))
                    .or_else(|| non_empty(&alts[i]))
                    .or_else(|| non_empty(&placeholders[i]))
                    .or_else(|| non_empty(&values[i]))
            }
        });
        let bounds = candidate.bounds;
        let in_viewport = bounds.visible_part(viewport_width, viewport_height).is_some();
        let mut item = json!({
            "id": id,
            "tag": tag,
            "bounds": rect_json(bounds.x, bounds.y, bounds.width, bounds.height),
            "center": {
                "x": ((bounds.x + bounds.width / 2.0) * 100.0).round() / 100.0,
                "y": ((bounds.y + bounds.height / 2.0) * 100.0).round() / 100.0,
            },
            "in_viewport": in_viewport,
        });
        let mut set = |key: &str, value: Option<String>| {
            if let Some(value) = value.filter(|v| !v.is_empty()) {
                item[key] = Value::from(value);
            }
        };
        set("role", roles[i].clone());
        set("text", label);
        set("href", hrefs[i].clone());
        set("type", types[i].clone());
        set("dom_id", dom_ids[i].clone());
        list.push(item);
    }
    *tab.clickables.borrow_mut() = cache;
    // Icon links: use the alt text of a contained image.
    for (i, candidate) in kept.iter().enumerate() {
        if list[i].get("text").is_none()
            && let Ok(images) = dom::find_css_in(browser, tab, &candidate.element, "img[alt]").await
            && let Some(image) = images.first()
        {
            let alts = dom::attributes(browser, tab, std::slice::from_ref(image), "alt").await?;
            if let Some(alt) = alts.into_iter().next().flatten().filter(|alt| !alt.trim().is_empty()) {
                list[i]["text"] = Value::from(shorten(&alt));
            }
        }
    }

    Ok(json!({
        "viewport": {"width": viewport_width.round(), "height": viewport_height.round()},
        "scroll": {"x": scroll_x, "y": scroll_y},
        "count": list.len(),
        "clickables": list,
    }))
}

async fn get_clickables(browser: Rc<Browser>, args: Args) -> ToolResult {
    let tab = match resolve_tab(&browser, &args) {
        Ok(tab) => tab,
        Err(error) => return ToolResult::error(error),
    };
    let include_offscreen = bool_arg(&args, "include_offscreen").unwrap_or(false);
    let max_count = limit(&args, "max_count", DEFAULT_MAX_CLICKABLES);
    match collect_clickables(&browser, &tab, include_offscreen, max_count).await {
        Ok(value) => ToolResult::json(value),
        Err(error) => ToolResult::error(error),
    }
}

pub fn register(registry: &mut ToolRegistry<Browser>) {
    registry.register(
        "get-whole-page-as-html",
        "Returns the current DOM of the tab's page serialized as HTML (reflects script changes, not \
         just the original source).",
        SchemaBuilder::new()
            .integer(TAB_ID_ARG, TAB_ID_DESCRIPTION)
            .integer("max_bytes", "Truncate after this many bytes (default 4MB).")
            .build(),
        get_html,
    );
    registry.register(
        "get-page-text",
        "Returns the visible text of the page (cheaper than HTML for reading).",
        SchemaBuilder::new()
            .integer(TAB_ID_ARG, TAB_ID_DESCRIPTION)
            .integer("max_chars", "Truncate after this many characters.")
            .build(),
        get_text,
    );
    registry.register(
        "get-clickables",
        "Lists elements a user could click (links, buttons, inputs, ARIA widgets, elements with click \
         handlers or pointer cursor) with their bounds and center in viewport coordinates. Pass `id` \
         as clickable_id to click/move-cursor, or use the center coordinates.",
        SchemaBuilder::new()
            .integer(TAB_ID_ARG, TAB_ID_DESCRIPTION)
            .boolean("include_offscreen", "Also list elements outside the visible viewport.")
            .integer("max_count", "Maximum number of elements (default 500).")
            .build(),
        get_clickables,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(shorten("  a \n b  "), "a b");
        assert_eq!(shorten(&"x".repeat(200)).chars().count(), MAX_LABEL_CHARS + 1);
        let mut text = "héllo".to_owned();
        assert!(truncate_bytes(&mut text, 2));
        assert_eq!(text, "h");
        let outer = Bounds { x: 0.0, y: 0.0, width: 100.0, height: 50.0 };
        let inner = Bounds { x: 10.0, y: 10.0, width: 20.0, height: 20.0 };
        assert!(outer.contains(&inner));
        assert!(!inner.contains(&outer));
        assert!(Bounds { x: -50.0, y: 10.0, width: 40.0, height: 10.0 }.visible_part(800.0, 600.0).is_none());
        let part =
            Bounds { x: -50.0, y: 590.0, width: 100.0, height: 40.0 }.visible_part(800.0, 600.0).unwrap();
        assert_eq!((part.x, part.y, part.width, part.height), (0.0, 590.0, 50.0, 10.0));
    }
}
