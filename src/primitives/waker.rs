use std::sync::OnceLock;
use std::thread::{self, Thread};

/// Adaptive park/unpark waker for worker threads.
pub struct WorkerWaker {
    thread: OnceLock<Thread>,
}

impl WorkerWaker {
    pub const fn new() -> Self {
        Self {
            thread: OnceLock::new(),
        }
    }

    /// Register the current thread as the wakeable target.
    pub fn register(&self) {
        // OnceLock::get_or_init ensures only the first call stores
        self.thread.get_or_init(thread::current);
    }

    /// Wake the registered thread.
    pub fn wake(&self) {
        if let Some(t) = self.thread.get() {
            t.unpark();
        }
    }

    /// Adaptive backoff park.
    ///
    /// - iteration 0: yield
    /// - iteration 1..=10: sleep 100us * iteration
    /// - iteration 11+: sleep 5ms (cap)
    ///
    /// Returns the next iteration value.
    pub fn park_with_backoff(&self, iteration: u32) -> u32 {
        if iteration == 0 {
            thread::yield_now();
        } else if iteration <= 10 {
            thread::park_timeout(std::time::Duration::from_micros(100 * u64::from(iteration)));
        } else {
            thread::park_timeout(std::time::Duration::from_millis(5));
        }
        iteration.saturating_add(1)
    }
}

impl Default for WorkerWaker {
    fn default() -> Self {
        Self::new()
    }
}
