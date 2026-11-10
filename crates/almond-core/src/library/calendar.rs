//! Calendar sources (Google accounts, ICS feeds, the phone), the poll loop that turns their
//! deltas into `calendar.upsert` / `calendar.delete` events, `calendar.upcoming`, and the
//! reminder loop that emits `calendar.reminder`.
//!
//! * Sources are device-local config (kv `library.calendar.sources`); their *events* sync to every
//!   paired device. Cursors live in kv `library.calendar.cursor.<source id>`.
//! * Google needs the user's own OAuth client in setting `google.oauth_client`
//!   (`{ client_id, client_secret }`, docs/google-setup.md). Refresh tokens and the account index
//!   are in the OS keychain (via the data dir's secret store), never in the database.
//! * E3: a delta's `reset_calendars` deletes that calendar's stored events that the delta does
//!   not re-add, before the upserts are applied.

use super::add;
use crate::error::ApiResult;
use crate::router::{NoParams, Router};
use crate::settings::keys;
use crate::{Core, Shared};
use almond_calendar::oauth::{AddAccountOptions, GoogleAccount, OpenUrl};
use almond_calendar::{
    ActiveDevicePolicy, DeviceActivity, GoogleAuth, GoogleCalendar, GoogleOAuthConfig,
    IcsSubscription, PhoneCalendar, PollPolicy, ReminderConfig, ReminderScheduler,
};
use almond_types::api::{CalendarSourcesResult, UpcomingEvent, notifications};
use almond_types::calendar::{CalendarDelta, CalendarEvent, CalendarSource, CalendarSourceInfo};
use almond_types::event::EventBody;
use almond_types::meeting::EventRef;
use almond_types::secrets::SecretStore;
use almond_types::store::{MeetingQuery, Store};
use almond_types::time::{UnixMs, now_ms};
use almond_types::{Error, Id, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::Notify;

const SOURCES_KEY: &str = "library.calendar.sources";
const FIRED_KEY: &str = "library.reminders.fired";
const MIN_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MIN_MS;

pub(crate) const MISSING_CLIENT: &str = "Google sign-in is not set up yet: add your own Google \
    OAuth client (client ID and secret of a \"Desktop app\" client) under Settings → Calendar & \
    mail → Google. docs/google-setup.md explains how to create one.";

fn cursor_key(source: &str) -> String {
    format!("library.calendar.cursor.{source}")
}

/// One configured source on this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SourceConfig {
    id: String,
    /// "google" | "ics" | "phone"
    provider: String,
    label: String,
    #[serde(default = "yes")]
    enabled: bool,
    /// ICS feed URL.
    #[serde(default)]
    url: Option<String>,
    /// Google account email.
    #[serde(default)]
    account: Option<String>,
}

fn yes() -> bool {
    true
}

impl SourceConfig {
    fn info(&self) -> CalendarSourceInfo {
        CalendarSourceInfo {
            id: self.id.clone(),
            provider: self.provider.clone(),
            label: self.label.clone(),
            enabled: self.enabled,
        }
    }
}

/// Setting `google.oauth_client`: the OAuth client plus optional API base URLs (tests point
/// them at a mock server; the OAuth endpoint URLs are part of [`GoogleOAuthConfig`]).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct GoogleClient {
    #[serde(flatten)]
    oauth: GoogleOAuthConfig,
    #[serde(default)]
    calendar_api_base: Option<String>,
    #[serde(default)]
    gmail_api_base: Option<String>,
}

pub(crate) struct CalState {
    secrets: Arc<dyn SecretStore>,
    google: Mutex<Option<(GoogleClient, Arc<GoogleAuth>)>>,
    opener: RwLock<OpenUrl>,
    phone: Mutex<Option<Arc<PhoneCalendar>>>,
    /// Serializes polls (loop, `calendar.refresh`, adds).
    poll_lock: tokio::sync::Mutex<()>,
    wake_poll: Notify,
    wake_reminders: Notify,
    /// source id → consecutive poll errors
    errors: Mutex<HashMap<String, u32>>,
    reminders: Mutex<ReminderScheduler>,
}

