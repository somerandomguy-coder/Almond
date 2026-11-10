//! Phone calendar source: events read by the Android plugin (WS-G) from `CalendarContract`
//! and pushed into the core as JSON snapshots. This module only defines the shape and the
//! conversion; reading the provider (and the `READ_CALENDAR` permission) is WS-G's job.
//!
//! ## JSON shape pushed by the plugin
//!
//! One snapshot = every instance in a time window (query `CalendarContract.Instances` with
//! `begin`/`end` = the window; default 30 days back, 90 days ahead):
//!
//! ```json
//! {
//!   "window_start": 1790000000000,           // ms, the Instances query begin
//!   "window_end":   1800000000000,           // ms, the Instances query end
//!   "rows": [{
//!     "event_id": 123,                       // Instances.EVENT_ID
//!     "begin": 1793000000000,                // Instances.BEGIN (ms UTC)
//!     "end":   1793001800000,                // Instances.END
//!     "calendar_id": 4,                      // Instances.CALENDAR_ID
//!     "calendar_name": "Work",               // Calendars.CALENDAR_DISPLAY_NAME   (optional)
//!     "owner_account": "me@example.com",     // Calendars.OWNER_ACCOUNT           (optional)
//!     "title": "Standup",                    // Events.TITLE                      (optional)
//!     "description": "…",                    // Events.DESCRIPTION                (optional)
//!     "location": "https://zoom.us/j/1",     // Events.EVENT_LOCATION             (optional)
//!     "all_day": false,                      // Events.ALL_DAY != 0               (default false)
//!     "organizer": "boss@example.com",       // Events.ORGANIZER                  (optional)
//!     "status": 1,                           // Events.STATUS 0 tentative / 1 confirmed / 2 canceled
//!     "self_attendee_status": 1,             // Events.SELF_ATTENDEE_STATUS (Attendees.ATTENDEE_STATUS_*)
//!     "last_modified": 1792000000000,        // optional, ms (not in the provider; plugin may omit)
//!     "attendees": [{                        // Attendees table rows for EVENT_ID (optional)
//!       "email": "bob@example.com",          // ATTENDEE_EMAIL
//!       "name": "Bob",                       // ATTENDEE_NAME
//!       "status": 1,                         // ATTENDEE_STATUS 0 none / 1 accepted / 2 declined / 3 invited / 4 tentative
//!       "relationship": 1                    // ATTENDEE_RELATIONSHIP 0 none / 1 attendee / 2 organizer / 3 performer / 4 speaker
//!     }]
//!   }]
//! }
//! ```
//!
//! Notes: Android stores all-day instances at UTC midnight, which matches Almond's convention.
//! Event ids are `"<event_id>_<begin>"` so recurring instances are distinct. The source id is
//! `"phone:<device id>"`. Events of every account on the phone (Google, Samsung, Exchange, local)
//! arrive in one go; the user may want to disable the Google calendars there if the same account
//! is also connected directly (duplicates are not merged across sources).

