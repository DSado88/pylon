use std::time::Duration;

/// Result of polling all PTYs during a single frame.
pub enum FrameResult {
    /// At least one PTY produced data -- keep spinning to drain remaining.
    DataReceived,
    /// No PTYs produced data -- idle until next tick.
    Idle,
}

/// Returns the recommended sleep duration for the event loop.
///
/// - `DataReceived` -> `None` (poll immediately, no sleep)
/// - `Idle` -> `Some(16ms)` (~60fps for cursor blink / sidebar refresh)
pub fn next_wait_duration(result: FrameResult) -> Option<Duration> {
    match result {
        FrameResult::DataReceived => None,
        FrameResult::Idle => Some(Duration::from_millis(16)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_received_returns_none() {
        assert!(next_wait_duration(FrameResult::DataReceived).is_none());
    }

    #[test]
    fn idle_returns_16ms() {
        let dur = next_wait_duration(FrameResult::Idle);
        assert_eq!(dur, Some(Duration::from_millis(16)));
    }
}
