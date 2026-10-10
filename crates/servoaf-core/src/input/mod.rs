// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! Produces mouse, wheel and keyboard input for a tab the way a person would.
//!
//! Events go through `WebView::notify_input_event`, the same entry point the
//! embedder uses for real OS input, so they are hit-tested normally and pages
//! see ordinary trusted events. Operations on one tab are serialized so that
//! concurrent tool calls never interleave their events. Positions are CSS px
//! relative to the viewport.

pub mod human_path;
pub mod keys;

use std::time::Duration;

use euclid::Point2D;
use human_path::{PathOptions, Point, ThreadRandom, human_mouse_path};
use keys::KeySpec;
use servo::{
    CSSPixel, InputEvent, KeyState, KeyboardEvent, Modifiers, MouseButton, MouseButtonAction,
    MouseButtonEvent, MouseMoveEvent, WebViewPoint, WheelDelta, WheelEvent, WheelMode,
};

use crate::browser::{Browser, Tab};
use crate::runtime::sleep;

const WHEEL_NOTCH: f64 = 120.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

impl Button {
    fn servo(self) -> MouseButton {
        match self {
            Button::Left => MouseButton::Primary,
            Button::Middle => MouseButton::Auxiliary,
            Button::Right => MouseButton::Secondary,
        }
    }
}

fn random_ms(min: u64, max: u64) -> Duration {
    let max = max.max(min);
    Duration::from_millis(min + (rand::random::<u64>() % (max - min + 1)))
}

fn page_point(point: Point) -> WebViewPoint {
    WebViewPoint::Page(Point2D::<f32, CSSPixel>::new(point.x as f32, point.y as f32))
}

fn send(tab: &Tab, event: InputEvent) -> Result<(), String> {
    tab.webview()?.notify_input_event(event);
    Ok(())
}

/// Current synthetic cursor position. Before the first movement this is a
/// plausible resting position: somewhere around the middle-lower part.
pub fn cursor_position(browser: &Browser, tab: &Tab) -> Point {
    if let Some(cursor) = tab.cursor.get() {
        return cursor;
    }
    let (width, height) = browser.viewport_size(tab);
    let cursor = Point::new(
        width * (rand::random::<f64>() * 0.5 + 0.25),
        height * (rand::random::<f64>() * 0.4 + 0.45),
    );
    tab.cursor.set(Some(cursor));
    cursor
}

fn set_cursor(browser: &Browser, tab: &Tab, point: Point) {
    tab.cursor.set(Some(point));
    if browser.config.cursor_overlay && browser.active_tab_id() == Some(tab.id) {
        browser.request_paint();
    }
}

async fn move_unlocked(
    browser: &Browser,
    tab: &Tab,
    target: Point,
    options: &PathOptions,
) -> Result<(), String> {
    let start = cursor_position(browser, tab);
    let path = human_mouse_path(start, target, options, &mut ThreadRandom);
    let mut previous = Duration::ZERO;
    for point in path {
        sleep(point.time.saturating_sub(previous)).await;
        previous = point.time;
        send(tab, InputEvent::MouseMove(MouseMoveEvent::new(page_point(point.position))))?;
        set_cursor(browser, tab, point.position);
    }
    Ok(())
}

pub async fn move_to(
    browser: &Browser,
    tab: &Tab,
    target: Point,
    options: PathOptions,
) -> Result<(), String> {
    let _guard = tab.input_lock.lock().await;
    move_unlocked(browser, tab, target, &options).await
}

/// Moves to `target`, then presses and releases `button` `count` times.
pub async fn click(
    browser: &Browser,
    tab: &Tab,
    target: Point,
    button: Button,
    count: u32,
    options: PathOptions,
) -> Result<(), String> {
    let _guard = tab.input_lock.lock().await;
    browser.dismiss_popups(tab.id);
    move_unlocked(browser, tab, target, &options).await?;
    for i in 0..count.clamp(1, 3) {
        // Settle on the target (first press) or pause between clicks.
        sleep(if i == 0 { random_ms(60, 180) } else { random_ms(70, 140) }).await;
        let point = page_point(target);
        send(
            tab,
            InputEvent::MouseButton(MouseButtonEvent::new(MouseButtonAction::Down, button.servo(), point)),
        )?;
        sleep(random_ms(55, 130)).await;
        send(
            tab,
            InputEvent::MouseButton(MouseButtonEvent::new(MouseButtonAction::Up, button.servo(), point)),
        )?;
    }
    Ok(())
}

