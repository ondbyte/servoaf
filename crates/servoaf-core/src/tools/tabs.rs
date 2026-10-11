// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! get-tabs, create-tab, close-tab, activate-tab, navigate, go-back,
//! go-forward, reload, wait-for-load.

use std::rc::Rc;
use std::time::Duration;

use serde_json::json;
use url::Url;

use super::{TAB_ID_ARG, TAB_ID_DESCRIPTION, parse_url, resolve_tab, tab_info};
use crate::browser::{Browser, Tab, WINDOW_ID};
use crate::mcp::registry::{Args, SchemaBuilder, ToolRegistry, ToolResult, bool_arg, int_arg, string_arg};

const DEFAULT_LOAD_TIMEOUT: Duration = Duration::from_secs(20);

fn load_timeout(args: &Args) -> Duration {
    int_arg(args, "timeout_ms")
        .filter(|ms| *ms > 0)
        .map_or(DEFAULT_LOAD_TIMEOUT, |ms| Duration::from_millis(ms.min(600_000) as u64))
}

fn add_wait_args(schema: SchemaBuilder) -> SchemaBuilder {
    schema
        .boolean("wait_for_load", "Wait until the page finishes loading (default true).")
        .integer("timeout_ms", "Maximum time to wait for the load.")
}

/// Waits (unless told not to) and reports the tab.
async fn reply_after_load(
    browser: &Browser,
    tab: &Tab,
    starts_before: Option<u64>,
    args: &Args,
) -> ToolResult {
    let mut info = tab_info(browser, tab);
    if bool_arg(args, "wait_for_load").unwrap_or(true) {
        let loaded = tab.wait_for_load(starts_before, load_timeout(args)).await;
        if tab.is_closed() {
            return ToolResult::error("Tab closed while loading");
        }
        info = tab_info(browser, tab);
        info["load_complete"] = loaded.into();
    }
    ToolResult::json(info)
}

async fn get_tabs(browser: Rc<Browser>, _: Args) -> ToolResult {
    let tabs: Vec<_> = browser.tabs().iter().map(|tab| tab_info(&browser, tab)).collect();
    ToolResult::json(json!({"windows": [{"window_id": WINDOW_ID, "focused": true, "tabs": tabs}]}))
}

async fn create_tab(browser: Rc<Browser>, args: Args) -> ToolResult {
    if let Some(window_id) = int_arg(&args, "window_id")
        && window_id != WINDOW_ID as i64
    {
        return ToolResult::error(format!("No window with id {window_id}"));
    }
    let url = match string_arg(&args, "url") {
        Some(url) => match parse_url(url) {
            Ok(url) => url,
            Err(error) => return ToolResult::error(error),
        },
        None => Url::parse("about:blank").expect("valid"),
    };
    let Some(tab) = browser.create_tab(url, bool_arg(&args, "foreground").unwrap_or(true)) else {
        return ToolResult::error("Could not open a tab");
    };
    // The new WebView has not started loading yet.
    reply_after_load(&browser, &tab, Some(0), &args).await
}

async fn close_tab(browser: Rc<Browser>, args: Args) -> ToolResult {
    let tab = match resolve_tab(&browser, &args) {
        Ok(tab) => tab,
        Err(error) => return ToolResult::error(error),
    };
    browser.close_tab(tab.id);
    ToolResult::json(json!({"closed_tab_id": tab.id}))
}

async fn activate_tab(browser: Rc<Browser>, args: Args) -> ToolResult {
    let tab = match resolve_tab(&browser, &args) {
        Ok(tab) => tab,
        Err(error) => return ToolResult::error(error),
    };
    browser.activate(tab.id);
    ToolResult::json(tab_info(&browser, &tab))
}

