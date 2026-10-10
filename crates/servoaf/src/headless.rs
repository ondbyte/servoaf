// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! Headless mode: offscreen software rendering, no display server needed.

use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use dpi::PhysicalSize;
use servo::{EventLoopWaker, RenderingContext, SoftwareRenderingContext};

use crate::Options;

/// Wakes the headless loop from any thread.
#[derive(Clone, Default)]
struct Waker(Arc<(Mutex<bool>, Condvar)>);

impl Waker {
    fn wake(&self) {
        let (flag, condvar) = &*self.0;
        *flag.lock().unwrap() = true;
        condvar.notify_one();
    }

    fn wait_until(&self, deadline: Option<Instant>) {
        let (flag, condvar) = &*self.0;
        let mut woken = flag.lock().unwrap();
        // Idle at most this long so signals and channel replies are noticed.
        let deadline = deadline.unwrap_or_else(|| Instant::now() + Duration::from_millis(250));
        while !*woken {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            woken = condvar.wait_timeout(woken, remaining).unwrap().0;
        }
        *woken = false;
    }
}

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        Waker::wake(self);
    }
}

pub fn run(options: Options) -> Result<(), String> {
    let waker = Waker::default();
    let http_waker = waker.clone();
    let requests = crate::start_mcp(&options, Arc::new(move || http_waker.wake()))?;
    let signal_waker = waker.clone();
    crate::handle_signals(Arc::new(move || signal_waker.wake()));

    let size = PhysicalSize::new(options.size.0.max(1), options.size.1.max(1));
    let rendering_context = Rc::new(
        SoftwareRenderingContext::new(size)
            .map_err(|error| format!("cannot create a software GL context: {error:?}"))?,
    );
    rendering_context
        .make_current()
        .map_err(|error| format!("cannot make the GL context current: {error:?}"))?;
    let servo = crate::build_servo(&options, Box::new(waker.clone()));
    let browser = crate::browser_for(&options, servo, rendering_context.clone(), 1.0, requests);
    eprintln!("servoaf: running headless at {}x{}", size.width, size.height);

    while !crate::shutdown_requested() {
        let deadline = browser.pump();
        if browser.paint_if_needed() {
            rendering_context.present();
        }
        waker.wait_until(deadline);
    }
    eprintln!("servoaf: shutting down");
    Ok(())
}
