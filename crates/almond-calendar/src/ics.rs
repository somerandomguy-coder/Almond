//! iCalendar (`.ics`) subscriptions: fetch over HTTP(S) (`webcal://` accepted) or read a local
//! `file://` URL / plain path (an exported calendar), parse `VEVENT`s,
//! expand recurrences (`RRULE`/`RDATE`/`EXDATE`, overrides via `RECURRENCE-ID`) inside the
//! [`SyncWindow`], and emit only what changed since the cursor (see [`crate::snapshot`]).
//!
//! * Source id: `"ics:<blake3 hex of the normalized URL>"`.
//! * Event ids: `UID` for single events; `"<UID>_<YYYYMMDDTHHMMSSZ>"` (or `_<YYYYMMDD>` for all-day)
//!   of the *original* start for recurrence instances, so moved instances keep their id.
//! * Time zones: IANA `TZID`s (also Mozilla-style `/mozilla.org/.../Europe/Berlin`) and common
//!   Windows names; unknown zones and floating times fall back to `X-WR-TIMEZONE`, then UTC.
//! * ICS has no notion of "me": pass the user's addresses via [`IcsSubscription::with_self_emails`]
//!   so `Attendee::is_self` and declined detection work.

use crate::conference;
use crate::snapshot::{SnapshotCursor, diff};
use crate::{Clock, SyncWindow, net_err, system_clock};
use almond_types::calendar::{
    Attendee, CalendarDelta, CalendarEvent, CalendarSource, CalendarSourceInfo,
};
use almond_types::time::UnixMs;
use almond_types::{Error, Result};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use rrule::{RRule, RRuleSet, Tz, Unvalidated};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// Safety cap on instances per recurring event inside one window.
const MAX_INSTANCES: u16 = 1000;
const DAY_MS: i64 = 86_400_000;

/// A subscribed `.ics` feed.
pub struct IcsSubscription {
    url: String,
    id: String,
    label: String,
    http: reqwest::Client,
    window: SyncWindow,
    clock: Clock,
    self_emails: Vec<String>,
    enabled: bool,
}

impl IcsSubscription {
    /// `label` is shown in the UI (defaults to the host name).
    pub fn new(url: &str, label: Option<String>) -> Result<Self> {
        let url = normalize_url(url)?;
        let label = label.unwrap_or_else(|| {
            url::Url::parse(&url)
                .ok()
                .and_then(|u| {
                    u.host_str()
                        .filter(|h| !h.is_empty())
                        .map(str::to_string)
                        .or_else(|| u.path_segments()?.next_back().map(str::to_string))
                })
                .unwrap_or_else(|| "Calendar feed".into())
        });
        Ok(IcsSubscription {
            id: format!("ics:{}", blake3::hash(url.as_bytes()).to_hex()),
            url,
            label,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            window: SyncWindow::default(),
            clock: system_clock(),
            self_emails: Vec::new(),
            enabled: true,
        })
    }