impl CalState {
    pub(crate) fn new(
        shared: &Shared,
        secrets: Arc<dyn SecretStore>,
        activity: Arc<DeviceActivity>,
    ) -> Result<Self> {
        let policy = ActiveDevicePolicy::new(shared.device_id().clone(), activity);
        let mut sched = ReminderScheduler::new(ReminderConfig::default(), Box::new(policy));
        if let Some(s) = shared.store().kv_get(FIRED_KEY)?
            && let Ok(fired) = serde_json::from_str::<Vec<(String, UnixMs)>>(&s)
        {
            sched.restore_fired(fired);
        }
        Ok(CalState {
            secrets,
            google: Mutex::new(None),
            opener: RwLock::new(Arc::new(system_open)),
            phone: Mutex::new(None),
            poll_lock: tokio::sync::Mutex::new(()),
            wake_poll: Notify::new(),
            wake_reminders: Notify::new(),
            errors: Mutex::new(HashMap::new()),
            reminders: Mutex::new(sched),
        })
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Open a URL in the system browser (Google sign-in).
fn system_open(url: &str) -> Result<()> {
    let cmd: Option<(&str, Vec<&str>)> = if cfg!(windows) {
        Some(("rundll32", vec!["url.dll,FileProtocolHandler", url]))
    } else if cfg!(target_os = "macos") {
        Some(("open", vec![url]))
    } else if cfg!(target_os = "android") {
        None
    } else {
        Some(("xdg-open", vec![url]))
    };
    let (prog, args) = cmd.ok_or_else(|| {
        Error::Unsupported("opening the browser needs the Android plugin (set_url_opener)".into())
    })?;
    std::process::Command::new(prog)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| Error::Other(format!("could not open the browser: {e}")))
}

/// Replace how the core opens URLs for Google sign-in (the Tauri shell can pass its opener
/// plugin; tests drive the OAuth redirect themselves).
pub fn set_url_opener(core: &Core, open: OpenUrl) {
    *core
        .library
        .cal
        .opener
        .write()
        .unwrap_or_else(|p| p.into_inner()) = open;
}

/// Android plugin entry point: hand over a phone-calendar snapshot (JSON shape documented in
/// `almond_calendar::phone`). The next poll writes the differences. Returns the row count.
pub fn push_phone_snapshot(core: &Core, json: &str) -> Result<usize> {
    let phone = lock(&core.library.cal.phone).clone().ok_or_else(|| {
        Error::Invalid("phone calendars are not imported (calendar.importPhone)".into())
    })?;
    let n = phone.push_json(json)?;
    core.library.cal.wake_poll.notify_one();
    Ok(n)
}

/// Re-check reminders now (after events arrived by sync, settings changes, ...).
pub(crate) fn wake_reminders(core: &Core) {
    core.library.cal.wake_reminders.notify_one();
}

// ---------------------------------------------------------------- config

fn sources(core: &Core) -> Result<Vec<SourceConfig>> {
    Ok(match core.store().kv_get(SOURCES_KEY)? {
        Some(s) if !s.is_empty() => serde_json::from_str(&s)?,
        _ => vec![],
    })
}

fn save_sources(core: &Core, list: &[SourceConfig]) -> Result<()> {
    core.store()
        .kv_set(SOURCES_KEY, &serde_json::to_string(list)?)
}

fn upsert_source(core: &Core, cfg: SourceConfig) -> Result<()> {
    let mut list = sources(core)?;
    match list.iter_mut().find(|s| s.id == cfg.id) {
        Some(s) => *s = cfg,
        None => list.push(cfg),
    }
    save_sources(core, &list)
}

/// The Google client from settings, and the (cached) `GoogleAuth` built from it.
fn google(core: &Core) -> Result<(GoogleClient, Arc<GoogleAuth>)> {
    let client: Option<GoogleClient> = core.setting_as(keys::GOOGLE_OAUTH_CLIENT)?;
    let client = client
        .filter(|c| !c.oauth.client_id.trim().is_empty())
        .ok_or_else(|| Error::Invalid(MISSING_CLIENT.into()))?;
    let mut g = lock(&core.library.cal.google);
    if let Some((c, auth)) = g.as_ref()
        && *c == client
    {
        return Ok((c.clone(), auth.clone()));
    }
    let auth = Arc::new(GoogleAuth::new(
        client.oauth.clone(),
        core.library.cal.secrets.clone(),
    ));
    *g = Some((client.clone(), auth.clone()));
    Ok((client, auth))
}

/// Connected Google accounts (reads the account index only; works without a client).
pub(crate) fn google_accounts(core: &Core) -> Result<Vec<GoogleAccount>> {
    match google(core) {
        Ok((_, auth)) => auth.accounts(),
        Err(_) => GoogleAuth::new(
            GoogleOAuthConfig::new("", None),
            core.library.cal.secrets.clone(),
        )
        .accounts(),
    }
}

pub(crate) fn gmail_source(core: &Core, account: &str) -> Result<almond_mail::GmailSource> {
    let (client, auth) = google(core)?;
    let mut src = almond_mail::GmailSource::for_account(auth, account)?;
    if let Some(b) = &client.gmail_api_base {
        src = src.with_api_base(b);
    }
    Ok(src)
}

fn build_source(core: &Core, cfg: &SourceConfig) -> Result<Arc<dyn CalendarSource>> {
    match cfg.provider.as_str() {
        "ics" => {
            let url = cfg
                .url
                .as_deref()
                .ok_or_else(|| Error::Invalid(format!("source {} has no url", cfg.id)))?;
            let self_emails = google_accounts(core)
                .unwrap_or_default()
                .into_iter()
                .map(|a| a.email);
            Ok(Arc::new(
                IcsSubscription::new(url, Some(cfg.label.clone()))?.with_self_emails(self_emails),
            ))
        }
        "google" => {
            let account = cfg.account.clone().unwrap_or_default();
            let (client, auth) = google(core)?;
            let mut g = GoogleCalendar::new(account, auth);
            if let Some(b) = &client.calendar_api_base {
                g = g.with_api_base(b);
            }
            Ok(Arc::new(g))
        }
        "phone" => match lock(&core.library.cal.phone).clone() {
            Some(p) => Ok(p),
            None => Err(Error::Unsupported(
                "no phone snapshot on this device".into(),
            )),
        },
        other => Err(Error::Unsupported(format!("calendar provider {other}"))),
    }
}

// ---------------------------------------------------------------- applying deltas

/// Write a delta into the log. Skips upserts identical to what is stored (several devices may
/// poll the same account) and deletes of events we don't have. Returns the number of events
/// written.
pub(crate) fn apply_delta(core: &Core, source: &str, delta: &CalendarDelta) -> Result<usize> {
    let store = core.store();
    let existing: HashMap<String, CalendarEvent> = store
        .calendar_events_of(source)?
        .into_iter()
        .map(|e| (e.event_id.clone(), e))
        .collect();
    let mut bodies = vec![];
    if !delta.reset_calendars.is_empty() {
        let keep: HashSet<&str> = delta.upserts.iter().map(|e| e.event_id.as_str()).collect();
        for e in existing.values() {
            if delta.reset_calendars.contains(&e.calendar_id) && !keep.contains(e.event_id.as_str())
            {
                bodies.push(EventBody::CalendarDelete {
                    source: source.to_string(),
                    event_id: e.event_id.clone(),
                });
            }
        }
    }
    for e in &delta.upserts {
        if existing.get(&e.event_id) != Some(e) {
            bodies.push(EventBody::CalendarUpsert(e.clone()));
        }
    }
    for id in &delta.deletes {
        if existing.contains_key(id) {
            bodies.push(EventBody::CalendarDelete {
                source: source.to_string(),
                event_id: id.clone(),
            });
        }
    }
    let n = bodies.len();
    for b in bodies {
        store.append_local(b)?;
    }
    Ok(n)
}

/// Sync one source and apply the result.
async fn sync_source(core: &Core, cfg: &SourceConfig) -> Result<usize> {
    let src = build_source(core, cfg)?;
    let cursor = core
        .store()
        .kv_get(&cursor_key(&cfg.id))?
        .filter(|c| !c.is_empty());
    let delta = src.sync(cursor).await?;
    let n = apply_delta(core, &cfg.id, &delta)?;
    if let Some(c) = &delta.next_cursor {
        core.store().kv_set(&cursor_key(&cfg.id), c)?;
    }
    Ok(n)
}

/// Poll every enabled source once. Returns `(source, error)` for the ones that failed.
async fn poll_all(core: &Core) -> Vec<(SourceConfig, Error)> {
    let cal = &core.library.cal;
    let _g = cal.poll_lock.lock().await;
    let list = match sources(core) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("calendar sources: {e}");
            return vec![];
        }
    };
    let mut failed = vec![];
    let mut changed = 0;
    for cfg in list.into_iter().filter(|s| s.enabled) {
        if cfg.provider == "phone" && lock(&cal.phone).is_none() {
            continue; // nothing pushed yet in this session
        }
        match sync_source(core, &cfg).await {
            Ok(n) => {
                changed += n;
                lock(&cal.errors).remove(&cfg.id);
            }
            Err(e) => {
                tracing::warn!("calendar {}: {e}", cfg.id);
                *lock(&cal.errors).entry(cfg.id.clone()).or_default() += 1;
                failed.push((cfg, e));
            }
        }
    }
    if changed > 0 {
        cal.wake_reminders.notify_one();
    }
    failed
}

