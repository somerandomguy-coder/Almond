//! Meeting reminders: "Meeting with X in 5 min — [Start notes]".
//!
//! [`ReminderScheduler::due`] is called with the upcoming events (e.g. every 30 s, or at
//! [`ReminderScheduler::next_check_at`]) and returns the [`MeetingReminder`]s to show now:
//!
//! * fires once per (source, event id, start) — rescheduling an event re-arms it;
//! * fires from `lead_minutes` before start until `grace` after start (so a laptop opened just
//!   as the meeting begins still gets it);
//! * only for events with a video link or other attendees (configurable);
//! * never for all-day, cancelled, or declined-by-me events.
//!
//! Which device shows the notification is a [`NotifyPolicy`]. The default,
//! [`ActiveDevicePolicy`], notifies on the device the user used most recently (from a
//! [`DeviceActivity`] board fed by local input and by peers' `device.info` heartbeats during
//! sync); if no device has been active recently, every device notifies (better twice than never).

use almond_types::DeviceId;
use almond_types::api::{MeetingReminder, UpcomingEvent};
use almond_types::calendar::CalendarEvent;
use almond_types::time::UnixMs;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const MIN_MS: i64 = 60_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderConfig {
    /// Minutes before start. Default 5.
    pub lead_minutes: u32,
    /// Still fire up to this long after start if we missed the window. Default 2 min.
    pub grace_ms: i64,
    /// Only remind for events with a conference link or attendees other than me. Default true.
    pub require_conference_or_attendees: bool,
}

impl Default for ReminderConfig {
    fn default() -> Self {
        ReminderConfig {
            lead_minutes: 5,
            grace_ms: 2 * MIN_MS,
            require_conference_or_attendees: true,
        }
    }
}

/// Decides whether *this* device should show a reminder.
pub trait NotifyPolicy: Send + Sync {
    fn should_notify(&self, event: &CalendarEvent, now: UnixMs) -> bool;
}

/// Every device notifies.
pub struct AlwaysNotify;

impl NotifyPolicy for AlwaysNotify {
    fn should_notify(&self, _: &CalendarEvent, _: UnixMs) -> bool {
        true
    }
}

/// Last-activity timestamps per device (local input, app foreground, peers' heartbeats).
#[derive(Debug, Default)]
pub struct DeviceActivity {
    last: Mutex<HashMap<DeviceId, UnixMs>>,
}

impl DeviceActivity {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record activity (keeps the max timestamp per device).
    pub fn record(&self, device: DeviceId, at: UnixMs) {
        let mut m = self.last.lock().expect("activity");
        let e = m.entry(device).or_insert(at);
        *e = (*e).max(at);
    }

    pub fn snapshot(&self) -> HashMap<DeviceId, UnixMs> {
        self.last.lock().expect("activity").clone()
    }

    /// The most recently active device within `window_ms` of `now` (ties → smallest id).
    pub fn most_active(&self, now: UnixMs, window_ms: i64) -> Option<DeviceId> {
        self.last
            .lock()
            .expect("activity")
            .iter()
            .filter(|(_, t)| **t >= now - window_ms)
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(d, _)| d.clone())
    }
}

/// Notify only on the most recently active device; everywhere if nobody was active lately.
pub struct ActiveDevicePolicy {
    this_device: DeviceId,
    activity: Arc<DeviceActivity>,
    /// How recent activity must be to count. Default 15 minutes.
    pub active_window_ms: i64,
}

impl ActiveDevicePolicy {
    pub fn new(this_device: DeviceId, activity: Arc<DeviceActivity>) -> Self {
        ActiveDevicePolicy {
            this_device,
            activity,
            active_window_ms: 15 * MIN_MS,
        }
    }
}

impl NotifyPolicy for ActiveDevicePolicy {
    fn should_notify(&self, _: &CalendarEvent, now: UnixMs) -> bool {
        match self.activity.most_active(now, self.active_window_ms) {
            None => true,
            Some(d) => d == self.this_device,
        }
    }
}

pub struct ReminderScheduler {
    cfg: ReminderConfig,
    policy: Box<dyn NotifyPolicy>,
    /// dedupe key → event start (for pruning)
    fired: HashMap<String, UnixMs>,
}

impl ReminderScheduler {
    pub fn new(cfg: ReminderConfig, policy: Box<dyn NotifyPolicy>) -> Self {
        ReminderScheduler {
            cfg,
            policy,
            fired: HashMap::new(),
        }
    }

