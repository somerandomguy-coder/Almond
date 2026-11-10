//! When to poll calendar sources. Google push notifications need a public HTTPS endpoint, which a
//! LAN app doesn't have, so we poll: faster when a meeting is close (last-minute changes, links),
//! slower otherwise, with exponential backoff on errors.

use almond_types::calendar::CalendarEvent;
use almond_types::time::UnixMs;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollPolicy {
    /// Interval when a meeting starts within `soon_window`.
    pub soon: Duration,
    /// Normal interval.
    pub normal: Duration,
    /// Interval when nothing is scheduled within `idle_after`.
    pub idle: Duration,
    pub soon_window: Duration,
    pub idle_after: Duration,
    /// Upper bound for error backoff.
    pub max_backoff: Duration,
}

impl Default for PollPolicy {
    fn default() -> Self {
        PollPolicy {
            soon: Duration::from_secs(60),
            normal: Duration::from_secs(5 * 60),
            idle: Duration::from_secs(15 * 60),
            soon_window: Duration::from_secs(30 * 60),
            idle_after: Duration::from_secs(8 * 3600),
            max_backoff: Duration::from_secs(30 * 60),
        }
    }
}

impl PollPolicy {
    /// Delay until the next sync given the currently known events.
    pub fn next_interval(&self, events: &[CalendarEvent], now: UnixMs) -> Duration {
        let next_start = events
            .iter()
            .filter(|e| !e.all_day && e.status != "cancelled" && e.start >= now)
            .map(|e| e.start)
            .min();
        match next_start {
            Some(s) if (s - now) as u128 <= self.soon_window.as_millis() => self.soon,
            Some(s) if (s - now) as u128 <= self.idle_after.as_millis() => self.normal,
            _ => self.idle,
        }
    }

    /// Delay after `consecutive_errors` failed syncs (1 → normal, doubling, capped).
    pub fn backoff(&self, consecutive_errors: u32) -> Duration {
        if consecutive_errors == 0 {
            return self.normal;
        }
        let factor = 1u32 << (consecutive_errors - 1).min(10);
        (self.soon * factor).min(self.max_backoff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(start: UnixMs) -> CalendarEvent {
        CalendarEvent {
            source: "s".into(),
            event_id: start.to_string(),
            calendar_id: "c".into(),
            calendar_name: None,
            title: "t".into(),
            description: None,
            location: None,
            start,
            end: start + 1,
            all_day: false,
            attendees: vec![],
            conference_url: None,
            conference_app: None,
            organizer: None,
            status: "confirmed".into(),
            updated: 0,
        }
    }

    #[test]
    fn intervals() {
        let p = PollPolicy::default();
        let min = 60_000;
        assert_eq!(p.next_interval(&[ev(10 * min)], 0), p.soon);
        assert_eq!(p.next_interval(&[ev(120 * min)], 0), p.normal);
        assert_eq!(
            p.next_interval(&[ev(-5 * min), ev(24 * 60 * min)], 0),
            p.idle
        );
        assert_eq!(p.next_interval(&[], 0), p.idle);
    }

    #[test]
    fn backoff_caps() {
        let p = PollPolicy::default();
        assert_eq!(p.backoff(1), Duration::from_secs(60));
        assert_eq!(p.backoff(3), Duration::from_secs(240));
        assert_eq!(p.backoff(30), p.max_backoff);
    }
}