    pub fn with_self_emails(mut self, emails: impl IntoIterator<Item = String>) -> Self {
        self.self_emails = emails.into_iter().map(|e| e.to_ascii_lowercase()).collect();
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
    pub fn url(&self) -> &str {
        &self.url
    }
}

fn normalize_url(url: &str) -> Result<String> {
    let url = url.trim();
    // a plain local path ("C:\\cal.ics", "/home/me/cal.ics") → file:// URL
    let path = std::path::Path::new(url);
    if !url.contains("://") && path.is_absolute() {
        return url::Url::from_file_path(path)
            .map(|u| u.to_string())
            .map_err(|_| Error::Invalid(format!("ics path {url:?}")));
    }
    let url = if let Some(rest) = url.strip_prefix("webcal://") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("webcals://") {
        format!("https://{rest}")
    } else {
        url.to_string()
    };
    let parsed = url::Url::parse(&url).map_err(|e| Error::Invalid(format!("ics url: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https" | "file") {
        return Err(Error::Invalid(format!(
            "unsupported ics url scheme: {}",
            parsed.scheme()
        )));
    }
    Ok(parsed.to_string())
}

#[async_trait]
impl CalendarSource for IcsSubscription {
    fn info(&self) -> CalendarSourceInfo {
        CalendarSourceInfo {
            id: self.id.clone(),
            provider: "ics".into(),
            label: self.label.clone(),
            enabled: self.enabled,
        }
    }

    async fn sync(&self, cursor: Option<String>) -> Result<CalendarDelta> {
        let prev = SnapshotCursor::parse(cursor.as_deref());
        if self.url.starts_with("file:") {
            let path = url::Url::parse(&self.url)
                .ok()
                .and_then(|u| u.to_file_path().ok())
                .ok_or_else(|| Error::Invalid(format!("ics url {}", self.url)))?;
            let body = std::fs::read_to_string(&path).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    Error::NotFound(format!("ics file {}", path.display()))
                }
                _ => Error::Io(e),
            })?;
            return self.delta_from(&prev, &body, None);
        }
        let mut req = self.http.get(&self.url);
        if let Some(etag) = &prev.etag {
            req = req.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let resp = req.send().await.map_err(net_err)?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_MODIFIED && cursor.is_some() {
            return Ok(CalendarDelta {
                next_cursor: cursor,
                ..Default::default()
            });
        }
        if !status.is_success() {
            let msg = format!("ics feed {status}");
            return Err(match status.as_u16() {
                401 | 403 => Error::PermissionDenied(msg),
                404 | 410 => Error::NotFound(msg),
                429 | 500..=599 => Error::Network(msg),
                _ => Error::Other(msg),
            });
        }
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp.text().await.map_err(net_err)?;
        self.delta_from(&prev, &body, etag)
    }
}

impl IcsSubscription {
    fn delta_from(
        &self,
        prev: &SnapshotCursor,
        body: &str,
        etag: Option<String>,
    ) -> Result<CalendarDelta> {
        let cal = parse_calendar(body)?;
        let (ws, we) = self.window.bounds((self.clock)());
        let events = cal.expand(
            &self.id,
            &self.id,
            Some(cal.name.as_deref().unwrap_or(&self.label)),
            &self.self_emails,
            ws,
            we,
        );
        let (mut delta, mut next) = diff(prev, events, ws, we);
        next.etag = etag;
        delta.next_cursor = Some(next.to_json()?);
        Ok(delta)
    }
}

// ---------------------------------------------------------------- content lines

#[derive(Debug, Clone)]
struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn param(&self, k: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.as_str())
    }
}

fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(cont) = line.strip_prefix([' ', '\t'])
            && let Some(last) = out.last_mut()
        {
            last.push_str(cont);
            continue;
        }
        if !line.is_empty() {
            out.push(line.to_string());
        }
    }
    out
}

fn parse_line(line: &str) -> Option<Prop> {
    // Find the first ':' that is not inside a quoted parameter value.
    let mut in_q = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_q = !in_q,
            ':' if !in_q => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = split_unquoted(head, ';').into_iter();
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((
                k.trim().to_ascii_uppercase(),
                v.trim().trim_matches('"').to_string(),
            ))
        })
        .collect();
    Some(Prop {
        name,
        params,
        value: value.to_string(),
    })
}

fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut in_q = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c == '"' {
            in_q = !in_q;
        } else if c == sep && !in_q {
            out.push(&s[start..i]);
            start = i + 1;
        }
    }
    out.push(&s[start..]);
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------- parsed model