/// Background: poll sources following `PollPolicy` (faster when a meeting is close, backoff
/// on errors) while the app is open; `calendar.refresh` and new sources wake it early.
pub(crate) async fn poll_loop(core: Arc<Core>) {
    let policy = PollPolicy::default();
    loop {
        poll_all(&core).await;
        let now = now_ms();
        let soon = core
            .store()
            .calendar_events(now, now + 24 * HOUR_MS)
            .unwrap_or_default();
        let mut delay = policy.next_interval(&soon, now);
        let worst = lock(&core.library.cal.errors)
            .values()
            .copied()
            .max()
            .unwrap_or(0);
        if worst > 0 {
            delay = delay.min(policy.backoff(worst));
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = core.library.cal.wake_poll.notified() => {}
        }
    }
}

// ---------------------------------------------------------------- reminders

/// One reminder pass: refresh the lead time and peers' activity, emit due reminders.
/// Returns when the next one could become due.
fn reminder_tick(core: &Core) -> Option<UnixMs> {
    let lib = &core.library;
    let lead = core
        .setting_as::<f64>(keys::REMINDER_LEAD_MIN)
        .ok()
        .flatten()
        .filter(|m| *m >= 0.0 && *m <= 24.0 * 60.0)
        .map(|m| m.round() as u32)
        .unwrap_or(5);
    // other devices' activity, as announced in their device.info heartbeats
    if let Ok(devs) = core.store().devices() {
        for d in devs {
            if &d.id != core.device_id()
                && let Some(t) = d.last_active
            {
                lib.activity.record(d.id, t);
            }
        }
    }
    let now = now_ms();
    let events = core
        .store()
        .calendar_events(now - HOUR_MS, now + (lead as i64 + 60) * MIN_MS)
        .unwrap_or_default();
    let mut sched = lock(&lib.cal.reminders);
    if sched.config().lead_minutes != lead {
        sched.set_config(ReminderConfig {
            lead_minutes: lead,
            ..ReminderConfig::default()
        });
    }
    let due = sched.due(&events, now);
    if !due.is_empty() {
        if let Ok(s) = serde_json::to_string(&sched.fired_keys()) {
            let _ = core.store().kv_set(FIRED_KEY, &s);
        }
        for r in due {
            core.notifier.emit(notifications::CALENDAR_REMINDER, r);
        }
    }
    sched.next_check_at(&events, now)
}

