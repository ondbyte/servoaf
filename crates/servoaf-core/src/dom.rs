// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! Reads the DOM through Servo's built-in WebDriver script commands.
//!
//! These are handled natively (Rust, in the script thread) by
//! `webdriver_handlers.rs`: no script is injected or evaluated in the page and
//! no WebDriver session exists, so pages cannot observe them. Commands sent in
//! one batch are answered in order, which keeps large queries fast.

use std::fmt::Debug;
use std::time::{Duration, Instant};

use euclid::default::Rect;
use serde::Serialize;
use serde::de::DeserializeOwned;
use servo::{JSValue, WebDriverCommandMsg, WebDriverScriptCommand};
use servo_base::generic_channel::{self, GenericReceiver, GenericSender, TryReceiveError};
use servo_base::id::BrowsingContextId;

use crate::browser::{Browser, Tab};
use crate::runtime::poll_until;

const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a poll may block the main thread waiting for the script thread.
const BLOCKING_SLICE: Duration = Duration::from_millis(20);

pub type DomResult<T> = Result<T, String>;

fn not_answering() -> String {
    "The page did not answer in time (it may be busy or blocked by a dialog; see get-dialogs)".into()
}

fn blocked(browser: &Browser, tab: &Tab) -> Option<String> {
    browser
        .blocking_dialog(tab.id)
        .map(|dialog| format!("The page is paused by an open {dialog}; answer it with respond-dialog first"))
}

async fn receive<T>(
    browser: &Browser,
    tab: &Tab,
    receiver: GenericReceiver<T>,
    deadline: Instant,
) -> DomResult<T>
where
    T: Serialize + DeserializeOwned,
{
    poll_until(|| {
        if let Some(error) = blocked(browser, tab) {
            return Some(Err(error));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Some(Err(not_answering()));
        }
        match receiver.try_recv_timeout(remaining.min(BLOCKING_SLICE)) {
            Ok(value) => Some(Ok(value)),
            Err(TryReceiveError::Empty) => None,
            Err(TryReceiveError::ReceiveError(_)) => Some(Err("The page went away".into())),
        }
    })
    .await
}

fn flatten<T, E: Debug>(result: DomResult<Result<T, E>>) -> DomResult<T> {
    result?.map_err(|error| format!("{error:?}"))
}

/// Sends one script command per item, then collects the replies in order.
pub async fn batch<I, T>(
    browser: &Browser,
    tab: &Tab,
    items: impl IntoIterator<Item = I>,
    build: impl Fn(I, GenericSender<T>) -> WebDriverScriptCommand,
) -> DomResult<Vec<DomResult<T>>>
where
    T: Serialize + DeserializeOwned,
{
    if let Some(error) = blocked(browser, tab) {
        return Err(error);
    }
    let webview = tab.webview()?;
    let context = BrowsingContextId::from(webview.id());
    let mut receivers = Vec::new();
    for item in items {
        let (sender, receiver) = generic_channel::channel().ok_or("Could not create a channel")?;
        browser
            .servo()
            .execute_webdriver_command(WebDriverCommandMsg::ScriptCommand(context, build(item, sender)));
        receivers.push(receiver);
    }
    let deadline = Instant::now() + REPLY_TIMEOUT;
    let started = Instant::now();
    let count = receivers.len();
    let mut results = Vec::with_capacity(receivers.len());
    for receiver in receivers {
        let result = receive(browser, tab, receiver, deadline).await;
        if result.is_err() {
            if let Some(error) = blocked(browser, tab) {
                return Err(error);
            }
            if Instant::now() >= deadline {
                return Err(not_answering());
            }
        }
        results.push(result);
    }
    log::debug!("dom batch of {count} took {:?}", started.elapsed());
    Ok(results)
}

pub async fn one<T>(
    browser: &Browser,
    tab: &Tab,
    build: impl FnOnce(GenericSender<T>) -> WebDriverScriptCommand,
) -> DomResult<T>
where
    T: Serialize + DeserializeOwned,
{
    let build = std::cell::Cell::new(Some(build));
    let mut results =
        batch(browser, tab, [()], |(), sender| (build.take().expect("called once"))(sender)).await?;
    results.pop().unwrap_or_else(|| Err("no reply".into()))
}

// Typed helpers ---------------------------------------------------------------

pub async fn page_source(browser: &Browser, tab: &Tab) -> DomResult<String> {
    flatten(one(browser, tab, WebDriverScriptCommand::GetPageSource).await)
}

