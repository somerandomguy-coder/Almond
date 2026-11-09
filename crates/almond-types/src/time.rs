use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use ts_rs::TS;

/// Milliseconds since the Unix epoch (UTC). Exported to TypeScript as `number`.
pub type UnixMs = i64;

pub fn now_ms() -> UnixMs {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Hybrid logical clock timestamp. Totally ordered by (wall_ms, logical); ties across devices are
/// broken by the event's `DeviceId` where a total order is needed (LWW).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS, Default,
)]
#[ts(export)]
pub struct Hlc {
    #[ts(type = "number")]
    pub wall_ms: UnixMs,
    pub logical: u32,
}

/// Thread-safe HLC generator for one device.
#[derive(Debug, Default)]
pub struct HlcClock {
    last: Mutex<Hlc>,
}

impl HlcClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Timestamp for a local event.
    pub fn now(&self) -> Hlc {
        self.tick(now_ms(), None)
    }

    /// Merge a remote timestamp (call when applying remote events) and return a new local one.
    pub fn observe(&self, remote: Hlc) -> Hlc {
        self.tick(now_ms(), Some(remote))
    }

    fn tick(&self, phys: UnixMs, remote: Option<Hlc>) -> Hlc {
        let mut last = self.last.lock().expect("hlc poisoned");
        let r = remote.unwrap_or_default();
        let wall = phys.max(last.wall_ms).max(r.wall_ms);
        let logical = if wall == last.wall_ms && wall == r.wall_ms {
            last.logical.max(r.logical) + 1
        } else if wall == last.wall_ms {
            last.logical + 1
        } else if wall == r.wall_ms {
            r.logical + 1
        } else {
            0
        };
        *last = Hlc {
            wall_ms: wall,
            logical,
        };
        *last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_and_observes_remote() {
        let c = HlcClock::new();
        let a = c.now();
        let b = c.now();
        assert!(b > a);
        let far = Hlc {
            wall_ms: a.wall_ms + 1_000_000,
            logical: 7,
        };
        let d = c.observe(far);
        assert!(d > far);
        assert!(c.now() > d);
    }
}
