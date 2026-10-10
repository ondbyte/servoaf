// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! The MCP tools. Names, arguments and result shapes follow chromeaf so the
//! same clients and scripts work with both browsers.

mod dialogs;
mod input;
mod page;
mod screenshot;
mod tabs;

use std::rc::Rc;

use serde_json::{Value, json};
use url::Url;

use crate::browser::{Browser, Tab, WINDOW_ID};
use crate::mcp::registry::{Args, ToolRegistry, int_arg};

pub const TAB_ID_ARG: &str = "tab_id";
pub const TAB_ID_DESCRIPTION: &str = "Tab to act on (from get-tabs). Defaults to the active tab.";

/// Every tool servoaf provides.
pub fn registry() -> ToolRegistry<Browser> {
    let mut registry = ToolRegistry::default();
    tabs::register(&mut registry);
    page::register(&mut registry);
    screenshot::register(&mut registry);
    input::register(&mut registry);
    dialogs::register(&mut registry);
    registry
}

/// The tab named by `tab_id`, or the active tab.
pub fn resolve_tab(browser: &Browser, args: &Args) -> Result<Rc<Tab>, String> {
    match int_arg(args, TAB_ID_ARG) {
        Some(id) => u32::try_from(id)
            .ok()
            .and_then(|id| browser.tab(id))
            .ok_or_else(|| format!("No tab with id {id}")),
        None => browser.active_tab().ok_or_else(|| "No tab is open; use create-tab".into()),
    }
}

/// Like [`resolve_tab`], but also brings the tab to the front: rendering and
/// input only happen for the visible tab.
pub fn resolve_visible_tab(browser: &Browser, args: &Args) -> Result<Rc<Tab>, String> {
    let tab = resolve_tab(browser, args)?;
    if browser.active_tab_id() != Some(tab.id) {
        browser.activate(tab.id);
    }
    Ok(tab)
}

pub fn tab_info(browser: &Browser, tab: &Tab) -> Value {
    let index = browser.tabs().iter().position(|t| t.id == tab.id).map_or(-1, |i| i as i64);
    let mut info = json!({
        "tab_id": tab.id,
        "window_id": WINDOW_ID,
        "index": index,
        "active": browser.active_tab_id() == Some(tab.id),
        "title": tab.title(),
        "url": tab.url(),
        "loading": tab.loading.get(),
    });
    if let Some(reason) = tab.crashed.borrow().as_ref() {
        info["crashed"] = Value::from(reason.as_str());
    }
    info
}

pub fn rect_json(x: f64, y: f64, width: f64, height: f64) -> Value {
    let round = |v: f64| (v * 100.0).round() / 100.0;
    json!({"x": round(x), "y": round(y), "width": round(width), "height": round(height)})
}

/// Interprets text the way an address bar does: full URLs as-is, otherwise a
/// host name ("example.com", "localhost:8000") gets a scheme.
pub fn parse_url(input: &str) -> Result<Url, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Empty URL".into());
    }
    let invalid = || format!("Invalid url: {input}");
    if let Ok(url) = Url::parse(input) {
        let scheme = url.scheme();
        if scheme == "javascript" {
            return Err("javascript: URLs are not allowed".into());
        }
        if input.contains("://") || matches!(scheme, "about" | "data" | "blob" | "mailto") {
            return Ok(url);
        }
    }
    if input.starts_with('/') {
        return Url::from_file_path(input).map_err(|_| invalid());
    }
    if input.contains(char::is_whitespace) {
        return Err(invalid());
    }
    let host = input.split(['/', '?', '#']).next().unwrap_or_default();
    let name = host.rsplit_once(':').map_or(host, |(name, _)| name);
    let local = name.eq_ignore_ascii_case("localhost") || name.parse::<std::net::IpAddr>().is_ok();
    if !local && !name.contains('.') {
        return Err(invalid());
    }
    let scheme = if local { "http" } else { "https" };
    Url::parse(&format!("{scheme}://{input}")).map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(parse_url("https://a.example/x").unwrap().as_str(), "https://a.example/x");
        assert_eq!(parse_url("example.com").unwrap().as_str(), "https://example.com/");
        assert_eq!(parse_url("localhost:8000/a").unwrap().as_str(), "http://localhost:8000/a");
        assert_eq!(parse_url("127.0.0.1:9").unwrap().as_str(), "http://127.0.0.1:9/");
        assert_eq!(parse_url("about:blank").unwrap().as_str(), "about:blank");
        assert!(parse_url("data:text/html,hi").is_ok());
        assert_eq!(parse_url("/tmp/x.html").unwrap().scheme(), "file");
        assert!(parse_url("javascript:alert(1)").is_err());
        assert!(parse_url("not a url").is_err());
        assert!(parse_url("word").is_err());
    }
}