#[derive(Debug, Clone)]
struct VEvent {
    uid: String,
    summary: Option<String>,
    description: Option<String>,
    location: Option<String>,
    url: Option<String>,
    start: DateTime<Tz>,
    all_day: bool,
    end: Option<DateTime<Tz>>,
    duration_ms: Option<i64>,
    status: String,
    organizer: Option<String>,
    attendees: Vec<(Option<String>, Option<String>, Option<String>)>,
    rrules: Vec<String>,
    rdates: Vec<DateTime<Tz>>,
    exdates: Vec<DateTime<Tz>>,
    recurrence_id: Option<DateTime<Tz>>,
    updated: UnixMs,
}

impl VEvent {
    fn duration(&self) -> i64 {
        if let Some(e) = &self.end {
            return (e.timestamp_millis() - self.start.timestamp_millis()).max(0);
        }
        if let Some(d) = self.duration_ms {
            return d.max(0);
        }
        if self.all_day { DAY_MS } else { 0 }
    }
}

/// A parsed feed.
#[derive(Debug, Default)]
pub struct IcsCalendar {
    pub name: Option<String>,
    events: Vec<VEvent>,
}

/// Parse iCalendar text. Malformed events are skipped; a body without `BEGIN:VCALENDAR` is an error.
pub fn parse_calendar(text: &str) -> Result<IcsCalendar> {
    let lines = unfold(text.trim_start_matches('\u{feff}'));
    if !lines
        .iter()
        .any(|l| l.trim().eq_ignore_ascii_case("BEGIN:VCALENDAR"))
    {
        return Err(Error::Invalid("not an iCalendar feed".into()));
    }
    let props: Vec<Prop> = lines.iter().filter_map(|l| parse_line(l)).collect();
    let mut cal = IcsCalendar::default();
    let mut default_tz: Option<chrono_tz::Tz> = None;
    for p in &props {
        match p.name.as_str() {
            "X-WR-CALNAME" if cal.name.is_none() => cal.name = Some(unescape(&p.value)),
            "X-WR-TIMEZONE" => default_tz = resolve_tz(&p.value),
            _ => {}
        }
    }
    // Collect top-level VEVENT property lists (skipping nested VALARM etc.).
    let mut stack: Vec<String> = Vec::new();
    let mut current: Option<Vec<Prop>> = None;
    for p in props {
        match p.name.as_str() {
            "BEGIN" => {
                let comp = p.value.trim().to_ascii_uppercase();
                if comp == "VEVENT" && stack.last().map(String::as_str) == Some("VCALENDAR") {
                    current = Some(Vec::new());
                }
                stack.push(comp);
            }
            "END" => {
                let comp = p.value.trim().to_ascii_uppercase();
                if comp == "VEVENT"
                    && stack.len() >= 2
                    && stack[stack.len() - 2] == "VCALENDAR"
                    && let Some(ps) = current.take()
                    && let Some(ev) = build_event(&ps, default_tz)
                {
                    cal.events.push(ev);
                }
                stack.pop();
            }
            _ => {
                if stack.last().map(String::as_str) == Some("VEVENT")
                    && let Some(c) = current.as_mut()
                {
                    c.push(p);
                }
            }
        }
    }
    Ok(cal)
}

