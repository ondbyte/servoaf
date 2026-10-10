// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! A tiny single-threaded executor driven by the embedder's event loop.
//!
//! Servo's `WebView`s are not `Send`, so tool calls run as futures on the main
//! thread. The embedder calls [`Runtime::poll`] whenever it wakes up and goes
//! back to sleep until the deadline it returns. Futures never need real wakers:
//! every pending task is re-polled on each wake-up, and while any task is
//! pending the loop wakes at least every [`POLL_INTERVAL`] (replies from
//! Servo's script thread arrive on channels that cannot wake the loop).

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

pub const POLL_INTERVAL: Duration = Duration::from_millis(4);

pub type LocalFuture<T> = Pin<Box<dyn Future<Output = T>>>;

thread_local! {
    static NEXT_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

fn register_deadline(deadline: Instant) {
    NEXT_DEADLINE.with(|next| {
        if next.get().is_none_or(|current| deadline < current) {
            next.set(Some(deadline));
        }
    });
}

#[derive(Default)]
pub struct Runtime {
    tasks: RefCell<Vec<LocalFuture<()>>>,
    spawned: RefCell<Vec<LocalFuture<()>>>,
}

impl Runtime {
    pub fn spawn(&self, future: impl Future<Output = ()> + 'static) {
        self.spawned.borrow_mut().push(Box::pin(future));
    }

    pub fn has_tasks(&self) -> bool {
        !self.tasks.borrow().is_empty() || !self.spawned.borrow().is_empty()
    }

    /// Polls every task once (and tasks spawned meanwhile). Returns when the
    /// event loop should wake up next, or `None` if nothing is pending.
    pub fn poll(&self) -> Option<Instant> {
        NEXT_DEADLINE.with(|next| next.set(None));
        let mut cx = Context::from_waker(Waker::noop());
        let mut tasks = std::mem::take(&mut *self.tasks.borrow_mut());
        tasks.retain_mut(|task| task.as_mut().poll(&mut cx).is_pending());
        loop {
            let mut spawned = std::mem::take(&mut *self.spawned.borrow_mut());
            if spawned.is_empty() {
                break;
            }
            spawned.retain_mut(|task| task.as_mut().poll(&mut cx).is_pending());
            tasks.append(&mut spawned);
        }
        let pending = !tasks.is_empty();
        self.tasks.borrow_mut().append(&mut tasks);
        if !pending {
            return None;
        }
        let soon = Instant::now() + POLL_INTERVAL;
        Some(NEXT_DEADLINE.with(|next| next.get()).map_or(soon, |d| d.min(soon)))
    }

    /// Runs `future` to completion by polling in a loop. For tests and tools
    /// that run without an embedder.
    pub fn block_on<T: 'static>(&self, future: impl Future<Output = T> + 'static) -> T {
        let result = std::rc::Rc::new(RefCell::new(None));
        let slot = result.clone();
        self.spawn(async move {
            *slot.borrow_mut() = Some(future.await);
        });
        loop {
            match self.poll() {
                _ if result.borrow().is_some() => break,
                Some(deadline) => std::thread::sleep(deadline.saturating_duration_since(Instant::now())),
                None => break,
            }
        }
        result.borrow_mut().take().expect("future did not complete")
    }
}

/// Completes after `duration`.
pub fn sleep(duration: Duration) -> Sleep {
    Sleep { deadline: Instant::now() + duration }
}

pub struct Sleep {
    deadline: Instant,
}

impl Future for Sleep {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        if Instant::now() >= self.deadline {
            Poll::Ready(())
        } else {
            register_deadline(self.deadline);
            Poll::Pending
        }
    }
}

/// Resolves once `check` returns `Some`.
pub fn poll_until<T>(mut check: impl FnMut() -> Option<T>) -> impl Future<Output = T> {
    std::future::poll_fn(move |_| match check() {
        Some(value) => Poll::Ready(value),
        None => Poll::Pending,
    })
}

#[derive(Debug)]
pub struct Elapsed;

/// Runs `future`, giving up after `duration`.
pub async fn timeout<T>(duration: Duration, future: impl Future<Output = T>) -> Result<T, Elapsed> {
    let deadline = Instant::now() + duration;
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(move |cx| {
        if let Poll::Ready(value) = future.as_mut().poll(cx) {
            return Poll::Ready(Ok(value));
        }
        if Instant::now() >= deadline {
            return Poll::Ready(Err(Elapsed));
        }
        register_deadline(deadline);
        Poll::Pending
    })
    .await
}

/// A one-shot slot filled by a callback on the main thread (Servo invokes
/// screenshot and similar callbacks from `spin_event_loop`).
pub struct Slot<T>(std::rc::Rc<RefCell<Option<T>>>);

impl<T> Clone for Slot<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<T> Slot<T> {
    pub fn fill(&self, value: T) {
        *self.0.borrow_mut() = Some(value);
    }

    pub async fn wait(self) -> T {
        poll_until(move || self.0.borrow_mut().take()).await
    }
}

/// A FIFO lock so that concurrent input operations on one tab never
/// interleave their events.
#[derive(Default)]
pub struct AsyncLock {
    next_ticket: Cell<u64>,
    serving: Cell<u64>,
}

pub struct AsyncLockGuard<'a>(&'a AsyncLock);

impl Drop for AsyncLockGuard<'_> {
    fn drop(&mut self) {
        self.0.serving.set(self.0.serving.get() + 1);
    }
}

impl AsyncLock {
    pub async fn lock(&self) -> AsyncLockGuard<'_> {
        let ticket = self.next_ticket.get();
        self.next_ticket.set(ticket + 1);
        poll_until(|| (self.serving.get() == ticket).then_some(())).await;
        AsyncLockGuard(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_and_timeout() {
        let runtime = Runtime::default();
        let start = Instant::now();
        runtime.block_on(sleep(Duration::from_millis(30)));
        assert!(start.elapsed() >= Duration::from_millis(30));

        let result = runtime.block_on(timeout(Duration::from_millis(10), sleep(Duration::from_secs(5))));
        assert!(result.is_err());
        let result = runtime.block_on(timeout(Duration::from_secs(5), async { 7 }));
        assert_eq!(result.unwrap(), 7);
    }

    #[test]
    fn lock_is_fifo() {
        let runtime = std::rc::Rc::new(Runtime::default());
        let lock = std::rc::Rc::new(AsyncLock::default());
        let order = std::rc::Rc::new(RefCell::new(Vec::new()));
        for i in 0..3 {
            let (lock, order) = (lock.clone(), order.clone());
            runtime.spawn(async move {
                let _guard = lock.lock().await;
                sleep(Duration::from_millis(5 * (3 - i))).await;
                order.borrow_mut().push(i);
            });
        }
        runtime.block_on(sleep(Duration::from_millis(80)));
        assert_eq!(*order.borrow(), vec![0, 1, 2]);
    }
}