/// Scrolls by `(dx, dy)` CSS px (positive y = down) with the pointer at `at`,
/// moving there first, as a series of wheel notches.
pub async fn scroll(browser: &Browser, tab: &Tab, at: Point, dx: f64, dy: f64) -> Result<(), String> {
    let _guard = tab.input_lock.lock().await;
    let options = PathOptions {
        target_width: 200.0, // Anywhere over the content will do.
        ..Default::default()
    };
    move_unlocked(browser, tab, at, &options).await?;
    // Servo's wheel deltas point the other way: positive y reveals content
    // above.
    let (mut remaining_x, mut remaining_y) = (-dx, -dy);
    let mut first = true;
    while remaining_x.abs() >= 1.0 || remaining_y.abs() >= 1.0 {
        let step_x = remaining_x.clamp(-WHEEL_NOTCH, WHEEL_NOTCH).trunc();
        let step_y = remaining_y.clamp(-WHEEL_NOTCH, WHEEL_NOTCH).trunc();
        remaining_x -= step_x;
        remaining_y -= step_y;
        sleep(if first { random_ms(80, 200) } else { random_ms(25, 70) }).await;
        first = false;
        let delta = WheelDelta { x: step_x, y: step_y, z: 0.0, mode: WheelMode::DeltaPixel };
        send(tab, InputEvent::Wheel(WheelEvent::new(delta, page_point(at))))?;
    }
    Ok(())
}

fn key_event(state: KeyState, spec: &KeySpec, modifiers: Modifiers) -> InputEvent {
    InputEvent::Keyboard(KeyboardEvent::new_without_event(
        state,
        spec.key.clone(),
        spec.code,
        spec.location,
        modifiers,
        false,
        false,
    ))
}

/// Presses and releases `spec`, holding Shift around it if the key needs it.
async fn key_stroke(tab: &Tab, spec: &KeySpec, held: Modifiers) -> Result<(), String> {
    let shift = spec.implied.contains(Modifiers::SHIFT) && !held.contains(Modifiers::SHIFT);
    let shift_key = keys::for_name("Shift").expect("Shift");
    let mut modifiers = held;
    if shift {
        modifiers |= Modifiers::SHIFT;
        send(tab, key_event(KeyState::Down, &shift_key, modifiers))?;
        sleep(random_ms(20, 60)).await;
    }
    send(tab, key_event(KeyState::Down, spec, modifiers))?;
    sleep(random_ms(30, 90)).await;
    send(tab, key_event(KeyState::Up, spec, modifiers))?;
    if shift {
        sleep(random_ms(10, 40)).await;
        send(tab, key_event(KeyState::Up, &shift_key, held))?;
    }
    Ok(())
}

/// Types `text` into the focused element, one key at a time.
pub async fn type_text(
    tab: &Tab,
    text: &str,
    min_delay: Duration,
    max_delay: Duration,
) -> Result<(), String> {
    let _guard = tab.input_lock.lock().await;
    let (min_ms, max_ms) = (min_delay.as_millis() as u64, max_delay.as_millis() as u64);
    let mut first = true;
    for character in text.chars() {
        // Inter-key interval; a little longer after spaces, like word breaks.
        let mut pause = if first { random_ms(10, 40) } else { random_ms(min_ms, max_ms) };
        if character == ' ' {
            pause += random_ms(0, min_ms);
        }
        first = false;
        sleep(pause).await;
        key_stroke(tab, &keys::for_character(character), Modifiers::empty()).await?;
    }
    Ok(())
}

/// Presses a key or chord such as "Enter", "Control+a", "Shift+Tab".
pub async fn press_key(tab: &Tab, chord: &str) -> Result<(), String> {
    let (modifier_keys, key) = keys::parse_chord(chord)?;
    let _guard = tab.input_lock.lock().await;
    let mut held = Modifiers::empty();
    for modifier in &modifier_keys {
        held |= modifier.modifier();
        sleep(random_ms(20, 60)).await;
        send(tab, key_event(KeyState::Down, modifier, held))?;
    }
    let own = key.modifier();
    if !own.is_empty() {
        // A modifier on its own.
        sleep(random_ms(20, 60)).await;
        send(tab, key_event(KeyState::Down, &key, held | own))?;
        sleep(random_ms(40, 100)).await;
        send(tab, key_event(KeyState::Up, &key, held))?;
    } else {
        sleep(random_ms(20, 60)).await;
        key_stroke(tab, &key, held).await?;
    }
    for modifier in modifier_keys.iter().rev() {
        held.remove(modifier.modifier());
        sleep(random_ms(15, 50)).await;
        send(tab, key_event(KeyState::Up, modifier, held))?;
    }
    Ok(())
}
