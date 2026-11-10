//! `SqliteStore`: the production [`Store`] implementation.

use crate::fts;
use crate::schema;
use almond_types::artifact::Artifact;
use almond_types::calendar::CalendarEvent;
use almond_types::event::{Event, EventBody, EventKind};
use almond_types::job::{Job, JobLease, JobOutcome, JobStatus};
use almond_types::meeting::Meeting;
pub use almond_types::store::BlobInfo;
use almond_types::store::{JobRecord, MeetingQuery, SearchHit, Store};
use almond_types::sync::{DeviceInfo, VersionVector};
use almond_types::time::{HlcClock, UnixMs, now_ms};
use almond_types::{BlobHash, DeviceId, Error, Hlc, Id, Result};
use base64::Engine;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::Path;
use std::sync::Mutex;

pub(crate) fn sqlerr(e: rusqlite::Error) -> Error {
    Error::Other(format!("sqlite: {e}"))
}

trait SqlExt<T> {
    fn sql(self) -> Result<T>;
}
impl<T> SqlExt<T> for rusqlite::Result<T> {
    fn sql(self) -> Result<T> {
        self.map_err(sqlerr)
    }
}

fn status_str(s: JobStatus) -> &'static str {
    match s {
        JobStatus::Pending => "pending",
        JobStatus::Claimed => "claimed",
        JobStatus::Done => "done",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "cancelled",
    }
}

struct Inner {
    conn: Connection,
    next_seq: u64,
}

/// SQLite-backed store (WAL mode). All methods are synchronous; wrap in `spawn_blocking` from
/// async code if latency matters.
pub struct SqliteStore {
    device: DeviceId,
    clock: HlcClock,
    inner: Mutex<Inner>,
}