pub async fn find_css(browser: &Browser, tab: &Tab, selector: &str) -> DomResult<Vec<String>> {
    let selector = selector.to_owned();
    flatten(
        one(browser, tab, move |sender| WebDriverScriptCommand::FindElementsCSSSelector(selector, sender))
            .await,
    )
}

pub async fn find_css_in(
    browser: &Browser,
    tab: &Tab,
    element: &str,
    selector: &str,
) -> DomResult<Vec<String>> {
    let (element, selector) = (element.to_owned(), selector.to_owned());
    flatten(
        one(browser, tab, move |sender| {
            WebDriverScriptCommand::FindElementElementsCSSSelector(selector, element, sender)
        })
        .await,
    )
}

pub async fn text(browser: &Browser, tab: &Tab, element: &str) -> DomResult<String> {
    let element = element.to_owned();
    flatten(one(browser, tab, move |sender| WebDriverScriptCommand::GetElementText(element, sender)).await)
}

/// Document-relative rectangle (WebDriver "element rect").
pub async fn rect(browser: &Browser, tab: &Tab, element: &str) -> DomResult<Rect<f64>> {
    let element = element.to_owned();
    flatten(one(browser, tab, move |sender| WebDriverScriptCommand::GetElementRect(element, sender)).await)
}

pub async fn rects(
    browser: &Browser,
    tab: &Tab,
    elements: &[String],
) -> DomResult<Vec<DomResult<Rect<f64>>>> {
    let results =
        batch(browser, tab, elements.iter().cloned(), WebDriverScriptCommand::GetElementRect).await?;
    Ok(results.into_iter().map(flatten).collect())
}

pub async fn texts(browser: &Browser, tab: &Tab, elements: &[String]) -> DomResult<Vec<DomResult<String>>> {
    let results =
        batch(browser, tab, elements.iter().cloned(), WebDriverScriptCommand::GetElementText).await?;
    Ok(results.into_iter().map(flatten).collect())
}

pub async fn tag_names(
    browser: &Browser,
    tab: &Tab,
    elements: &[String],
) -> DomResult<Vec<DomResult<String>>> {
    let results =
        batch(browser, tab, elements.iter().cloned(), WebDriverScriptCommand::GetElementTagName).await?;
    Ok(results.into_iter().map(|result| flatten(result).map(|tag| tag.to_ascii_lowercase())).collect())
}

pub async fn attributes(
    browser: &Browser,
    tab: &Tab,
    elements: &[String],
    name: &str,
) -> DomResult<Vec<Option<String>>> {
    let results = batch(browser, tab, elements.iter().cloned(), |element, sender| {
        WebDriverScriptCommand::GetElementAttribute(element, name.to_owned(), sender)
    })
    .await?;
    Ok(results.into_iter().map(|result| flatten(result).ok().flatten()).collect())
}

pub async fn css_values(
    browser: &Browser,
    tab: &Tab,
    elements: &[String],
    property: &str,
) -> DomResult<Vec<String>> {
    let results = batch(browser, tab, elements.iter().cloned(), |element, sender| {
        WebDriverScriptCommand::GetElementCSS(element, property.to_owned(), sender)
    })
    .await?;
    Ok(results.into_iter().map(|result| flatten(result).unwrap_or_default()).collect())
}

pub async fn number_property(browser: &Browser, tab: &Tab, element: &str, name: &str) -> f64 {
    let (element, name) = (element.to_owned(), name.to_owned());
    match flatten(
        one(browser, tab, move |sender| WebDriverScriptCommand::GetElementProperty(element, name, sender))
            .await,
    ) {
        Ok(JSValue::Number(value)) if value.is_finite() => value,
        _ => 0.0,
    }
}

/// The viewport's scroll offset. `scrollTop` of the root element is the
/// viewport scroll in standards mode; in quirks mode it's the body's.
pub async fn scroll_offset(browser: &Browser, tab: &Tab) -> (f64, f64) {
    let mut x = 0.0f64;
    let mut y = 0.0f64;
    for selector in ["html", "body"] {
        if let Ok(found) = find_css(browser, tab, selector).await
            && let Some(element) = found.first()
        {
            x = x.max(number_property(browser, tab, element, "scrollLeft").await);
            y = y.max(number_property(browser, tab, element, "scrollTop").await);
        }
    }
    (x, y)
}
