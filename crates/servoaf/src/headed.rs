// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! Windowed mode (winit). The page fills the window; a person can use it with
//! mouse and keyboard while an agent drives it over MCP.

use std::cell::Cell;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use dpi::PhysicalSize;
use gleam::gl;
use servo::{
    Code, DevicePoint, EventLoopWaker, InputEvent, Key, KeyState, KeyboardEvent, Location, Modifiers,
    MouseButton, MouseButtonAction, MouseButtonEvent, MouseLeftViewportEvent, MouseMoveEvent, NamedKey,
    RenderingContext, WheelDelta, WheelEvent, WheelMode, WindowRenderingContext,
};
use servoaf_core::Browser;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, KeyLocation, ModifiersState, PhysicalKey};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{Window, WindowId};

use crate::Options;

const LINE_HEIGHT: f64 = 38.0;

#[derive(Debug)]
struct Wake;

#[derive(Clone)]
struct Waker(Arc<Mutex<EventLoopProxy<Wake>>>);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        let _ = self.0.lock().unwrap().send_event(Wake);
    }
}

struct Running {
    window: Window,
    rendering_context: Rc<WindowRenderingContext>,
    browser: Rc<Browser>,
    mouse: Cell<DevicePoint>,
    modifiers: Cell<ModifiersState>,
}

struct App {
    options: Options,
    waker: Waker,
    requests: Option<crossbeam_channel::Receiver<servoaf_core::mcp::http::McpRequest>>,
    running: Option<Running>,
    error: Option<String>,
}

pub fn run(options: Options) -> Result<(), String> {
    let event_loop = EventLoop::<Wake>::with_user_event()
        .build()
        .map_err(|error| format!("cannot open a window (no display? try --headless): {error}"))?;
    let waker = Waker(Arc::new(Mutex::new(event_loop.create_proxy())));
    let http_waker = waker.clone();
    let requests = crate::start_mcp(&options, Arc::new(move || http_waker.wake()))?;
    let signal_waker = waker.clone();
    crate::handle_signals(Arc::new(move || signal_waker.wake()));
    let mut app = App { options, waker, requests: Some(requests), running: None, error: None };
    event_loop.run_app(&mut app).map_err(|error| error.to_string())?;
    app.error.map_or(Ok(()), Err)
}

impl App {
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let (width, height) = self.options.size;
        let attributes = Window::default_attributes()
            .with_title("servoaf")
            .with_inner_size(winit::dpi::LogicalSize::new(width as f64, height as f64));
        let window = event_loop.create_window(attributes).map_err(|error| error.to_string())?;
        let display_handle = event_loop.display_handle().map_err(|error| error.to_string())?;
        let window_handle = window.window_handle().map_err(|error| error.to_string())?;
        let rendering_context = Rc::new(
            WindowRenderingContext::new(display_handle, window_handle, window.inner_size())
                .map_err(|error| format!("cannot create a GL context for the window: {error:?}"))?,
        );
        rendering_context
            .make_current()
            .map_err(|error| format!("cannot make the GL context current: {error:?}"))?;
        let servo = crate::build_servo(&self.options, Box::new(self.waker.clone()));
        let requests = self.requests.take().expect("started once");
        let browser = crate::browser_for(
            &self.options,
            servo,
            rendering_context.clone(),
            window.scale_factor() as f32,
            requests,
        );
        self.running = Some(Running {
            window,
            rendering_context,
            browser,
            mouse: Cell::new(DevicePoint::zero()),
            modifiers: Cell::new(ModifiersState::empty()),
        });
        Ok(())
    }
}

impl Running {
    /// Lets Servo and the tools run, then schedules the next wake-up.
    fn pump(&self, event_loop: &ActiveEventLoop) {
        let deadline = self.browser.pump();
        if self.browser.wants_paint() {
            self.window.request_redraw();
        }
        if let Some(title) = self.browser.take_window_title() {
            self.window.set_title(&title);
        }
        event_loop.set_control_flow(match deadline {
            Some(deadline) => ControlFlow::WaitUntil(deadline.max(Instant::now())),
            None => ControlFlow::Wait,
        });
    }

    fn redraw(&self) {
        self.browser.request_paint();
        self.browser.paint_if_needed();
        if let Some(point) = self.browser.cursor_overlay_point() {
            draw_pointer(&*self.rendering_context, point, self.window.scale_factor() as f32);
        }
        self.rendering_context.present();
    }

    fn send(&self, event: InputEvent) {
        if let Some(webview) = self.browser.active_tab().and_then(|tab| tab.webview().ok()) {
            webview.notify_input_event(event);
        }
    }