    pub fn config(&self) -> &ReminderConfig {
        &self.cfg
    }

    pub fn set_config(&mut self, cfg: ReminderConfig) {
        self.cfg = cfg;
    }

    fn lead_ms(&self) -> i64 {
        self.cfg.lead_minutes as i64 * MIN_MS
    }

    fn key(ev: &CalendarEvent) -> String {
        format!("{}\u{1f}{}\u{1f}{}", ev.source, ev.event_id, ev.start)
    }

    /// Is this event worth a reminder at all (independent of time and device)?
    pub fn eligible(&self, ev: &CalendarEvent) -> bool {
        if ev.all_day || ev.status == "cancelled" {
            return false;
        }
        if ev
            .attendees
            .iter()
            .any(|a| a.is_self && a.response.as_deref() == Some("declined"))
        {
            return false;
        }
        if self.cfg.require_conference_or_attendees {
            let others = ev.attendees.iter().any(|a| !a.is_self);
            return ev.conference_url.is_some() || others;
        }
        true
    }

    /// Reminders to show now. Marks them fired (also when the policy says another device
    /// notifies, so this device doesn't fire late after the user switches devices).
    pub fn due(&mut self, events: &[CalendarEvent], now: UnixMs) -> Vec<MeetingReminder> {
        self.fired
            .retain(|_, start| *start > now - 24 * 60 * MIN_MS);
        let lead = self.lead_ms();
        let mut out = Vec::new();
        for ev in events {
            if !self.eligible(ev) || now < ev.start - lead || now >= ev.start + self.cfg.grace_ms {
                continue;
            }
            let key = Self::key(ev);
            if self.fired.contains_key(&key) {
                continue;
            }
            self.fired.insert(key, ev.start);
            if self.policy.should_notify(ev, now) {
                out.push(MeetingReminder {
                    event: ev.clone(),
                    minutes_before: self.cfg.lead_minutes,
                });
            }
        }
        out.sort_by_key(|r| r.event.start);
        out
    }

    /// Convenience for the API's `UpcomingEvent` list.
    pub fn due_upcoming(&mut self, events: &[UpcomingEvent], now: UnixMs) -> Vec<MeetingReminder> {
        let evs: Vec<CalendarEvent> = events.iter().map(|u| u.event.clone()).collect();
        self.due(&evs, now)
    }

    /// When the next not-yet-fired reminder becomes due (for timer scheduling).
    pub fn next_check_at(&self, events: &[CalendarEvent], now: UnixMs) -> Option<UnixMs> {
        let lead = self.lead_ms();
        events
            .iter()
            .filter(|e| self.eligible(e) && e.start + self.cfg.grace_ms > now)
            .filter(|e| !self.fired.contains_key(&Self::key(e)))
            .map(|e| (e.start - lead).max(now))
            .min()
    }

    /// Dedupe keys already fired (persist across restarts if desired).
    pub fn fired_keys(&self) -> Vec<(String, UnixMs)> {
        self.fired.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }

    pub fn restore_fired(&mut self, keys: impl IntoIterator<Item = (String, UnixMs)>) {
        self.fired.extend(keys);
    }
}