async fn navigate(browser: Rc<Browser>, args: Args) -> ToolResult {
    let result: Result<_, String> = (|| {
        let tab = resolve_tab(&browser, &args)?;
        let url = parse_url(string_arg(&args, "url").unwrap_or_default())?;
        let webview = tab.webview()?;
        let before = tab.begin_navigation();
        webview.load(url);
        Ok((tab, before))
    })();
    match result {
        Ok((tab, before)) => reply_after_load(&browser, &tab, Some(before), &args).await,
        Err(error) => ToolResult::error(error),
    }
}

#[derive(Clone, Copy)]
enum History {
    Back,
    Forward,
    Reload,
}

async fn history(browser: Rc<Browser>, args: Args, action: History) -> ToolResult {
    let result: Result<_, String> = (|| {
        let tab = resolve_tab(&browser, &args)?;
        let webview = tab.webview()?;
        match action {
            History::Back if !webview.can_go_back() => return Err("Cannot go back".into()),
            History::Forward if !webview.can_go_forward() => return Err("Cannot go forward".into()),
            _ => {},
        }
        let before = tab.begin_navigation();
        match action {
            History::Back => drop(webview.go_back(1)),
            History::Forward => drop(webview.go_forward(1)),
            History::Reload => webview.reload(),
        }
        Ok((tab, before))
    })();
    match result {
        Ok((tab, before)) => reply_after_load(&browser, &tab, Some(before), &args).await,
        Err(error) => ToolResult::error(error),
    }
}

async fn wait_for_load(browser: Rc<Browser>, args: Args) -> ToolResult {
    let tab = match resolve_tab(&browser, &args) {
        Ok(tab) => tab,
        Err(error) => return ToolResult::error(error),
    };
    let loaded = tab.wait_for_load(None, load_timeout(&args)).await;
    let mut info = tab_info(&browser, &tab);
    info["load_complete"] = loaded.into();
    ToolResult::json(info)
}

pub fn register(registry: &mut ToolRegistry<Browser>) {
    registry.register(
        "get-tabs",
        "Lists all browser windows and their tabs (id, title, url, active, loading).",
        SchemaBuilder::new().build(),
        get_tabs,
    );
    registry.register(
        "create-tab",
        "Opens a new tab.",
        add_wait_args(
            SchemaBuilder::new()
                .string("url", "URL to open. Defaults to about:blank.")
                .boolean("foreground", "Activate the new tab (default true).")
                .integer("window_id", "Window to open the tab in (from get-tabs)."),
        )
        .build(),
        create_tab,
    );
    registry.register(
        "close-tab",
        "Closes a tab.",
        SchemaBuilder::new().required_integer(TAB_ID_ARG, "Tab to close.").build(),
        close_tab,
    );
    registry.register(
        "activate-tab",
        "Switches to a tab, making it the active one.",
        SchemaBuilder::new().required_integer(TAB_ID_ARG, "Tab to activate.").build(),
        activate_tab,
    );
    registry.register(
        "navigate",
        "Loads a URL in a tab, as if typed into the address bar.",
        add_wait_args(
            SchemaBuilder::new()
                .required_string("url", "URL (or host name) to load.")
                .integer(TAB_ID_ARG, TAB_ID_DESCRIPTION),
        )
        .build(),
        navigate,
    );
    for (name, description, action) in [
        ("go-back", "Goes back in the tab's history.", History::Back),
        ("go-forward", "Goes forward in the tab's history.", History::Forward),
        ("reload", "Reloads the tab.", History::Reload),
    ] {
        registry.register(
            name,
            description,
            add_wait_args(SchemaBuilder::new().integer(TAB_ID_ARG, TAB_ID_DESCRIPTION)).build(),
            move |browser, args| history(browser, args, action),
        );
    }
    registry.register(
        "wait-for-load",
        "Waits until the tab has finished loading.",
        SchemaBuilder::new()
            .integer(TAB_ID_ARG, TAB_ID_DESCRIPTION)
            .integer("timeout_ms", "Maximum time to wait.")
            .build(),
        wait_for_load,
    );
}