    fn handle(&self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(size) => {
                self.browser.resize(PhysicalSize::new(size.width.max(1), size.height.max(1)));
                self.window.request_redraw();
            },
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.browser.set_hidpi_scale(euclid::Scale::new(scale_factor as f32));
            },
            WindowEvent::Focused(focused) => {
                if let Some(webview) = self.browser.active_tab().and_then(|tab| tab.webview().ok()) {
                    webview.set_focused(focused);
                }
            },
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers.set(modifiers.state()),
            WindowEvent::CursorMoved { position, .. } => {
                let point = DevicePoint::new(position.x as f32, position.y as f32);
                self.mouse.set(point);
                self.send(InputEvent::MouseMove(MouseMoveEvent::new(point.into())));
            },
            WindowEvent::CursorLeft { .. } => {
                self.send(InputEvent::MouseLeftViewport(MouseLeftViewportEvent::default()));
            },
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    winit::event::MouseButton::Left => MouseButton::Primary,
                    winit::event::MouseButton::Right => MouseButton::Secondary,
                    winit::event::MouseButton::Middle => MouseButton::Auxiliary,
                    winit::event::MouseButton::Back => MouseButton::Back,
                    winit::event::MouseButton::Forward => MouseButton::Forward,
                    winit::event::MouseButton::Other(_) => return,
                };
                let action = match state {
                    ElementState::Pressed => MouseButtonAction::Down,
                    ElementState::Released => MouseButtonAction::Up,
                };
                self.send(InputEvent::MouseButton(MouseButtonEvent::new(
                    action,
                    button,
                    self.mouse.get().into(),
                )));
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x as f64 * LINE_HEIGHT, y as f64 * LINE_HEIGHT),
                    MouseScrollDelta::PixelDelta(delta) => (delta.x, delta.y),
                };
                let delta = WheelDelta { x, y, z: 0.0, mode: WheelMode::DeltaPixel };
                self.send(InputEvent::Wheel(WheelEvent::new(delta, self.mouse.get().into())));
            },
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some(event) = keyboard_event(&event, self.modifiers.get()) {
                    self.send(InputEvent::Keyboard(event));
                }
            },
            _ => {},
        }
    }
}

/// winit's key and code names are the DOM's, so convert through them.
fn keyboard_event(event: &winit::event::KeyEvent, state: ModifiersState) -> Option<KeyboardEvent> {
    let key = match &event.logical_key {
        WinitKey::Character(text) => Key::Character(text.to_string()),
        WinitKey::Named(named) => {
            Key::Named(NamedKey::from_str(&format!("{named:?}")).unwrap_or(NamedKey::Unidentified))
        },
        WinitKey::Dead(_) => Key::Named(NamedKey::Dead),
        WinitKey::Unidentified(_) => return None,
    };
    let code = match event.physical_key {
        PhysicalKey::Code(code) => Code::from_str(&format!("{code:?}")).unwrap_or(Code::Unidentified),
        PhysicalKey::Unidentified(_) => Code::Unidentified,
    };
    let location = match event.location {
        KeyLocation::Standard => Location::Standard,
        KeyLocation::Left => Location::Left,
        KeyLocation::Right => Location::Right,
        KeyLocation::Numpad => Location::Numpad,
    };
    let mut modifiers = Modifiers::empty();
    modifiers.set(Modifiers::SHIFT, state.shift_key());
    modifiers.set(Modifiers::CONTROL, state.control_key());
    modifiers.set(Modifiers::ALT, state.alt_key());
    modifiers.set(Modifiers::META, state.super_key());
    let key_state = match event.state {
        ElementState::Pressed => KeyState::Down,
        ElementState::Released => KeyState::Up,
    };
    Some(KeyboardEvent::new_without_event(key_state, key, code, location, modifiers, event.repeat, false))
}

/// Draws a small arrow pointer at `point` (device px) on top of the page with
/// scissored clears, so no shaders are needed.
fn draw_pointer(rendering_context: &dyn RenderingContext, point: DevicePoint, scale: f32) {
    let gl = rendering_context.gleam_gl_api();
    let height = rendering_context.size().height as i32;
    let unit = scale.max(1.0);
    let rows = 16;
    let fill = |color: [f32; 4], inset: f32| {
        gl.clear_color(color[0], color[1], color[2], color[3]);
        for row in 0..rows {
            // An arrow: a triangle widening downwards, cut off at the bottom.
            let width = if row < 12 { row as f32 + 1.0 } else { (16 - row) as f32 };
            let x = point.x + inset * unit;
            let w = (width - 2.0 * inset).max(0.0) * unit;
            let y = point.y + (row as f32 + inset) * unit;
            if w <= 0.0 || row as f32 + inset >= rows as f32 - inset {
                continue;
            }
            let gl_y = height - (y + unit).round() as i32;
            gl.scissor(x.round() as i32, gl_y, w.round().max(1.0) as i32, unit.round() as i32);
            gl.clear(gl::COLOR_BUFFER_BIT);
        }
    };
    gl.enable(gl::SCISSOR_TEST);
    fill([0.0, 0.0, 0.0, 1.0], 0.0);
    fill([1.0, 1.0, 1.0, 1.0], 1.0);
    gl.disable(gl::SCISSOR_TEST);
}

impl ApplicationHandler<Wake> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.running.is_none()
            && self.error.is_none()
            && let Err(error) = self.start(event_loop)
        {
            self.error = Some(error);
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _: Wake) {
        if crate::shutdown_requested() {
            event_loop.exit();
            return;
        }
        if let Some(running) = &self.running {
            running.pump(event_loop);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if let Some(running) = &self.running {
            running.handle(event_loop, event);
            running.pump(event_loop);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(running) = &self.running {
            running.pump(event_loop);
        }
    }
}