impl Default for ReminderScheduler {
    fn default() -> Self {
        Self::new(ReminderConfig::default(), Box::new(AlwaysNotify))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::calendar::Attendee;

    fn ev(id: &str, start: UnixMs) -> CalendarEvent {
        CalendarEvent {
            source: "google:me@example.com".into(),
            event_id: id.into(),
            calendar_id: "primary".into(),
            calendar_name: None,
            title: id.into(),
            description: None,
            location: None,
            start,
            end: start + 30 * MIN_MS,
            all_day: false,
            attendees: vec![],
            conference_url: Some("https://meet.google.com/a-b-c".into()),
            conference_app: Some("google_meet".into()),
            organizer: None,
            status: "confirmed".into(),
            updated: 0,
        }
    }

    fn att(email: &str, is_self: bool, response: &str) -> Attendee {
        Attendee {
            email: Some(email.into()),
            name: None,
            response: Some(response.into()),
            is_self,
        }
    }

    #[test]
    fn fires_once_in_window() {
        let mut s = ReminderScheduler::default();
        let t = 1_000 * MIN_MS;
        let evs = vec![ev("a", t)];
        assert!(s.due(&evs, t - 6 * MIN_MS).is_empty());
        assert_eq!(s.next_check_at(&evs, t - 6 * MIN_MS), Some(t - 5 * MIN_MS));
        let r = s.due(&evs, t - 5 * MIN_MS);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].minutes_before, 5);
        assert!(s.due(&evs, t - 4 * MIN_MS).is_empty(), "deduped");
        assert_eq!(s.next_check_at(&evs, t - 4 * MIN_MS), None);
        // rescheduled → fires again
        let moved = vec![ev("a", t + 60 * MIN_MS)];
        assert_eq!(s.due(&moved, t + 56 * MIN_MS).len(), 1);
    }

    #[test]
    fn grace_after_start() {
        let mut s = ReminderScheduler::default();
        let t = 1_000 * MIN_MS;
        assert_eq!(s.due(&[ev("a", t)], t + MIN_MS).len(), 1);
        assert!(s.due(&[ev("b", t)], t + 3 * MIN_MS).is_empty());
    }

    #[test]
    fn eligibility() {
        let s = ReminderScheduler::default();
        let mut solo = ev("solo", 0);
        solo.conference_url = None;
        assert!(!s.eligible(&solo), "no link, no attendees");
        solo.attendees = vec![att("me@example.com", true, "accepted")];
        assert!(!s.eligible(&solo), "only me");
        solo.attendees
            .push(att("bob@example.com", false, "accepted"));
        assert!(s.eligible(&solo));
        let mut declined = ev("d", 0);
        declined.attendees = vec![att("me@example.com", true, "declined")];
        assert!(!s.eligible(&declined));
        let mut cancelled = ev("c", 0);
        cancelled.status = "cancelled".into();
        assert!(!s.eligible(&cancelled));
        let mut all_day = ev("ad", 0);
        all_day.all_day = true;
        assert!(!s.eligible(&all_day));
        let lax = ReminderScheduler::new(
            ReminderConfig {
                require_conference_or_attendees: false,
                ..Default::default()
            },
            Box::new(AlwaysNotify),
        );
        let mut plain = ev("p", 0);
        plain.conference_url = None;
        assert!(lax.eligible(&plain));
    }

    #[test]
    fn configurable_lead() {
        let mut s = ReminderScheduler::new(
            ReminderConfig {
                lead_minutes: 10,
                ..Default::default()
            },
            Box::new(AlwaysNotify),
        );
        let t = 1_000 * MIN_MS;
        let r = s.due(&[ev("a", t)], t - 10 * MIN_MS);
        assert_eq!(r[0].minutes_before, 10);
    }

    #[test]
    fn active_device_policy() {
        let pc = DeviceId("pc".into());
        let laptop = DeviceId("laptop".into());
        let act = Arc::new(DeviceActivity::new());
        let t = 1_000 * MIN_MS;
        let mut on_pc = ReminderScheduler::new(
            ReminderConfig::default(),
            Box::new(ActiveDevicePolicy::new(pc.clone(), act.clone())),
        );
        let mut on_laptop = ReminderScheduler::new(
            ReminderConfig::default(),
            Box::new(ActiveDevicePolicy::new(laptop.clone(), act.clone())),
        );
        let evs = [ev("a", t), ev("b", t + 60 * MIN_MS)];

        // nobody active → both notify
        assert_eq!(on_pc.due(&evs[..1], t - 5 * MIN_MS).len(), 1);
        assert_eq!(on_laptop.due(&evs[..1], t - 5 * MIN_MS).len(), 1);

        // laptop used most recently → only laptop notifies
        let now = t + 55 * MIN_MS;
        act.record(pc.clone(), now - 10 * MIN_MS);
        act.record(laptop.clone(), now - MIN_MS);
        assert!(on_pc.due(&evs, now).is_empty());
        assert_eq!(on_laptop.due(&evs, now).len(), 1);
        assert_eq!(act.most_active(now, 15 * MIN_MS), Some(laptop));
        // old activity doesn't count
        assert_eq!(act.most_active(now + 60 * MIN_MS, 15 * MIN_MS), None);
    }

    #[test]
    fn upcoming_wrapper() {
        let mut s = ReminderScheduler::default();
        let t = 1_000 * MIN_MS;
        let up = vec![UpcomingEvent {
            event: ev("a", t),
            meeting_id: None,
        }];
        assert_eq!(s.due_upcoming(&up, t - MIN_MS).len(), 1);
    }
}
