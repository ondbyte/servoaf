// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! Reads the DOM through Servo's built-in WebDriver script commands.
//!
//! These are handled natively (Rust, in the script thread) by
//! `webdriver_handlers.rs`: no script is injected or evaluated in the page and
//! no WebDriver session exists, so pages cannot observe them. Commands sent in
//! one batch are answered in order, which keeps large queries fast.

use std::cell::RefCell;
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

pub type DomResult<T> = Result<T, String>;

/// Reply channels whose request we stopped waiting for (timeout, an early
/// return, or a cancelled tool call) but which the script thread has not
/// answered yet.
///
/// Servo's script thread `unwrap()`s the send of every WebDriver reply. If the
/// receiving end were dropped, a late answer would panic the script thread and
/// take down every tab it runs (they are shared per origin). So unanswered
/// receivers are parked here and dropped only once the answer has arrived or
/// the sender is gone.
#[derive(Default)]
pub struct PendingReplies(RefCell<Vec<Box<dyn FnMut() -> bool>>>);

impl PendingReplies {
    fn adopt<T: Serialize + DeserializeOwned + 'static>(&self, receiver: GenericReceiver<T>) {
        self.0
            .borrow_mut()
            .push(Box::new(move || !matches!(receiver.try_recv(), Err(TryReceiveError::Empty))));
    }

    /// Drops the parked receivers that have been answered. Call regularly.
    pub fn poll(&self) {
        let mut pending = std::mem::take(&mut *self.0.borrow_mut());
        pending.retain_mut(|answered| !answered());
        self.0.borrow_mut().append(&mut pending);
    }

    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A reply receiver that is never closed before its answer arrives.
struct Reply<'a, T: Serialize + DeserializeOwned + 'static> {
    receiver: Option<GenericReceiver<T>>,
    pending: &'a PendingReplies,
}

impl<T: Serialize + DeserializeOwned + 'static> Drop for Reply<'_, T> {
    fn drop(&mut self) {
        if let Some(receiver) = self.receiver.take() {
            self.pending.adopt(receiver);
        }
    }
}

fn not_answering() -> String {
    "The page did not answer in time (it may be busy or blocked by a dialog; see get-dialogs)".into()
}

fn blocked(browser: &Browser, tab: &Tab) -> Option<String> {
    browser
        .blocking_dialog(tab.id)
        .map(|dialog| format!("The page is paused by an open {dialog}; answer it with respond-dialog first"))
}

async fn receive<T>(browser: &Browser, tab: &Tab, reply: &mut Reply<'_, T>, deadline: Instant) -> DomResult<T>
where
    T: Serialize + DeserializeOwned + 'static,
{
    poll_until(|| {
        let receiver = reply.receiver.as_ref()?;
        // Never block the main thread: the runtime re-polls every few ms.
        match receiver.try_recv() {
            Ok(value) => {
                reply.receiver = None;
                return Some(Ok(value));
            },
            Err(TryReceiveError::ReceiveError(_)) => {
                reply.receiver = None;
                return Some(Err("The page went away".into()));
            },
            Err(TryReceiveError::Empty) => {},
        }
        if let Some(error) = blocked(browser, tab) {
            return Some(Err(error));
        }
        (Instant::now() >= deadline).then(|| Err(not_answering()))
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
    T: Serialize + DeserializeOwned + 'static,
{
    if let Some(error) = blocked(browser, tab) {
        return Err(error);
    }
    let webview = tab.webview()?;
    let context = BrowsingContextId::from(webview.id());
    let pending = browser.pending_replies();
    let mut replies = Vec::new();
    for item in items {
        let (sender, receiver) = generic_channel::channel().ok_or("Could not create a channel")?;
        browser
            .servo()
            .execute_webdriver_command(WebDriverCommandMsg::ScriptCommand(context, build(item, sender)));
        replies.push(Reply { receiver: Some(receiver), pending });
    }
    let deadline = Instant::now() + REPLY_TIMEOUT;
    let started = Instant::now();
    let count = replies.len();
    let mut results = Vec::with_capacity(replies.len());
    // Replies not consumed below (early return, cancellation) are parked in
    // `pending` when `replies` is dropped.
    for reply in replies.iter_mut() {
        let result = receive(browser, tab, reply, deadline).await;
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
    T: Serialize + DeserializeOwned + 'static,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abandoned_reply_keeps_channel_open_until_answered() {
        let pending = PendingReplies::default();
        let (sender, receiver) = generic_channel::channel::<u32>().expect("channel");
        // Stop waiting before the answer arrives (timeout / cancelled tool).
        drop(Reply { receiver: Some(receiver), pending: &pending });
        assert_eq!(pending.len(), 1);
        pending.poll();
        assert_eq!(pending.len(), 1, "still waiting for the answer");
        // The script thread answers late: the send must still succeed
        // (Servo unwrap()s it).
        sender.send(7).expect("late reply must not hit a closed channel");
        pending.poll();
        assert!(pending.is_empty(), "dropped once answered");
    }

    #[test]
    fn parked_reply_is_dropped_when_sender_goes_away() {
        let pending = PendingReplies::default();
        let (sender, receiver) = generic_channel::channel::<u32>().expect("channel");
        drop(Reply { receiver: Some(receiver), pending: &pending });
        drop(sender);
        pending.poll();
        assert!(pending.is_empty());
    }
}
