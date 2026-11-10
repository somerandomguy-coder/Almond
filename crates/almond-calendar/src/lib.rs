//! # almond-calendar — calendar sources, Google OAuth and meeting reminders (WS-E)
//!
//! * [`oauth`] — Google installed-app OAuth (loopback redirect + PKCE + state), multiple accounts
//!   keyed by email, refresh tokens in the [`SecretStore`](almond_types::secrets::SecretStore).
//! * [`google::GoogleCalendar`] — `calendarList` + `events.list` with `syncToken` incremental sync.
//! * [`ics::IcsSubscription`] — any `.ics` / `webcal://` feed, recurrence expanded in the window.
//! * [`phone::PhoneCalendar`] — adapter for events pushed by the Android plugin (WS-G); the JSON
//!   shape is documented in [`phone`].
//! * [`reminder::ReminderScheduler`] — "meeting in N minutes" reminders with a pluggable
//!   "which device notifies" policy.
//! * [`poll::PollPolicy`] — how often to call [`CalendarSource::sync`](almond_types::calendar::CalendarSource::sync).
//!
//! All sources implement [`almond_types::calendar::CalendarSource`]; the integrator turns a
//! [`CalendarDelta`](almond_types::calendar::CalendarDelta) into `CalendarUpsert` / `CalendarDelete`
//! events in the log (see [`delta_to_event_bodies`]).

pub mod conference;
pub mod google;
pub mod ics;
pub mod oauth;
pub mod phone;
pub mod poll;
pub mod reminder;
mod snapshot;

pub use almond_types as types;

use almond_types::calendar::CalendarDelta;
use almond_types::event::EventBody;
use almond_types::time::{UnixMs, now_ms};
use std::sync::Arc;

pub use google::GoogleCalendar;
pub use ics::IcsSubscription;
pub use oauth::{AccessTokenProvider, GoogleAccount, GoogleAuth, GoogleOAuthConfig};
pub use phone::{PhoneCalendar, PhoneSnapshot};
pub use poll::PollPolicy;
pub use reminder::{
    ActiveDevicePolicy, AlwaysNotify, DeviceActivity, NotifyPolicy, ReminderConfig,
    ReminderScheduler,
};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Time window used for a full sync (`cursor = None`). Default: 30 days back, 90 days ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncWindow {
    pub past_days: u32,
    pub future_days: u32,
}

impl Default for SyncWindow {
    fn default() -> Self {
        SyncWindow {
            past_days: 30,
            future_days: 90,
        }
    }
}

impl SyncWindow {
    /// `(start, end)` in Unix ms around `now`.
    pub fn bounds(&self, now: UnixMs) -> (UnixMs, UnixMs) {
        (
            now - self.past_days as i64 * DAY_MS,
            now + self.future_days as i64 * DAY_MS,
        )
    }
}

/// Injectable clock (tests pin "now").
pub type Clock = Arc<dyn Fn() -> UnixMs + Send + Sync>;

pub(crate) fn system_clock() -> Clock {
    Arc::new(now_ms)
}

/// Convert a delta from source `source_id` into log event bodies (upserts first, then deletes).
pub fn delta_to_event_bodies(source_id: &str, delta: &CalendarDelta) -> Vec<EventBody> {
    let mut out: Vec<EventBody> = delta
        .upserts
        .iter()
        .cloned()
        .map(EventBody::CalendarUpsert)
        .collect();
    out.extend(delta.deletes.iter().map(|id| EventBody::CalendarDelete {
        source: source_id.to_string(),
        event_id: id.clone(),
    }));
    out
}

/// Map a transport error to the shared error type.
pub(crate) fn net_err(e: reqwest::Error) -> almond_types::Error {
    almond_types::Error::Network(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_bounds() {
        let (a, b) = SyncWindow::default().bounds(100 * DAY_MS);
        assert_eq!(a, 70 * DAY_MS);
        assert_eq!(b, 190 * DAY_MS);
    }
}