fn build_event(props: &[Prop], default_tz: Option<chrono_tz::Tz>) -> Option<VEvent> {
    let get = |n: &str| props.iter().find(|p| p.name == n);
    let text = |n: &str| {
        get(n)
            .map(|p| unescape(&p.value))
            .filter(|s| !s.trim().is_empty())
    };
    let dtstart = get("DTSTART")?;
    let (start, all_day) = parse_dt(&dtstart.value, dtstart, default_tz)?;
    let end = get("DTEND").and_then(|p| parse_dt(&p.value, p, default_tz).map(|d| d.0));
    let mut rdates = Vec::new();
    let mut exdates = Vec::new();
    let mut rrules = Vec::new();
    let mut attendees = Vec::new();
    for p in props {
        match p.name.as_str() {
            "RRULE" => rrules.push(p.value.trim().to_string()),
            "RDATE" | "EXDATE" => {
                if p.param("VALUE")
                    .is_some_and(|v| v.eq_ignore_ascii_case("PERIOD"))
                {
                    continue;
                }
                let list = if p.name == "RDATE" {
                    &mut rdates
                } else {
                    &mut exdates
                };
                list.extend(
                    p.value
                        .split(',')
                        .filter_map(|v| parse_dt(v, p, default_tz).map(|d| d.0)),
                );
            }
            "ATTENDEE" => attendees.push((
                mailto(&p.value),
                p.param("CN").map(str::to_string),
                p.param("PARTSTAT").map(map_partstat),
            )),
            _ => {}
        }
    }
    let updated = get("LAST-MODIFIED")
        .or_else(|| get("DTSTAMP"))
        .and_then(|p| parse_dt(&p.value, p, None))
        .map(|d| d.0.timestamp_millis())
        .unwrap_or(0);
    Some(VEvent {
        uid: get("UID")
            .map(|p| p.value.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("nouid-{}", start.timestamp_millis())),
        summary: text("SUMMARY"),
        description: text("DESCRIPTION"),
        location: text("LOCATION"),
        url: get("URL").map(|p| p.value.trim().to_string()),
        start,
        all_day,
        end,
        duration_ms: get("DURATION").and_then(|p| parse_duration(&p.value)),
        status: get("STATUS")
            .map(|p| p.value.trim().to_ascii_lowercase())
            .unwrap_or_else(|| "confirmed".into()),
        organizer: get("ORGANIZER").and_then(|p| mailto(&p.value)),
        attendees,
        rrules,
        rdates,
        exdates,
        recurrence_id: get("RECURRENCE-ID")
            .and_then(|p| parse_dt(&p.value, p, default_tz).map(|d| d.0)),
        updated,
    })
}

fn mailto(v: &str) -> Option<String> {
    let v = v.trim();
    let v = v
        .strip_prefix("mailto:")
        .or_else(|| v.strip_prefix("MAILTO:"))
        .unwrap_or(v);
    (v.contains('@')).then(|| v.to_ascii_lowercase())
}

fn map_partstat(s: &str) -> String {
    match s.to_ascii_uppercase().as_str() {
        "ACCEPTED" => "accepted".into(),
        "DECLINED" => "declined".into(),
        "TENTATIVE" => "tentative".into(),
        _ => "needs_action".into(),
    }
}

// ---------------------------------------------------------------- dates

fn utc() -> Tz {
    Tz::Tz(chrono_tz::UTC)
}

/// Parse a DATE or DATE-TIME value. Returns (instant, is_date).
fn parse_dt(
    value: &str,
    prop: &Prop,
    default_tz: Option<chrono_tz::Tz>,
) -> Option<(DateTime<Tz>, bool)> {
    let v = value.trim();
    let is_date = prop
        .param("VALUE")
        .is_some_and(|x| x.eq_ignore_ascii_case("DATE"))
        || (v.len() == 8 && v.bytes().all(|b| b.is_ascii_digit()));
    if is_date {
        let d = NaiveDate::parse_from_str(&v[..8.min(v.len())], "%Y%m%d").ok()?;
        return Some((utc().from_utc_datetime(&d.and_hms_opt(0, 0, 0)?), true));
    }
    if let Some(z) = v.strip_suffix('Z').or_else(|| v.strip_suffix('z')) {
        let n = NaiveDateTime::parse_from_str(z, "%Y%m%dT%H%M%S").ok()?;
        return Some((utc().from_utc_datetime(&n), false));
    }
    let n = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
    let tz = prop
        .param("TZID")
        .and_then(resolve_tz)
        .or(default_tz)
        .map(Tz::Tz)
        .unwrap_or_else(utc);
    let local = tz
        .from_local_datetime(&n)
        .earliest()
        // in a DST gap: the wall-clock time doesn't exist; shift forward an hour
        .or_else(|| {
            tz.from_local_datetime(&(n + chrono::Duration::hours(1)))
                .earliest()
        })?;
    Some((local, false))
}