use crate::conference;
use crate::snapshot::{SnapshotCursor, diff};
use almond_types::calendar::{
    Attendee, CalendarDelta, CalendarEvent, CalendarSource, CalendarSourceInfo,
};
use almond_types::time::UnixMs;
use almond_types::{DeviceId, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhoneSnapshot {
    pub window_start: UnixMs,
    pub window_end: UnixMs,
    #[serde(default)]
    pub rows: Vec<PhoneEventRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhoneEventRow {
    pub event_id: i64,
    pub begin: UnixMs,
    pub end: UnixMs,
    pub calendar_id: i64,
    #[serde(default)]
    pub calendar_name: Option<String>,
    #[serde(default)]
    pub owner_account: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub organizer: Option<String>,
    #[serde(default)]
    pub status: Option<i32>,
    #[serde(default)]
    pub self_attendee_status: Option<i32>,
    #[serde(default)]
    pub last_modified: Option<UnixMs>,
    #[serde(default)]
    pub attendees: Vec<PhoneAttendeeRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhoneAttendeeRow {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<i32>,
    #[serde(default)]
    pub relationship: Option<i32>,
}

fn attendee_status(s: Option<i32>) -> Option<String> {
    match s? {
        1 => Some("accepted".into()),
        2 => Some("declined".into()),
        3 => Some("needs_action".into()),
        4 => Some("tentative".into()),
        _ => None,
    }
}

impl PhoneEventRow {
    /// Convert one Instances row to the shared model.
    pub fn to_event(&self, source: &str) -> CalendarEvent {
        let owner = self.owner_account.as_deref().map(str::to_ascii_lowercase);
        let mut attendees: Vec<Attendee> = self
            .attendees
            .iter()
            .filter(|a| a.email.is_some() || a.name.is_some())
            .map(|a| {
                let email = a.email.as_deref().map(str::to_ascii_lowercase);
                let is_self = email.is_some() && email == owner;
                Attendee {
                    response: if is_self && self.self_attendee_status.is_some() {
                        attendee_status(self.self_attendee_status)
                    } else {
                        attendee_status(a.status)
                    },
                    email,
                    name: a.name.clone().filter(|n| !n.is_empty()),
                    is_self,
                }
            })
            .collect();
        // Make "declined" visible even if the self row is missing from the attendee list.
        if !attendees.is_empty()
            && !attendees.iter().any(|a| a.is_self)
            && self.self_attendee_status == Some(2)
        {
            attendees.push(Attendee {
                email: owner.clone(),
                name: None,
                response: Some("declined".into()),
                is_self: true,
            });
        }
        let conf = conference::detect_in([self.location.as_deref(), self.description.as_deref()]);
        CalendarEvent {
            source: source.into(),
            event_id: format!("{}_{}", self.event_id, self.begin),
            calendar_id: self.calendar_id.to_string(),
            calendar_name: self.calendar_name.clone(),
            title: self
                .title
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "(No title)".into()),
            description: self.description.clone().filter(|d| !d.is_empty()),
            location: self.location.clone().filter(|l| !l.is_empty()),
            start: self.begin,
            end: self.end.max(self.begin),
            all_day: self.all_day,
            attendees,
            conference_url: conf.as_ref().map(|c| c.0.clone()),
            conference_app: conf.map(|c| c.1.to_string()),
            organizer: self
                .organizer
                .as_deref()
                .filter(|o| o.contains('@'))
                .map(str::to_ascii_lowercase),
            status: match self.status {
                Some(0) => "tentative".into(),
                Some(2) => "cancelled".into(),
                _ => "confirmed".into(),
            },
            updated: self.last_modified.unwrap_or(0),
        }
    }
}

/// The phone's calendars as a [`CalendarSource`]. The plugin calls [`PhoneCalendar::push`] (or
/// [`PhoneCalendar::push_json`]); the next [`CalendarSource::sync`] emits the differences.
pub struct PhoneCalendar {
    device: DeviceId,
    label: String,
    enabled: bool,
    latest: Mutex<Option<PhoneSnapshot>>,
}

impl PhoneCalendar {
    pub fn new(device: DeviceId, label: impl Into<String>) -> Self {
        PhoneCalendar {
            device,
            label: label.into(),
            enabled: true,
            latest: Mutex::new(None),
        }
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// `"phone:<device id>"`
    pub fn source_id(&self) -> String {
        format!("phone:{}", self.device)
    }

    /// Replace the current snapshot.
    pub fn push(&self, snapshot: PhoneSnapshot) {
        *self.latest.lock().expect("phone snapshot") = Some(snapshot);
    }

    /// Parse and push a JSON snapshot (shape in the module docs). Returns the row count.
    pub fn push_json(&self, json: &str) -> Result<usize> {
        let snap: PhoneSnapshot = serde_json::from_str(json)?;
        let n = snap.rows.len();
        self.push(snap);
        Ok(n)
    }
}

#[async_trait]
impl CalendarSource for PhoneCalendar {
    fn info(&self) -> CalendarSourceInfo {
        CalendarSourceInfo {
            id: self.source_id(),
            provider: "phone".into(),
            label: self.label.clone(),
            enabled: self.enabled,
        }
    }

    async fn sync(&self, cursor: Option<String>) -> Result<CalendarDelta> {
        let snap = self.latest.lock().expect("phone snapshot").clone();
        let Some(snap) = snap else {
            // Nothing pushed yet: no changes, keep the cursor.
            return Ok(CalendarDelta {
                next_cursor: cursor,
                ..Default::default()
            });
        };
        let prev = SnapshotCursor::parse(cursor.as_deref());
        let source = self.source_id();
        let events: Vec<CalendarEvent> = snap
            .rows
            .iter()
            .filter(|r| r.status != Some(2))
            .map(|r| r.to_event(&source))
            .collect();
        let (mut delta, next) = diff(&prev, events, snap.window_start, snap.window_end);
        delta.next_cursor = Some(next.to_json()?);
        Ok(delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAP: &str = r#"{
      "window_start": 0, "window_end": 1000000000,
      "rows": [
        {"event_id": 7, "begin": 1000, "end": 2000, "calendar_id": 1, "calendar_name": "Work",
         "owner_account": "Me@Example.com", "title": "Sync", "location": "https://zoom.us/j/42",
         "status": 1, "self_attendee_status": 2,
         "attendees": [
            {"email": "me@example.com", "status": 1, "relationship": 1},
            {"email": "bob@example.com", "name": "Bob", "status": 4, "relationship": 2}
         ]},
        {"event_id": 8, "begin": 86400000, "end": 172800000, "calendar_id": 2, "all_day": true},
        {"event_id": 9, "begin": 5000, "end": 6000, "calendar_id": 1, "status": 2}
      ]
    }"#;

    #[tokio::test]
    async fn converts_and_diffs() {
        let p = PhoneCalendar::new(DeviceId("dev1".into()), "Galaxy A26");
        // nothing pushed yet
        let d0 = p.sync(Some("c".into())).await.unwrap();
        assert!(d0.upserts.is_empty());
        assert_eq!(d0.next_cursor.as_deref(), Some("c"));

        assert_eq!(p.push_json(SNAP).unwrap(), 3);
        let d1 = p.sync(None).await.unwrap();
        assert_eq!(d1.upserts.len(), 2, "cancelled row skipped");
        let e = &d1.upserts[0];
        assert_eq!(e.source, "phone:dev1");
        assert_eq!(e.event_id, "7_1000");
        assert_eq!(e.calendar_id, "1");
        assert_eq!(e.conference_app.as_deref(), Some("zoom"));
        assert!(e.attendees[0].is_self);
        assert_eq!(e.attendees[0].response.as_deref(), Some("declined"));
        assert_eq!(e.attendees[1].response.as_deref(), Some("tentative"));
        assert!(d1.upserts[1].all_day);
        assert_eq!(d1.upserts[1].title, "(No title)");

        // second snapshot: event 8 gone, event 7 unchanged
        let mut snap: PhoneSnapshot = serde_json::from_str(SNAP).unwrap();
        snap.rows.retain(|r| r.event_id == 7);
        p.push(snap);
        let d2 = p.sync(d1.next_cursor).await.unwrap();
        assert!(d2.upserts.is_empty());
        assert_eq!(d2.deletes, vec!["8_86400000".to_string()]);
    }
}
