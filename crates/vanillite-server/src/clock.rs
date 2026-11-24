//! The monotonic server clock.

use std::time::Instant;

/// Monotonic nanoseconds since server start. Shared by the tick loop and the watchdog.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    start: Instant,
}

impl Clock {
    /// A clock starting now.
    pub fn new() -> Clock {
        Clock {
            start: Instant::now(),
        }
    }

    /// Nanoseconds since the clock started (never 0, so 0 can mean "unset").
    pub fn now(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX) | 1
    }
}

impl Default for Clock {
    fn default() -> Self {
        Clock::new()
    }
}
