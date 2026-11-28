//! A repaint hook for background services, so they don't hold an `egui::Context`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

type Hook = Arc<dyn Fn(Duration) + Send + Sync>;

/// Wakes the UI from any thread. Services (indexer, watcher, peers, jobs) keep a clone for their whole life,
/// the host points it at the shared egui context with `set` once, when it starts. Until then `wake` does
/// nothing.
#[derive(Clone, Default)]
pub struct Waker(Arc<Mutex<Option<Hook>>>);

impl Waker {
    /// Points the waker at a window. `f(Duration::ZERO)` must repaint at once.
    pub fn set(&self, f: impl Fn(Duration) + Send + Sync + 'static) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(f));
    }

    /// Detaches the hook: later wakes are no-ops until `set` is called again (tests only).
    #[cfg(test)]
    pub fn clear(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn wake(&self) {
        self.wake_after(Duration::ZERO);
    }

    pub fn wake_after(&self, d: Duration) {
        // The hook is cloned out so it never runs under the lock (it may call back into egui).
        let hook = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(h) = hook {
            h(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn wake_reaches_the_current_window_only() {
        let (w, a, b) = (Waker::default(), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        w.wake(); // no window yet: must not panic
        let a2 = a.clone();
        w.set(move |_| { a2.fetch_add(1, Ordering::SeqCst); });
        let clone = w.clone(); // services hold clones; they must follow `set` / `clear`
        clone.wake();
        let b2 = b.clone();
        w.set(move |_| { b2.fetch_add(1, Ordering::SeqCst); });
        clone.wake_after(Duration::from_millis(5));
        w.clear();
        clone.wake();
        assert_eq!((a.load(Ordering::SeqCst), b.load(Ordering::SeqCst)), (1, 1));
    }

    #[test]
    fn hook_may_call_back_into_the_waker() {
        let w = Waker::default();
        let inner = w.clone();
        w.set(move |_| inner.clear()); // would deadlock if the hook ran under the lock
        w.wake();
        w.wake(); // cleared by the first call: a no-op
    }
}