impl SqliteStore {
    /// Open (or create) the database at `path` for this device.
    pub fn open(path: impl AsRef<Path>, device: DeviceId) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).sql()?;
        Self::init(conn, device)
    }

    /// Fresh in-memory database (tests, ephemeral peers).
    pub fn open_in_memory(device: DeviceId) -> Result<Self> {
        Self::init(Connection::open_in_memory().sql()?, device)
    }

    fn init(mut conn: Connection, device: DeviceId) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL").sql()?;
        conn.pragma_update(None, "synchronous", "NORMAL").sql()?;
        conn.pragma_update(None, "foreign_keys", true).sql()?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).sql()?;
        schema::migrate(&mut conn).sql()?;

        // A DB belongs to one device identity; refuse to silently mix identities.
        let owner: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'device'", [], |r| {
                r.get(0)
            })
            .optional()
            .sql()?;
        match owner {
            Some(o) if o != device.0 => {
                return Err(Error::Invalid(format!(
                    "database belongs to device {o}, opened as {}",
                    device.0
                )));
            }
            Some(_) => {}
            None => {
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('device', ?1)",
                    params![device.0],
                )
                .sql()?;
            }
        }

        let max_seq: Option<i64> = conn
            .query_row(
                "SELECT MAX(seq) FROM events WHERE device = ?1",
                params![device.0],
                |r| r.get(0),
            )
            .sql()?;
        let clock = HlcClock::new();
        let max_hlc: Option<(i64, i64)> = conn
            .query_row(
                "SELECT hlc_wall, hlc_logical FROM events ORDER BY hlc_wall DESC, hlc_logical DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .sql()?;
        if let Some((w, l)) = max_hlc {
            clock.observe(Hlc {
                wall_ms: w,
                logical: l as u32,
            });
        }
        Ok(SqliteStore {
            device,
            clock,
            inner: Mutex::new(Inner {
                conn,
                next_seq: max_seq.unwrap_or(0) as u64 + 1,
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ------------------------------------------------------------------ write path

    /// Insert one event if new; materialize it. Returns true if it was new.
    fn insert_event(tx: &Transaction, ev: &Event) -> Result<bool> {
        let n = tx
            .execute(
                "INSERT OR IGNORE INTO events (device, seq, hlc_wall, hlc_logical, kind, body, received_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    ev.device.0,
                    ev.seq as i64,
                    ev.hlc.wall_ms,
                    ev.hlc.logical as i64,
                    ev.kind.0,
                    serde_json::to_string(&ev.body)?,
                    now_ms()
                ],
            )
            .sql()?;
        if n == 0 {
            return Ok(false);
        }
        Self::materialize(tx, ev)?;
        Ok(true)
    }

    /// Advance the contiguous version-vector entry for `device` as far as the log allows.
    fn advance_vv(tx: &Transaction, device: &DeviceId) -> Result<()> {
        let cur: i64 = tx
            .query_row(
                "SELECT seq FROM vv WHERE device = ?1",
                params![device.0],
                |r| r.get(0),
            )
            .optional()
            .sql()?
            .unwrap_or(0);
        let mut next = cur;
        {
            let mut st = tx
                .prepare_cached(
                    "SELECT seq FROM events WHERE device = ?1 AND seq > ?2 ORDER BY seq",
                )
                .sql()?;
            let mut rows = st.query(params![device.0, cur]).sql()?;
            while let Some(r) = rows.next().sql()? {
                let s: i64 = r.get(0).sql()?;
                if s == next + 1 {
                    next = s;
                } else {
                    break;
                }
            }
        }
        if next != cur {
            tx.execute(
                "INSERT INTO vv (device, seq) VALUES (?1, ?2)
                 ON CONFLICT(device) DO UPDATE SET seq = excluded.seq",
                params![device.0, next],
            )
            .sql()?;
        }
        Ok(())
    }

    /// Apply an event to the materialized views. Every rule here is order-independent so that
    /// peers converge no matter in which order they receive events. Malformed bodies of known
    /// kinds are kept in the log but not materialized (never fail a sync batch over them).
    fn materialize(tx: &Transaction, ev: &Event) -> Result<()> {
        let body = match EventBody::decode(ev) {
            Ok(Some(b)) => b,
            Ok(None) | Err(_) => return Ok(()),
        };
        let (w, l, dev) = (ev.hlc.wall_ms, ev.hlc.logical as i64, ev.device.0.as_str());
        // (hlc, device) of the incoming event is newer than the stored one?
        let newer = |cur: Option<(i64, i64, String)>| match cur {
            None => true,
            Some((cw, cl, cd)) => (w, l, dev) > (cw, cl, cd.as_str()),
        };
        match body {
            EventBody::MeetingUpsert(m) => {
                let cur = Self::lww_get(tx, "meetings", "id = ?1", &[&m.id.0])?;
                if newer(cur) {
                    tx.execute(
                        "INSERT INTO meetings (id, data, started_at, deleted, hlc_wall, hlc_logical, device)
                         VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)
                         ON CONFLICT(id) DO UPDATE SET data = excluded.data, started_at = excluded.started_at,
                           deleted = 0, hlc_wall = excluded.hlc_wall, hlc_logical = excluded.hlc_logical,
                           device = excluded.device",
                        params![m.id.0, serde_json::to_string(&m)?, m.started_at, w, l, dev],
                    )
                    .sql()?;
                    Self::index_meeting(tx, &m.id, Some(&m.title))?;
                }
            }
            EventBody::MeetingDelete { id } => {
                let cur = Self::lww_get(tx, "meetings", "id = ?1", &[&id.0])?;
                if newer(cur) {
                    tx.execute(
                        "INSERT INTO meetings (id, data, started_at, deleted, hlc_wall, hlc_logical, device)
                         VALUES (?1, NULL, NULL, 1, ?2, ?3, ?4)
                         ON CONFLICT(id) DO UPDATE SET data = NULL, started_at = NULL, deleted = 1,
                           hlc_wall = excluded.hlc_wall, hlc_logical = excluded.hlc_logical,
                           device = excluded.device",
                        params![id.0, w, l, dev],
                    )
                    .sql()?;
                    Self::index_meeting(tx, &id, None)?;
                }
            }
            EventBody::ArtifactAdd(a) => {
                let n = tx
                    .execute(
                        "INSERT OR IGNORE INTO artifacts (id, meeting_id, kind, created_wall, created_logical, device, data)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            a.id.0,
                            a.meeting_id.0,
                            a.kind.0,
                            a.created_at.wall_ms,
                            a.created_at.logical as i64,
                            a.device.0,
                            serde_json::to_string(&a)?
                        ],
                    )
                    .sql()?;
                if n > 0 {
                    for d in fts::artifact_docs(&a.kind.0, &a.body) {
                        tx.execute(
                            "INSERT INTO fts (text, folded, meeting_id, artifact_id, kind, segment)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            params![
                                d.text,
                                fts::fold(&d.text),
                                a.meeting_id.0,
                                a.id.0,
                                d.kind,
                                d.segment
                            ],
                        )
                        .sql()?;
                    }
                }
            }
            EventBody::NotesUpdate { doc, update_b64 } => {
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(update_b64) else {
                    return Ok(());
                };
                tx.execute(
                    "INSERT OR IGNORE INTO notes (doc, device, seq, update_blob) VALUES (?1, ?2, ?3, ?4)",
                    params![doc.0, dev, ev.seq as i64, bytes],
                )
                .sql()?;
            }
            EventBody::CalendarUpsert(e) => {
                let cur = Self::lww_get(
                    tx,
                    "calendar",
                    "source = ?1 AND event_id = ?2",
                    &[&e.source, &e.event_id],
                )?;
                if newer(cur) {
                    tx.execute(
                        "INSERT INTO calendar (source, event_id, data, start_ms, end_ms, deleted, hlc_wall, hlc_logical, device)
                         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8)
                         ON CONFLICT(source, event_id) DO UPDATE SET data = excluded.data,
                           start_ms = excluded.start_ms, end_ms = excluded.end_ms, deleted = 0,
                           hlc_wall = excluded.hlc_wall, hlc_logical = excluded.hlc_logical, device = excluded.device",
                        params![e.source, e.event_id, serde_json::to_string(&e)?, e.start, e.end, w, l, dev],
                    )
                    .sql()?;
                }
            }
            EventBody::CalendarDelete { source, event_id } => {
                let cur = Self::lww_get(
                    tx,
                    "calendar",
                    "source = ?1 AND event_id = ?2",
                    &[&source, &event_id],
                )?;
                if newer(cur) {
                    tx.execute(
                        "INSERT INTO calendar (source, event_id, data, start_ms, end_ms, deleted, hlc_wall, hlc_logical, device)
                         VALUES (?1, ?2, NULL, NULL, NULL, 1, ?3, ?4, ?5)
                         ON CONFLICT(source, event_id) DO UPDATE SET data = NULL, start_ms = NULL, end_ms = NULL,
                           deleted = 1, hlc_wall = excluded.hlc_wall, hlc_logical = excluded.hlc_logical,
                           device = excluded.device",
                        params![source, event_id, w, l, dev],
                    )
                    .sql()?;
                }
            }
            EventBody::JobRequested(j) => {
                tx.execute(
                    "INSERT OR IGNORE INTO jobs (id, priority, data) VALUES (?1, ?2, ?3)",
                    params![j.id.0, j.priority, serde_json::to_string(&j)?],
                )
                .sql()?;
            }
            EventBody::JobClaimed(c) => {
                tx.execute(
                    "INSERT OR IGNORE INTO job_claims (job_id, device, claimed_wall, claimed_logical, expires_at, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        c.job_id.0,
                        c.device.0,
                        c.claimed_at.wall_ms,
                        c.claimed_at.logical as i64,
                        c.expires_at,
                        serde_json::to_string(&c)?
                    ],
                )
                .sql()?;
            }
            EventBody::JobFinished(o) => {
                tx.execute(
                    "INSERT OR IGNORE INTO job_outcomes (job_id, hlc_wall, hlc_logical, device, status, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![o.job_id.0, w, l, dev, status_str(o.status), serde_json::to_string(&o)?],
                )
                .sql()?;
            }
            EventBody::DeviceInfo(d) => {
                let cur: Option<(i64, i64, String)> = tx
                    .query_row(
                        "SELECT hlc_wall, hlc_logical, author FROM devices WHERE id = ?1",
                        params![d.id.0],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()
                    .sql()?;
                if newer(cur) {
                    tx.execute(
                        "INSERT INTO devices (id, data, hlc_wall, hlc_logical, author) VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT(id) DO UPDATE SET data = excluded.data, hlc_wall = excluded.hlc_wall,
                           hlc_logical = excluded.hlc_logical, author = excluded.author",
                        params![d.id.0, serde_json::to_string(&d)?, w, l, dev],
                    )
                    .sql()?;
                }
            }
            EventBody::BlobAvailable { hash, size, mime } => {
                tx.execute(
                    "INSERT OR IGNORE INTO blobs (hash, size, mime) VALUES (?1, ?2, ?3)",
                    params![hash.0, size as i64, mime],
                )
                .sql()?;
                tx.execute(
                    "INSERT OR IGNORE INTO blob_holders (hash, device) VALUES (?1, ?2)",
                    params![hash.0, dev],
                )
                .sql()?;
            }
        }
        Ok(())
    }

    fn lww_get(
        tx: &Transaction,
        table: &str,
        cond: &str,
        args: &[&dyn rusqlite::ToSql],
    ) -> Result<Option<(i64, i64, String)>> {
        tx.query_row(
            &format!("SELECT hlc_wall, hlc_logical, device FROM {table} WHERE {cond}"),
            args,
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .sql()
    }

    fn index_meeting(tx: &Transaction, id: &Id, title: Option<&str>) -> Result<()> {
        tx.execute(
            "DELETE FROM fts WHERE meeting_id = ?1 AND kind = 'meeting'",
            params![id.0],
        )
        .sql()?;
        if let Some(t) = title {
            tx.execute(
                "INSERT INTO fts (text, folded, meeting_id, artifact_id, kind, segment)
                 VALUES (?1, ?2, ?3, NULL, 'meeting', NULL)",
                params![t, fts::fold(t), id.0],
            )
            .sql()?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ extra queries

    /// Latest `DeviceInfo` per device, as announced through `device.info` events.
    pub fn devices(&self) -> Result<Vec<DeviceInfo>> {
        let g = self.lock();
        let mut st = g
            .conn
            .prepare("SELECT data FROM devices ORDER BY id")
            .sql()?;
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .sql()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .sql()?;
        rows.iter()
            .map(|s| serde_json::from_str(s).map_err(Into::into))
            .collect()
    }

    fn known_blobs_impl(&self) -> Result<Vec<BlobInfo>> {
        let g = self.lock();
        let mut st = g
            .conn
            .prepare(
                "SELECT b.hash, b.size, b.mime, group_concat(h.device, ',')
                 FROM blobs b LEFT JOIN blob_holders h ON h.hash = b.hash
                 GROUP BY b.hash ORDER BY b.hash",
            )
            .sql()?;
        st.query_map([], |r| {
            let holders: Option<String> = r.get(3)?;
            Ok(BlobInfo {
                hash: BlobHash(r.get(0)?),
                size: r.get::<_, i64>(1)? as u64,
                mime: r.get(2)?,
                holders: holders
                    .map(|s| s.split(',').map(|d| DeviceId(d.to_string())).collect())
                    .unwrap_or_default(),
            })
        })
        .sql()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .sql()
    }

    /// Current winning lease for a job (lowest `(claimed_at, device)` among unexpired claims).
    pub fn job_lease(&self, job: &Id) -> Result<Option<JobLease>> {
        let g = self.lock();
        Self::active_lease(&g.conn, job, now_ms())
    }

    fn active_lease(conn: &Connection, job: &Id, now: UnixMs) -> Result<Option<JobLease>> {
        let s: Option<String> = conn
            .query_row(
                "SELECT data FROM job_claims WHERE job_id = ?1 AND expires_at > ?2
                 ORDER BY claimed_wall, claimed_logical, device LIMIT 1",
                params![job.0, now],
                |r| r.get(0),
            )
            .optional()
            .sql()?;
        s.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }

    /// Latest outcome for a job (by event `(hlc, device)`).
    pub fn job_outcome(&self, job: &Id) -> Result<Option<JobOutcome>> {
        let g = self.lock();
        Self::latest_outcome(&g.conn, job)
    }

    fn latest_outcome(conn: &Connection, job: &Id) -> Result<Option<JobOutcome>> {
        let s: Option<String> = conn
            .query_row(
                "SELECT data FROM job_outcomes WHERE job_id = ?1
                 ORDER BY hlc_wall DESC, hlc_logical DESC, device DESC LIMIT 1",
                params![job.0],
                |r| r.get(0),
            )
            .optional()
            .sql()?;
        s.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }

    fn job_json<T: serde::de::DeserializeOwned>(
        conn: &Connection,
        sql: &str,
        job: &Id,
    ) -> Result<Vec<T>> {
        let mut st = conn.prepare_cached(sql).sql()?;
        let rows = st
            .query_map(params![job.0], |r| r.get::<_, String>(0))
            .sql()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .sql()?;
        rows.iter()
            .map(|s| serde_json::from_str(s).map_err(Into::into))
            .collect()
    }

    /// Total events in the log (all authors).
    pub fn event_count(&self) -> Result<u64> {
        let g = self.lock();
        g.conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
            .sql()
            .map(|n| n as u64)
    }

    /// Notes doc ids that have at least one update.
    pub fn notes_docs(&self) -> Result<Vec<Id>> {
        let g = self.lock();
        let mut st = g
            .conn
            .prepare("SELECT DISTINCT doc FROM notes ORDER BY doc")
            .sql()?;
        st.query_map([], |r| Ok(Id(r.get(0)?)))
            .sql()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .sql()
    }

    /// One calendar event by `(source, event_id)`, `None` if unknown or deleted.
    pub fn calendar_event(&self, source: &str, event_id: &str) -> Result<Option<CalendarEvent>> {
        let g = self.lock();
        let s: Option<Option<String>> = g
            .conn
            .query_row(
                "SELECT data FROM calendar WHERE source = ?1 AND event_id = ?2 AND deleted = 0",
                params![source, event_id],
                |r| r.get(0),
            )
            .optional()
            .sql()?;
        s.flatten()
            .map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }

    /// Every live (not deleted) event of one calendar source, any time.
    pub fn calendar_events_of(&self, source: &str) -> Result<Vec<CalendarEvent>> {
        let g = self.lock();
        let mut st = g
            .conn
            .prepare_cached(
                "SELECT data FROM calendar WHERE source = ?1 AND deleted = 0
                 ORDER BY start_ms, event_id",
            )
            .sql()?;
        let rows = st
            .query_map(params![source], |r| r.get::<_, String>(0))
            .sql()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .sql()?;
        rows.iter()
            .map(|s| serde_json::from_str(s).map_err(Into::into))
            .collect()
    }

    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut g = self.lock();
        f(&mut g.conn)
    }
}

impl Store for SqliteStore {
    fn device_id(&self) -> DeviceId {
        self.device.clone()
    }

    fn append_local(&self, body: EventBody) -> Result<Event> {
        let mut g = self.lock();
        let seq = g.next_seq;
        let ev = Event {
            device: self.device.clone(),
            seq,
            hlc: self.clock.now(),
            kind: EventKind(body.kind().to_string()),
            body: body.to_json()?,
        };
        let tx = g.conn.transaction().sql()?;
        if !Self::insert_event(&tx, &ev)? {
            return Err(Error::Other(format!("local seq {seq} already used")));
        }
        Self::advance_vv(&tx, &self.device)?;
        tx.commit().sql()?;
        g.next_seq = seq + 1;
        Ok(ev)
    }

    fn apply_remote(&self, events: Vec<Event>) -> Result<usize> {
        let mut g = self.lock();
        let tx = g.conn.transaction().sql()?;
        let mut n = 0;
        let mut touched = std::collections::BTreeSet::new();
        let mut max_own = 0u64;
        for ev in &events {
            if ev.seq == 0 || ev.device.0.is_empty() {
                continue; // invalid; seqs start at 1
            }
            if Self::insert_event(&tx, ev)? {
                self.clock.observe(ev.hlc);
                touched.insert(ev.device.clone());
                if ev.device == self.device {
                    max_own = max_own.max(ev.seq);
                }
                n += 1;
            }
        }
        for d in &touched {
            Self::advance_vv(&tx, d)?;
        }
        tx.commit().sql()?;
        // our own events coming back (e.g. restored from a peer after data loss)
        if max_own >= g.next_seq {
            g.next_seq = max_own + 1;
        }
        Ok(n)
    }

    fn version_vector(&self) -> Result<VersionVector> {
        let g = self.lock();
        let mut st = g.conn.prepare_cached("SELECT device, seq FROM vv").sql()?;
        let mut vv = VersionVector::default();
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .sql()?;
        for r in rows {
            let (d, s) = r.sql()?;
            vv.0.insert(DeviceId(d), s as u64);
        }
        Ok(vv)
    }

    fn events_missing(&self, have: &VersionVector, limit: usize) -> Result<Vec<Event>> {
        let g = self.lock();
        let devices: Vec<String> = {
            let mut st = g
                .conn
                .prepare_cached("SELECT DISTINCT device FROM events ORDER BY device")
                .sql()?;
            st.query_map([], |r| r.get(0))
                .sql()?
                .collect::<rusqlite::Result<_>>()
                .sql()?
        };
        let mut out = Vec::new();
        let mut st = g
            .conn
            .prepare_cached(
                "SELECT seq, hlc_wall, hlc_logical, kind, body FROM events
                 WHERE device = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
            )
            .sql()?;
        for d in devices {
            if out.len() >= limit {
                break;
            }
            let did = DeviceId(d);
            let from = have.get(&did) as i64;
            let left = (limit - out.len()) as i64;
            let rows = st
                .query_map(params![did.0, from, left], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })
                .sql()?;
            for r in rows {
                let (seq, w, l, kind, body) = r.sql()?;
                out.push(Event {
                    device: did.clone(),
                    seq: seq as u64,
                    hlc: Hlc {
                        wall_ms: w,
                        logical: l as u32,
                    },
                    kind: EventKind(kind),
                    body: serde_json::from_str(&body)?,
                });
            }
        }
        Ok(out)
    }

    fn get_meeting(&self, id: &Id) -> Result<Option<Meeting>> {
        let g = self.lock();
        let s: Option<String> = g
            .conn
            .query_row(
                "SELECT data FROM meetings WHERE id = ?1 AND deleted = 0",
                params![id.0],
                |r| r.get(0),
            )
            .optional()
            .sql()?;
        s.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }

    fn list_meetings(&self, q: &MeetingQuery) -> Result<Vec<Meeting>> {
        let g = self.lock();
        let mut sql = String::from("SELECT m.data FROM meetings m WHERE m.deleted = 0");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![];
        if let Some(f) = q.from {
            args.push(Box::new(f));
            sql += &format!(" AND m.started_at >= ?{}", args.len());
        }
        if let Some(t) = q.to {
            args.push(Box::new(t));
            sql += &format!(" AND m.started_at < ?{}", args.len());
        }
        if let Some(tag) = &q.tag {
            args.push(Box::new(tag.clone()));
            sql += &format!(
                " AND EXISTS (SELECT 1 FROM json_each(m.data, '$.tags') WHERE value = ?{})",
                args.len()
            );
        }
        if let Some(text) = &q.text {
            match fts::match_expr(text) {
                Some(expr) => {
                    // title substring OR any FTS hit within the meeting
                    args.push(Box::new(format!("%{}%", text.to_lowercase())));
                    let like = args.len();
                    args.push(Box::new(format!("folded : ({expr})")));
                    sql += &format!(
                        " AND (lower(json_extract(m.data, '$.title')) LIKE ?{like}
                           OR m.id IN (SELECT meeting_id FROM fts WHERE fts MATCH ?{}))",
                        args.len()
                    );
                }
                None => return Ok(vec![]),
            }
        }
        sql += " ORDER BY m.started_at DESC, m.id DESC";
        args.push(Box::new(q.limit.map(|x| x as i64).unwrap_or(-1)));
        sql += &format!(" LIMIT ?{}", args.len());
        args.push(Box::new(q.offset.unwrap_or(0) as i64));
        sql += &format!(" OFFSET ?{}", args.len());
        let mut st = g.conn.prepare(&sql).sql()?;
        let rows = st
            .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                r.get::<_, String>(0)
            })
            .sql()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .sql()?;
        rows.iter()
            .map(|s| serde_json::from_str(s).map_err(Into::into))
            .collect()
    }

    fn get_artifact(&self, id: &Id) -> Result<Option<Artifact>> {
        let g = self.lock();
        let s: Option<String> = g
            .conn
            .query_row(
                "SELECT data FROM artifacts WHERE id = ?1",
                params![id.0],
                |r| r.get(0),
            )
            .optional()
            .sql()?;
        s.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }

    fn list_artifacts(&self, meeting: &Id, kind: Option<&str>) -> Result<Vec<Artifact>> {
        let g = self.lock();
        let mut st = g
            .conn
            .prepare_cached(
                "SELECT data FROM artifacts WHERE meeting_id = ?1 AND (?2 IS NULL OR kind = ?2)
                 ORDER BY created_wall DESC, created_logical DESC, device DESC, id DESC",
            )
            .sql()?;
        let rows = st
            .query_map(params![meeting.0, kind], |r| r.get::<_, String>(0))
            .sql()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .sql()?;
        rows.iter()
            .map(|s| serde_json::from_str(s).map_err(Into::into))
            .collect()
    }

    fn notes_updates(&self, doc: &Id) -> Result<Vec<Vec<u8>>> {
        let g = self.lock();
        let snap: Option<(Vec<u8>, i64)> = g
            .conn
            .query_row(
                "SELECT snapshot, covered_id FROM notes_snapshots WHERE doc = ?1",
                params![doc.0],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .sql()?;
        let mut out = vec![];
        let covered = match snap {
            Some((s, c)) => {
                out.push(s);
                c
            }
            None => 0,
        };
        let mut st = g
            .conn
            .prepare_cached("SELECT update_blob FROM notes WHERE doc = ?1 AND id > ?2 ORDER BY id")
            .sql()?;
        let rows = st
            .query_map(params![doc.0, covered], |r| r.get::<_, Vec<u8>>(0))
            .sql()?;
        for r in rows {
            out.push(r.sql()?);
        }
        Ok(out)
    }

    fn calendar_events(&self, from: UnixMs, to: UnixMs) -> Result<Vec<CalendarEvent>> {
        let g = self.lock();
        let mut st = g
            .conn
            .prepare_cached(
                "SELECT data FROM calendar WHERE deleted = 0 AND end_ms >= ?1 AND start_ms < ?2
                 ORDER BY start_ms, source, event_id",
            )
            .sql()?;
        let rows = st
            .query_map(params![from, to], |r| r.get::<_, String>(0))
            .sql()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .sql()?;
        rows.iter()
            .map(|s| serde_json::from_str(s).map_err(Into::into))
            .collect()
    }

    fn jobs(&self, status: Option<JobStatus>) -> Result<Vec<(Job, JobStatus)>> {
        Ok(self
            .job_records(status)?
            .into_iter()
            .map(|r| (r.job, r.status))
            .collect())
    }

    fn job_records(&self, status: Option<JobStatus>) -> Result<Vec<JobRecord>> {
        let g = self.lock();
        let now = now_ms();
        let mut st = g
            .conn
            .prepare_cached("SELECT data FROM jobs ORDER BY priority DESC, id")
            .sql()?;
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .sql()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .sql()?;
        let mut out = vec![];
        for s in rows {
            let job: Job = serde_json::from_str(&s)?;
            let leases: Vec<JobLease> = Self::job_json(
                &g.conn,
                "SELECT data FROM job_claims WHERE job_id = ?1
                 ORDER BY claimed_wall, claimed_logical, device",
                &job.id,
            )?;
            let outcomes: Vec<JobOutcome> = Self::job_json(
                &g.conn,
                "SELECT data FROM job_outcomes WHERE job_id = ?1
                 ORDER BY hlc_wall, hlc_logical, device",
                &job.id,
            )?;
            let st = JobStatus::derive(&outcomes, leases.iter().any(|l| l.expires_at > now));
            if status.is_none_or(|x| x == st) {
                out.push(JobRecord {
                    job,
                    status: st,
                    leases,
                    outcomes,
                });
            }
        }
        Ok(out)
    }

    fn known_blobs(&self) -> Result<Vec<BlobInfo>> {
        self.known_blobs_impl()
    }

    fn search(&self, text: &str, limit: u32) -> Result<Vec<SearchHit>> {
        let Some(expr) = fts::match_expr(text) else {
            return Ok(vec![]);
        };
        let g = self.lock();
        let (a, b) = fts::highlight_markers();
        let mut st = g
            .conn
            .prepare_cached(
                "SELECT text, highlight(fts, 1, ?2, ?3), meeting_id, artifact_id, kind, segment, bm25(fts)
                 FROM fts
                 WHERE fts MATCH ?1
                   AND meeting_id NOT IN (SELECT id FROM meetings WHERE deleted = 1)
                 ORDER BY bm25(fts) LIMIT ?4",
            )
            .sql()?;
        let rows = st
            .query_map(
                params![format!("folded : ({expr})"), a, b, limit as i64],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<i64>>(5)?,
                        r.get::<_, f64>(6)?,
                    ))
                },
            )
            .sql()?;
        let mut hits = vec![];
        for r in rows {
            let (orig, hl, mid, aid, kind, seg, rank) = r.sql()?;
            hits.push(SearchHit {
                meeting_id: Id(mid),
                artifact_id: aid.map(Id),
                kind,
                snippet: fts::snippet(&orig, &hl, 160),
                segment: seg.map(|s| s as u32),
                score: -rank as f32,
            });
        }
        Ok(hits)
    }

    fn kv_get(&self, key: &str) -> Result<Option<String>> {
        let g = self.lock();
        g.conn
            .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
            .sql()
    }

    fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        let g = self.lock();
        g.conn
            .execute(
                "INSERT INTO kv (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .sql()?;
        Ok(())
    }
}
