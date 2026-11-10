//! Google Calendar source: `calendarList` + `events.list` (`singleEvents=true`) per calendar,
//! with per-calendar `syncToken`s for incremental sync.
//!
//! * `sync(None)` lists all *selected* calendars (those shown in Google Calendar's UI, plus the
//!   primary one) and fetches events in the [`SyncWindow`] (default −30 / +90 days).
//! * `sync(Some(cursor))` uses each calendar's `syncToken`; HTTP 410 (token expired) falls back
//!   to a full sync of that calendar; calendars that newly appear are fully synced.
//! * Cancelled events/instances in an incremental response become `deletes`.
//! * The cursor is a small JSON document (`{"v":1,"calendars":{"<calendarId>":"<syncToken>"}}`);
//!   callers should treat it as opaque.
//!
//! Event ids are Google's (instance ids for recurring events, e.g. `abc123_20261110T090000Z`).
//! All-day events start/end at UTC midnight of their dates (end exclusive).

use crate::conference::{self, GOOGLE_MEET};
use crate::oauth::{AccessTokenProvider, authorized_get, json_or_error};
use crate::{Clock, SyncWindow, system_clock};
use almond_types::calendar::{
    Attendee, CalendarDelta, CalendarEvent, CalendarSource, CalendarSourceInfo,
};
use almond_types::time::UnixMs;
use almond_types::{Error, Result};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_API_BASE: &str = "https://www.googleapis.com/calendar/v3";

/// One Google account's calendars as a [`CalendarSource`].
pub struct GoogleCalendar {
    account: String,
    auth: Arc<dyn AccessTokenProvider>,
    http: reqwest::Client,
    api_base: String,
    window: SyncWindow,
    clock: Clock,
    enabled: bool,
}

impl GoogleCalendar {
    pub fn new(account: impl Into<String>, auth: Arc<dyn AccessTokenProvider>) -> Self {
        GoogleCalendar {
            account: account.into().to_ascii_lowercase(),
            auth,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            api_base: DEFAULT_API_BASE.into(),
            window: SyncWindow::default(),
            clock: system_clock(),
            enabled: true,
        }
    }

    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into().trim_end_matches('/').to_string();
        self
    }
    pub fn with_window(mut self, w: SyncWindow) -> Self {
        self.window = w;
        self
    }
    pub fn with_clock(mut self, c: Clock) -> Self {
        self.clock = c;
        self
    }
    pub fn with_http(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// `"google:<email>"`
    pub fn source_id(&self) -> String {
        format!("google:{}", self.account)
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    /// `api_base` + percent-encoded path segments.
    fn url(&self, segments: &[&str]) -> Result<url::Url> {
        let mut u = url::Url::parse(&self.api_base)
            .map_err(|e| Error::Invalid(format!("calendar url: {e}")))?;
        u.path_segments_mut()
            .map_err(|_| Error::Invalid("calendar api base cannot be a base".into()))?
            .pop_if_empty()
            .extend(segments);
        Ok(u)
    }

    async fn list_calendars(&self) -> Result<Vec<GCalendarListEntry>> {
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut u = self.url(&["users", "me", "calendarList"])?;
            {
                let mut q = u.query_pairs_mut();
                q.append_pair("maxResults", "250");
                if let Some(p) = &page {
                    q.append_pair("pageToken", p);
                }
            }
            let resp = authorized_get(&self.http, self.auth.as_ref(), &self.account, u).await?;
            let list: GCalendarList = json_or_error(resp).await?;
            out.extend(list.items.into_iter().filter(|c| {
                !c.deleted
                    && (c.primary || c.selected)
                    && c.id.as_deref().is_some_and(|s| !s.is_empty())
            }));
            match list.next_page_token {
                Some(p) => page = Some(p),
                None => break,
            }
        }
        Ok(out)
    }

    /// Fetch one calendar. `sync_token = None` → windowed full fetch.
    async fn fetch_events(
        &self,
        cal_id: &str,
        sync_token: Option<&str>,
    ) -> std::result::Result<(Vec<GEvent>, Option<String>), FetchError> {
        let (tmin, tmax) = self.window.bounds((self.clock)());
        let mut items = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut u = self
                .url(&["calendars", cal_id, "events"])
                .map_err(FetchError::Other)?;
            {
                let mut q = u.query_pairs_mut();
                q.append_pair("singleEvents", "true")
                    .append_pair("maxResults", "250");
                match sync_token {
                    Some(t) => {
                        q.append_pair("syncToken", t);
                    }
                    None => {
                        q.append_pair("timeMin", &rfc3339(tmin))
                            .append_pair("timeMax", &rfc3339(tmax))
                            .append_pair("showDeleted", "false");
                    }
                }
                if let Some(p) = &page {
                    q.append_pair("pageToken", p);
                }
            }
            let resp = authorized_get(&self.http, self.auth.as_ref(), &self.account, u)
                .await
                .map_err(FetchError::Other)?;
            if resp.status() == reqwest::StatusCode::GONE {
                return Err(FetchError::Gone);
            }
            let page_data: GEventsPage = json_or_error(resp).await.map_err(FetchError::Other)?;
            items.extend(page_data.items);
            match page_data.next_page_token {
                Some(p) => page = Some(p),
                None => return Ok((items, page_data.next_sync_token)),
            }
        }
    }
}