/// Resolve a TZID to an IANA zone.
pub(crate) fn resolve_tz(tzid: &str) -> Option<chrono_tz::Tz> {
    let t = tzid.trim().trim_matches('"');
    if let Ok(z) = t.parse::<chrono_tz::Tz>() {
        return Some(z);
    }
    // "/mozilla.org/20050126_1/Europe/Berlin", "/softwarestudio.org/Olson_20011030_5/America/New_York"
    let segs: Vec<&str> = t.split('/').filter(|s| !s.is_empty()).collect();
    for i in 0..segs.len() {
        if let Ok(z) = segs[i..].join("/").parse::<chrono_tz::Tz>() {
            return Some(z);
        }
    }
    let iana = match t {
        "SE Asia Standard Time" => "Asia/Bangkok",
        "Pacific Standard Time" => "America/Los_Angeles",
        "Mountain Standard Time" => "America/Denver",
        "Central Standard Time" => "America/Chicago",
        "Eastern Standard Time" => "America/New_York",
        "GMT Standard Time" => "Europe/London",
        "W. Europe Standard Time" => "Europe/Berlin",
        "Romance Standard Time" => "Europe/Paris",
        "Central Europe Standard Time" => "Europe/Budapest",
        "Central European Standard Time" => "Europe/Warsaw",
        "E. Europe Standard Time" => "Europe/Chisinau",
        "FLE Standard Time" => "Europe/Kiev",
        "Russian Standard Time" => "Europe/Moscow",
        "India Standard Time" => "Asia/Kolkata",
        "China Standard Time" => "Asia/Shanghai",
        "Singapore Standard Time" => "Asia/Singapore",
        "Tokyo Standard Time" => "Asia/Tokyo",
        "Korea Standard Time" => "Asia/Seoul",
        "AUS Eastern Standard Time" => "Australia/Sydney",
        "UTC" | "Coordinated Universal Time" | "GMT" => "UTC",
        _ => return None,
    };
    iana.parse().ok()
}

/// RFC 5545 DURATION (`P1W`, `PT1H30M`, `-P1D`, `P1DT2H`) → ms.
fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix('P')?;
    let mut total: i64 = 0;
    let mut num = String::new();
    let mut in_time = false;
    for c in s.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += n * match (c, in_time) {
                    ('W', false) => 7 * DAY_MS,
                    ('D', false) => DAY_MS,
                    ('H', true) => 3_600_000,
                    ('M', true) => 60_000,
                    ('S', true) => 1000,
                    _ => return None,
                };
            }
        }
    }
    Some(if neg { -total } else { total })
}

fn instance_suffix(start: &DateTime<Tz>, all_day: bool) -> String {
    let u = start.with_timezone(&Utc);
    if all_day {
        u.format("%Y%m%d").to_string()
    } else {
        u.format("%Y%m%dT%H%M%SZ").to_string()
    }
}

fn from_ms(ms: UnixMs) -> DateTime<Tz> {
    utc()
        .timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(|| utc().timestamp_millis_opt(0).unwrap())
}

// ---------------------------------------------------------------- expansion