/// Background: emit `calendar.reminder` `calendar.reminder_lead_min` minutes before meetings
/// (only on the most recently used device, see `ActiveDevicePolicy`).
pub(crate) async fn reminder_loop(core: Arc<Core>) {
    const MAX_SLEEP: Duration = Duration::from_secs(20);
    loop {
        let next = reminder_tick(&core);
        let sleep = next
            .map(|t| Duration::from_millis((t - now_ms()).clamp(200, 20_000) as u64))
            .unwrap_or(MAX_SLEEP);
        tokio::select! {
            _ = tokio::time::sleep(sleep) => {}
            _ = core.library.cal.wake_reminders.notified() => {}
        }
    }
}

// ---------------------------------------------------------------- methods

#[derive(Deserialize)]
struct AddIcsParams {
    url: String,
    label: Option<String>,
}

#[derive(Deserialize, Default)]
struct AddGoogleParams {
    #[serde(default)]
    gmail: bool,
    login_hint: Option<String>,
}

#[derive(Deserialize)]
struct RemoveParams {
    source: String,
}

#[derive(Deserialize)]
struct UpcomingParams {
    hours: Option<f64>,
}

async fn add_ics(core: &Core, p: AddIcsParams) -> Result<CalendarSourceInfo> {
    let label = p.label.filter(|l| !l.trim().is_empty());
    let sub = IcsSubscription::new(&p.url, label)?;
    let info = sub.info();
    let cfg = SourceConfig {
        id: info.id.clone(),
        provider: "ics".into(),
        label: info.label.clone(),
        enabled: true,
        url: Some(sub.url().to_string()),
        account: None,
    };
    // Fetch once before saving, so a wrong URL is reported instead of silently polled.
    {
        let _g = core.library.cal.poll_lock.lock().await;
        sync_source(core, &cfg).await?;
    }
    upsert_source(core, cfg)?;
    core.library.cal.wake_reminders.notify_one();
    Ok(info)
}

