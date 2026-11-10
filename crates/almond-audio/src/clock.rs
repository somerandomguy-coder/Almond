//! Shared capture clock: every channel of one recording measures time from the same epoch so
//! `AudioFrame::t0_ms` values are directly comparable across channels.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Monotonic epoch shared by all channels of a capture session.
#[derive(Debug, Clone, Copy)]
pub struct CaptureClock {
    epoch: Instant,
    epoch_unix_ms: u64,
}

impl CaptureClock {
    /// Start a new clock now.
    pub fn start() -> Self {
        let epoch_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Self {
            epoch: Instant::now(),
            epoch_unix_ms,
        }
    }

    pub fn epoch(&self) -> Instant {
        self.epoch
    }

    /// Wall-clock time (Unix ms) of the epoch; use as `Meeting::started_at`.
    pub fn epoch_unix_ms(&self) -> u64 {
        self.epoch_unix_ms
    }

    /// Seconds since the epoch for `at` (0 if `at` is before the epoch).
    pub fn secs_at(&self, at: Instant) -> f64 {
        at.saturating_duration_since(self.epoch).as_secs_f64()
    }

    /// Seconds elapsed since the epoch.
    pub fn now_secs(&self) -> f64 {
        self.secs_at(Instant::now())
    }

    pub fn elapsed(&self) -> Duration {
        self.epoch.elapsed()
    }
}

impl Default for CaptureClock {
    fn default() -> Self {
        Self::start()
    }
}