enum FetchError {
    /// 410: sync token expired → full resync.
    Gone,
    Other(Error),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cursor {
    v: u32,
    /// calendarId → syncToken
    calendars: BTreeMap<String, String>,
}

#[async_trait]
impl CalendarSource for GoogleCalendar {
    fn info(&self) -> CalendarSourceInfo {
        CalendarSourceInfo {
            id: self.source_id(),
            provider: "google".into(),
            label: self.account.clone(),
            enabled: self.enabled,
        }
    }

    async fn sync(&self, cursor: Option<String>) -> Result<CalendarDelta> {
        let prev: Cursor = cursor
            .as_deref()
            .and_then(|c| serde_json::from_str(c).ok())
            .unwrap_or_default();
        let calendars = self.list_calendars().await?;
        let source = self.source_id();
        let mut next = Cursor {
            v: 1,
            calendars: BTreeMap::new(),
        };
        let mut upserts: HashMap<String, CalendarEvent> = HashMap::new();
        let mut deletes: Vec<String> = Vec::new();

        for cal in &calendars {
            let cal_id = cal.id.clone().unwrap_or_default();
            let name = cal.summary_override.clone().or_else(|| cal.summary.clone());
            let token = prev.calendars.get(&cal_id).map(String::as_str);
            let (items, next_token, incremental) = match self.fetch_events(&cal_id, token).await {
                Ok((i, t)) => (i, t, token.is_some()),
                Err(FetchError::Gone) => match self.fetch_events(&cal_id, None).await {
                    Ok((i, t)) => (i, t, false),
                    Err(FetchError::Gone) => {
                        return Err(Error::Network("calendar returned 410 on full sync".into()));
                    }
                    Err(FetchError::Other(e)) => return Err(e),
                },
                Err(FetchError::Other(e)) => return Err(e),
            };
            for ge in items {
                let cancelled = ge.status.as_deref() == Some("cancelled");
                if cancelled {
                    if incremental && let Some(id) = &ge.id {
                        deletes.push(id.clone());
                    }
                    continue;
                }
                if let Some(ev) = normalize(&ge, &source, &cal_id, name.as_deref()) {
                    upserts.insert(ev.event_id.clone(), ev);
                }
            }
            if let Some(t) = next_token {
                next.calendars.insert(cal_id, t);
            }
        }
        deletes.retain(|d| !upserts.contains_key(d));
        deletes.sort();
        deletes.dedup();
        let mut upserts: Vec<CalendarEvent> = upserts.into_values().collect();
        upserts.sort_by(|a, b| a.start.cmp(&b.start).then(a.event_id.cmp(&b.event_id)));
        Ok(CalendarDelta {
            upserts,
            deletes,
            next_cursor: Some(serde_json::to_string(&next)?),
        })
    }
}

// ---------------------------------------------------------------- normalization

/// Convert a Google event to the shared model. Returns `None` for events Almond ignores
/// (working-location markers, events without start time).
pub(crate) fn normalize(
    ge: &GEvent,
    source: &str,
    calendar_id: &str,
    calendar_name: Option<&str>,
) -> Option<CalendarEvent> {
    if matches!(ge.event_type.as_deref(), Some("workingLocation")) {
        return None;
    }
    let id = ge.id.clone()?;
    let (start, all_day) = ge.start.as_ref()?.to_ms()?;
    let end = ge
        .end
        .as_ref()
        .and_then(|e| e.to_ms())
        .map(|(e, _)| e)
        .unwrap_or(start);

    let attendees: Vec<Attendee> = ge
        .attendees
        .iter()
        .filter(|a| !a.resource)
        .map(|a| Attendee {
            email: a.email.clone().map(|e| e.to_ascii_lowercase()),
            name: a.display_name.clone(),
            response: a.response_status.as_deref().map(map_response),
            is_self: a.is_self,
        })
        .collect();

    let (conference_url, conference_app) = conference_of(ge)
        .map(|(u, a)| (Some(u), Some(a.to_string())))
        .unwrap_or((None, None));

    Some(CalendarEvent {
        source: source.to_string(),
        event_id: id,
        calendar_id: calendar_id.to_string(),
        calendar_name: calendar_name.map(str::to_string),
        title: ge
            .summary
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "(No title)".into()),
        description: ge.description.clone(),
        location: ge.location.clone(),
        start,
        end,
        all_day,
        attendees,
        conference_url,
        conference_app,
        organizer: ge.organizer.as_ref().and_then(|o| o.email.clone()),
        status: ge.status.clone().unwrap_or_else(|| "confirmed".into()),
        updated: ge.updated.as_deref().and_then(parse_rfc3339).unwrap_or(0),
    })
}