impl IcsCalendar {
    pub fn len(&self) -> usize {
        self.events.len()
    }
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// All event instances overlapping `[wstart, wend)`.
    pub fn expand(
        &self,
        source: &str,
        calendar_id: &str,
        calendar_name: Option<&str>,
        self_emails: &[String],
        wstart: UnixMs,
        wend: UnixMs,
    ) -> Vec<CalendarEvent> {
        let mut overrides: HashMap<(String, UnixMs), &VEvent> = HashMap::new();
        let mut masters: Vec<&VEvent> = Vec::new();
        for e in &self.events {
            match &e.recurrence_id {
                Some(r) => {
                    overrides.insert((e.uid.clone(), r.timestamp_millis()), e);
                }
                None => masters.push(e),
            }
        }
        let overlaps = |s: UnixMs, e: UnixMs| s < wend && (e > wstart || (s == e && s >= wstart));
        let mk = |ev: &VEvent, id: String, s: UnixMs, e: UnixMs, all_day: bool| {
            to_event(
                ev,
                id,
                s,
                e,
                all_day,
                source,
                calendar_id,
                calendar_name,
                self_emails,
            )
        };

        let mut out = Vec::new();
        let mut used_overrides: HashSet<(String, UnixMs)> = HashSet::new();
        for m in masters {
            if m.status == "cancelled" {
                continue;
            }
            let dur = m.duration();
            let recurring = !m.rrules.is_empty() || !m.rdates.is_empty();
            if !recurring {
                let s = m.start.timestamp_millis();
                if overlaps(s, s + dur) {
                    out.push(mk(m, m.uid.clone(), s, s + dur, m.all_day));
                }
                continue;
            }
            for occ in occurrences(m, dur, wstart, wend) {
                let occ_ms = occ.timestamp_millis();
                let id = format!("{}_{}", m.uid, instance_suffix(&occ, m.all_day));
                let key = (m.uid.clone(), occ_ms);
                if let Some(o) = overrides.get(&key) {
                    used_overrides.insert(key);
                    if o.status == "cancelled" {
                        continue;
                    }
                    let s = o.start.timestamp_millis();
                    let e = s + o.duration();
                    if overlaps(s, e) {
                        out.push(mk(o, id, s, e, o.all_day));
                    }
                } else if overlaps(occ_ms, occ_ms + dur) {
                    out.push(mk(m, id, occ_ms, occ_ms + dur, m.all_day));
                }
            }
        }
        // Overrides moved into the window from an occurrence outside it, or orphans.
        for ((uid, rid), o) in &overrides {
            if used_overrides.contains(&(uid.clone(), *rid)) || o.status == "cancelled" {
                continue;
            }
            let rid_dt = o.recurrence_id.as_ref().expect("override has rid");
            let s = o.start.timestamp_millis();
            let e = s + o.duration();
            if overlaps(s, e) {
                let all_day = o.all_day;
                let id = format!("{uid}_{}", instance_suffix(rid_dt, all_day));
                out.push(mk(o, id, s, e, all_day));
            }
        }
        out.sort_by(|a, b| a.start.cmp(&b.start).then(a.event_id.cmp(&b.event_id)));
        out.dedup_by(|a, b| a.event_id == b.event_id);
        out
    }
}

/// Occurrence starts of a recurring master whose instance may overlap the window.
fn occurrences(m: &VEvent, dur: i64, wstart: UnixMs, wend: UnixMs) -> Vec<DateTime<Tz>> {
    let mut set = RRuleSet::new(m.start);
    for r in &m.rrules {
        let parsed = r
            .parse::<RRule<Unvalidated>>()
            .and_then(|u| u.validate(m.start));
        match parsed {
            Ok(v) => set = set.rrule(v),
            Err(_) => return single_if_in(m, dur, wstart, wend),
        }
    }
    for d in &m.rdates {
        set = set.rdate(*d);
    }
    for d in &m.exdates {
        set = set.exdate(*d);
    }
    let set = set
        .after(from_ms(wstart - dur.max(0) - 1))
        .before(from_ms(wend));
    let mut dates = set.all(MAX_INSTANCES).dates;
    // An RDATE-only event still has DTSTART as its first instance.
    if m.rrules.is_empty() {
        let s = m.start.timestamp_millis();
        if s >= wstart - dur && s < wend && !m.exdates.contains(&m.start) {
            dates.push(m.start);
        }
    }
    dates.sort();
    dates.dedup();
    dates
}

fn single_if_in(m: &VEvent, dur: i64, wstart: UnixMs, wend: UnixMs) -> Vec<DateTime<Tz>> {
    let s = m.start.timestamp_millis();
    if s < wend && s + dur >= wstart {
        vec![m.start]
    } else {
        vec![]
    }
}