async fn add_google(core: &Core, p: AddGoogleParams) -> Result<CalendarSourceInfo> {
    let (_, auth) = google(core)?;
    let opener = core
        .library
        .cal
        .opener
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let acct = auth
        .add_account(
            AddAccountOptions {
                gmail: p.gmail,
                login_hint: p.login_hint,
                timeout: None,
            },
            &opener,
        )
        .await?;
    let cfg = SourceConfig {
        id: format!("google:{}", acct.email),
        provider: "google".into(),
        label: acct.email.clone(),
        enabled: true,
        url: None,
        account: Some(acct.email.clone()),
    };
    upsert_source(core, cfg.clone())?;
    core.library.cal.wake_poll.notify_one();
    Ok(cfg.info())
}

fn import_phone(core: &Core) -> Result<CalendarSourceInfo> {
    if core.cfg.platform != "android" {
        return Err(Error::Unsupported(
            "importing phone calendars works only in the Android app".into(),
        ));
    }
    let phone = Arc::new(PhoneCalendar::new(
        core.device_id().clone(),
        core.device().name.clone(),
    ));
    let info = phone.info();
    *lock(&core.library.cal.phone) = Some(phone);
    upsert_source(
        core,
        SourceConfig {
            id: info.id.clone(),
            provider: "phone".into(),
            label: info.label.clone(),
            enabled: true,
            url: None,
            account: None,
        },
    )?;
    Ok(info)
}