fn conference_of(ge: &GEvent) -> Option<(String, &'static str)> {
    if let Some(cd) = &ge.conference_data {
        let video = cd
            .entry_points
            .iter()
            .find(|e| e.entry_point_type.as_deref() == Some("video"))
            .and_then(|e| e.uri.clone());
        if let Some(uri) = video {
            let key = cd
                .conference_solution
                .as_ref()
                .and_then(|s| s.key.as_ref())
                .and_then(|k| k.kind.as_deref());
            let app = match key {
                Some("hangoutsMeet") | Some("eventHangout") | Some("eventNamedHangout") => {
                    Some(GOOGLE_MEET)
                }
                _ => conference::classify_url(&uri),
            };
            return Some((uri, app.unwrap_or("other")));
        }
    }
    if let Some(h) = &ge.hangout_link {
        return Some((h.clone(), GOOGLE_MEET));
    }
    conference::detect_in([ge.location.as_deref(), ge.description.as_deref()])
}

fn map_response(s: &str) -> String {
    match s {
        "needsAction" => "needs_action".into(),
        other => other.to_string(),
    }
}

fn rfc3339(ms: UnixMs) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn parse_rfc3339(s: &str) -> Option<UnixMs> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp_millis())
}

// ---------------------------------------------------------------- wire types

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GCalendarList {
    #[serde(default)]
    items: Vec<GCalendarListEntry>,
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GCalendarListEntry {
    id: Option<String>,
    summary: Option<String>,
    summary_override: Option<String>,
    #[serde(default)]
    selected: bool,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    deleted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GEventsPage {
    #[serde(default)]
    items: Vec<GEvent>,
    next_page_token: Option<String>,
    next_sync_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GEvent {
    id: Option<String>,
    status: Option<String>,
    summary: Option<String>,
    description: Option<String>,
    location: Option<String>,
    start: Option<GTime>,
    end: Option<GTime>,
    #[serde(default)]
    attendees: Vec<GAttendee>,
    organizer: Option<GPerson>,
    hangout_link: Option<String>,
    conference_data: Option<GConferenceData>,
    updated: Option<String>,
    event_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GTime {
    date: Option<String>,
    date_time: Option<String>,
}

impl GTime {
    /// (ms, all_day)
    fn to_ms(&self) -> Option<(UnixMs, bool)> {
        if let Some(dt) = &self.date_time {
            return parse_rfc3339(dt).map(|m| (m, false));
        }
        let d = NaiveDate::parse_from_str(self.date.as_deref()?, "%Y-%m-%d").ok()?;
        Some((d.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis(), true))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GAttendee {
    email: Option<String>,
    display_name: Option<String>,
    response_status: Option<String>,
    #[serde(default, rename = "self")]
    is_self: bool,
    #[serde(default)]
    resource: bool,
}

#[derive(Debug, Deserialize)]
struct GPerson {
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GConferenceData {
    #[serde(default)]
    entry_points: Vec<GEntryPoint>,
    conference_solution: Option<GConferenceSolution>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GEntryPoint {
    entry_point_type: Option<String>,
    uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GConferenceSolution {
    key: Option<GSolutionKey>,
}

#[derive(Debug, Deserialize)]
struct GSolutionKey {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ge(v: serde_json::Value) -> GEvent {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn normalizes_meet_event() {
        let e = ge(serde_json::json!({
            "id": "abc_20261110T090000Z",
            "status": "confirmed",
            "summary": "Standup",
            "start": {"dateTime": "2026-11-10T09:00:00+07:00"},
            "end": {"dateTime": "2026-11-10T09:15:00+07:00"},
            "updated": "2026-11-01T10:00:00.000Z",
            "organizer": {"email": "boss@example.com"},
            "attendees": [
                {"email": "Me@Example.com", "self": true, "responseStatus": "needsAction"},
                {"email": "room@resource.calendar.google.com", "resource": true},
                {"email": "bob@example.com", "displayName": "Bob", "responseStatus": "accepted"}
            ],
            "conferenceData": {
                "entryPoints": [
                    {"entryPointType": "phone", "uri": "tel:+1-555"},
                    {"entryPointType": "video", "uri": "https://meet.google.com/abc-defg-hij"}
                ],
                "conferenceSolution": {"key": {"type": "hangoutsMeet"}}
            }
        }));
        let ev = normalize(&e, "google:me@example.com", "primary", Some("Me")).unwrap();
        assert_eq!(ev.start, parse_rfc3339("2026-11-10T02:00:00Z").unwrap());
        assert_eq!(ev.end - ev.start, 15 * 60 * 1000);
        assert!(!ev.all_day);
        assert_eq!(ev.attendees.len(), 2);
        assert!(ev.attendees[0].is_self);
        assert_eq!(ev.attendees[0].email.as_deref(), Some("me@example.com"));
        assert_eq!(ev.attendees[0].response.as_deref(), Some("needs_action"));
        assert_eq!(ev.conference_app.as_deref(), Some("google_meet"));
        assert_eq!(
            ev.conference_url.as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
        assert_eq!(ev.organizer.as_deref(), Some("boss@example.com"));
        assert!(ev.updated > 0);
    }

    #[test]
    fn all_day_and_zoom_in_description() {
        let e = ge(serde_json::json!({
            "id": "x", "summary": " ",
            "start": {"date": "2026-11-10"}, "end": {"date": "2026-11-11"},
            "description": "Join <a href=\"https://zoom.us/j/123\">here</a>"
        }));
        let ev = normalize(&e, "s", "c", None).unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.end - ev.start, 24 * 3600 * 1000);
        assert_eq!(ev.title, "(No title)");
        assert_eq!(ev.conference_app.as_deref(), Some("zoom"));
        assert_eq!(ev.status, "confirmed");
    }

    #[test]
    fn skips_working_location() {
        let e = ge(serde_json::json!({
            "id": "w", "eventType": "workingLocation",
            "start": {"date": "2026-11-10"}, "end": {"date": "2026-11-11"}
        }));
        assert!(normalize(&e, "s", "c", None).is_none());
    }

    #[test]
    fn calendar_id_is_one_path_segment() {
        let g = GoogleCalendar::new("a", Arc::new(crate::oauth::StaticTokenProvider("t".into())));
        let u = g.url(&["calendars", "a b/c#d@x.com", "events"]).unwrap();
        assert_eq!(
            u.as_str(),
            "https://www.googleapis.com/calendar/v3/calendars/a%20b%2Fc%23d@x.com/events"
        );
    }
}