#[allow(clippy::too_many_arguments)]
fn to_event(
    ev: &VEvent,
    event_id: String,
    start: UnixMs,
    end: UnixMs,
    all_day: bool,
    source: &str,
    calendar_id: &str,
    calendar_name: Option<&str>,
    self_emails: &[String],
) -> CalendarEvent {
    let attendees = ev
        .attendees
        .iter()
        .map(|(email, name, resp)| Attendee {
            is_self: email
                .as_ref()
                .is_some_and(|e| self_emails.iter().any(|s| s == e)),
            email: email.clone(),
            name: name.clone(),
            response: resp.clone(),
        })
        .collect();
    let conf = conference::detect_in([
        ev.location.as_deref(),
        ev.url.as_deref(),
        ev.description.as_deref(),
    ]);
    CalendarEvent {
        source: source.into(),
        event_id,
        calendar_id: calendar_id.into(),
        calendar_name: calendar_name.map(str::to_string),
        title: ev.summary.clone().unwrap_or_else(|| "(No title)".into()),
        description: ev.description.clone(),
        location: ev.location.clone(),
        start,
        end,
        all_day,
        attendees,
        conference_url: conf.as_ref().map(|c| c.0.clone()),
        conference_app: conf.map(|c| c.1.to_string()),
        organizer: ev.organizer.clone(),
        status: match ev.status.as_str() {
            "tentative" => "tentative".into(),
            "cancelled" => "cancelled".into(),
            _ => "confirmed".into(),
        },
        updated: ev.updated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str) -> UnixMs {
        DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis()
    }

    const FEED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nX-WR-CALNAME:Team\r\n\
BEGIN:VTIMEZONE\r\nTZID:Asia/Ho_Chi_Minh\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:weekly@test\r\nDTSTAMP:20261001T000000Z\r\n\
DTSTART;TZID=Asia/Ho_Chi_Minh:20261005T090000\r\nDTEND;TZID=Asia/Ho_Chi_Minh:20261005T093000\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=10\r\nEXDATE;TZID=Asia/Ho_Chi_Minh:20261019T090000\r\n\
SUMMARY:Weekly\\, sync\r\nDESCRIPTION:Join https://meet.google.com/abc-defg-hij\\nThanks\r\n\
ORGANIZER;CN=Boss:mailto:boss@example.com\r\n\
ATTENDEE;CN=\"Me, Myself\";PARTSTAT=DECLINED:mailto:ME@example.com\r\n\
ATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.com\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Reminder\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:weekly@test\r\nRECURRENCE-ID;TZID=Asia/Ho_Chi_Minh:20261012T090000\r\n\
DTSTART;TZID=Asia/Ho_Chi_Minh:20261012T140000\r\nDURATION:PT1H\r\nSUMMARY:Weekly (moved)\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:allday@test\r\nDTSTART;VALUE=DATE:20261010\r\nDTEND;VALUE=DATE:20261011\r\n\
SUMMARY:Holiday\r\nLOCATION:Somewhere with a very long name that is folded acr\r\n oss lines\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:gone@test\r\nDTSTART:20261011T010000Z\r\nSTATUS:CANCELLED\r\nSUMMARY:x\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";

    #[test]
    fn parses_and_expands() {
        let cal = parse_calendar(FEED).unwrap();
        assert_eq!(cal.name.as_deref(), Some("Team"));
        let evs = cal.expand(
            "ics:x",
            "ics:x",
            Some("Team"),
            &["me@example.com".into()],
            ms("2026-10-01T00:00:00Z"),
            ms("2026-10-27T00:00:00Z"),
        );
        let ids: Vec<_> = evs.iter().map(|e| e.event_id.as_str()).collect();
        // 10-05, (10-10 all-day), 10-12 moved, [10-19 excluded], 10-26
        assert_eq!(
            ids,
            vec![
                "weekly@test_20261005T020000Z",
                "allday@test",
                "weekly@test_20261012T020000Z",
                "weekly@test_20261026T020000Z",
            ]
        );
        let first = &evs[0];
        assert_eq!(first.start, ms("2026-10-05T09:00:00+07:00"));
        assert_eq!(first.end - first.start, 30 * 60_000);
        assert_eq!(first.title, "Weekly, sync");
        assert_eq!(first.conference_app.as_deref(), Some("google_meet"));
        assert_eq!(first.organizer.as_deref(), Some("boss@example.com"));
        assert!(first.attendees[0].is_self);
        assert_eq!(first.attendees[0].name.as_deref(), Some("Me, Myself"));
        assert_eq!(first.attendees[0].response.as_deref(), Some("declined"));
        assert!(first.description.as_deref().unwrap().contains("\nThanks"));
        let moved = &evs[2];
        assert_eq!(moved.title, "Weekly (moved)");
        assert_eq!(moved.start, ms("2026-10-12T14:00:00+07:00"));
        assert_eq!(moved.end - moved.start, 3_600_000);
        let ad = &evs[1];
        assert!(ad.all_day);
        assert_eq!(ad.end - ad.start, DAY_MS);
        assert!(ad.location.as_deref().unwrap().ends_with("across lines"));
    }

    #[test]
    fn tz_and_duration_helpers() {
        assert_eq!(
            resolve_tz("/mozilla.org/20050126_1/Europe/Berlin"),
            Some(chrono_tz::Europe::Berlin)
        );
        assert_eq!(
            resolve_tz("SE Asia Standard Time"),
            Some(chrono_tz::Asia::Bangkok)
        );
        assert_eq!(resolve_tz("Mars/Olympus"), None);
        assert_eq!(parse_duration("PT1H30M"), Some(90 * 60_000));
        assert_eq!(parse_duration("P1W"), Some(7 * DAY_MS));
        assert_eq!(parse_duration("-P1DT1S"), Some(-(DAY_MS + 1000)));
        assert_eq!(parse_duration("garbage"), None);
    }

    #[test]
    fn rejects_non_ics_and_bad_scheme() {
        assert!(parse_calendar("<html>").is_err());
        assert!(IcsSubscription::new("ftp://example.com/cal.ics", None).is_err());
        assert!(IcsSubscription::new("relative/cal.ics", None).is_err());
        let s = IcsSubscription::new("webcal://example.com/cal.ics", None).unwrap();
        assert_eq!(s.url(), "https://example.com/cal.ics");
        assert!(s.info().id.starts_with("ics:"));
        assert_eq!(s.info().label, "example.com");
    }

    #[tokio::test]
    async fn reads_local_files() {
        let dir = std::env::temp_dir().join(format!("almond-ics-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("team.ics");
        let now = 1_793_000_000_000i64; // 2026-10-26T07:33:20Z
        std::fs::write(
            &path,
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:u1\r\n\
             DTSTART:20261027T090000Z\r\nDTEND:20261027T100000Z\r\nSUMMARY:Planning\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n",
        )
        .unwrap();
        // a plain absolute path and the file:// URL name the same source
        let s = IcsSubscription::new(path.to_str().unwrap(), None)
            .unwrap()
            .with_clock(std::sync::Arc::new(move || now));
        assert!(s.url().starts_with("file:"));
        assert_eq!(s.info().label, "team.ics");
        let d = s.sync(None).await.unwrap();
        assert_eq!(d.upserts.len(), 1);
        assert_eq!(d.upserts[0].title, "Planning");
        let d2 = s.sync(d.next_cursor).await.unwrap();
        assert!(d2.upserts.is_empty() && d2.deletes.is_empty());
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(s.sync(None).await, Err(Error::NotFound(_))));
        let _ = std::fs::remove_dir_all(dir);
    }
}