async fn remove(core: &Core, source: &str) -> Result<()> {
    let mut list = sources(core)?;
    let Some(pos) = list.iter().position(|s| s.id == source) else {
        return Err(Error::NotFound(format!("calendar source {source}")));
    };
    let _g = core.library.cal.poll_lock.lock().await;
    let cfg = list.remove(pos);
    if cfg.provider == "google"
        && let Some(email) = &cfg.account
    {
        // revoke + forget the token; works without a configured client too
        let auth = match google(core) {
            Ok((_, a)) => a,
            Err(_) => Arc::new(GoogleAuth::new(
                GoogleOAuthConfig::new("", None),
                core.library.cal.secrets.clone(),
            )),
        };
        auth.remove_account(email).await?;
    }
    if cfg.provider == "phone" {
        *lock(&core.library.cal.phone) = None;
    }
    save_sources(core, &list)?;
    core.store().kv_set(&cursor_key(source), "")?;
    lock(&core.library.cal.errors).remove(source);
    // the user removed the calendar: its events go away on every device
    for e in core.store().calendar_events_of(source)? {
        core.store().append_local(EventBody::CalendarDelete {
            source: source.to_string(),
            event_id: e.event_id,
        })?;
    }
    Ok(())
}

async fn refresh(core: &Core) -> Result<()> {
    let failed = poll_all(core).await;
    if failed.is_empty() {
        return Ok(());
    }
    let msg = failed
        .iter()
        .map(|(s, e)| format!("{}: {e}", s.label))
        .collect::<Vec<_>>()
        .join("; ");
    // keep the first error's kind (e.g. auth required → the UI offers "sign in again")
    Err(match failed.into_iter().next().map(|f| f.1) {
        Some(Error::AuthRequired(_)) => Error::AuthRequired(msg),
        Some(Error::PermissionDenied(_)) => Error::PermissionDenied(msg),
        Some(Error::Invalid(_)) => Error::Invalid(msg),
        Some(Error::NotFound(_)) => Error::NotFound(msg),
        Some(Error::Unsupported(_)) => Error::Unsupported(msg),
        _ => Error::Network(msg),
    })
}

fn upcoming(core: &Core, hours: f64) -> Result<Vec<UpcomingEvent>> {
    let now = now_ms();
    let hours = if hours.is_finite() && hours > 0.0 {
        hours.min(24.0 * 366.0)
    } else {
        24.0
    };
    let events = core
        .store()
        .calendar_events(now, now + (hours * HOUR_MS as f64) as i64)?;
    let linked: HashMap<EventRef, Id> = core
        .store()
        .list_meetings(&MeetingQuery {
            from: Some(now - 30 * 24 * HOUR_MS),
            ..Default::default()
        })?
        .into_iter()
        .filter_map(|m| m.calendar_event.map(|r| (r, m.id)))
        .collect();
    Ok(events
        .into_iter()
        .filter(|e| e.status != "cancelled")
        .map(|e| {
            let r = EventRef {
                source: e.source.clone(),
                event_id: e.event_id.clone(),
            };
            UpcomingEvent {
                meeting_id: linked.get(&r).cloned(),
                event: e,
            }
        })
        .collect())
}

pub(super) fn register(r: &mut Router) {
    add(r, "calendar.sources", |core, _: NoParams| async move {
        ApiResult::Ok(CalendarSourcesResult {
            sources: sources(&core)?.iter().map(SourceConfig::info).collect(),
        })
    });
    add(r, "calendar.addIcs", |core, p: AddIcsParams| async move {
        ApiResult::Ok(add_ics(&core, p).await?)
    });
    add(
        r,
        "calendar.addGoogle",
        |core, p: AddGoogleParams| async move { ApiResult::Ok(add_google(&core, p).await?) },
    );
    add(r, "calendar.importPhone", |core, _: NoParams| async move {
        ApiResult::Ok(import_phone(&core)?)
    });
    add(r, "calendar.remove", |core, p: RemoveParams| async move {
        remove(&core, &p.source).await?;
        core.library.cal.wake_reminders.notify_one();
        ApiResult::Ok(Value::Null)
    });
    add(
        r,
        "calendar.upcoming",
        |core, p: UpcomingParams| async move {
            ApiResult::Ok(upcoming(&core, p.hours.unwrap_or(24.0))?)
        },
    );
    add(r, "calendar.refresh", |core, _: NoParams| async move {
        let r = refresh(&core).await;
        // events may also have arrived by sync since the last check
        core.library.cal.wake_reminders.notify_one();
        r?;
        ApiResult::Ok(Value::Null)
    });
}
