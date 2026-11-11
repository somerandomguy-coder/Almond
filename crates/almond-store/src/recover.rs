//! Opening a database that may be damaged (power loss, a full disk), and salvaging it.
//!
//! [`open_or_recover`] opens the store and runs `PRAGMA quick_check`. If the file can't be opened
//! or the check fails, it moves the database files into `corrupt-<unix ms>/` next to it (never
//! deleted), creates a fresh database and salvages what it can:
//!
//! 1. **Events** (the source of truth), row by row per `(device, seq)`, skipping unreadable rows
//!    and bodies that aren't valid JSON. They are applied to the fresh store, which rebuilds the
//!    views (meetings, artifacts, notes, calendar, jobs, records, search index).
//! 2. **View rows** whose events were lost (`INSERT OR IGNORE`, so event-derived rows win):
//!    every table of the fresh schema except the event log and the search index, bulk or row by
//!    row by primary key.
//! 3. **Sequence floor:** this device's next event seq continues after the highest seq it is
//!    known to have used (from the old version vector / events), so peers that already have a
//!    lost event never see its seq reused. Peers send lost own events back on the next sync.
//!
//! The report is stored under kv `store.last_recovery` (JSON) for the app to show.

use crate::sqlite::{SqliteStore, sqlerr};
use almond_types::event::{Event, EventKind};
use almond_types::store::Store;
use almond_types::time::now_ms;
use almond_types::{DeviceId, Error, Hlc, Result};
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// What a recovery salvaged.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RecoveryReport {
    /// Why the database was considered damaged.
    pub reason: String,
    /// Where the damaged files were moved.
    pub backup_dir: PathBuf,
    pub events_recovered: usize,
    pub events_lost: usize,
    /// Per view table: (rows copied that events didn't rebuild, rows unreadable).
    pub tables: BTreeMap<String, (usize, usize)>,
    #[serde(skip)]
    pub at: i64,
}

/// kv key holding the last [`RecoveryReport`] as JSON.
pub const LAST_RECOVERY_KEY: &str = "store.last_recovery";

/// Open the store at `path`; if it is damaged, back it up and salvage it into a fresh one.
pub fn open_or_recover(
    path: impl AsRef<Path>,
    device: DeviceId,
) -> Result<(SqliteStore, Option<RecoveryReport>)> {
    let path = path.as_ref();
    // Check before opening: opening migrates, and a damaged file can fail half-way through.
    if path.exists()
        && let Some(problem) = precheck(path)
    {
        tracing::error!(
            "database {} is damaged ({problem}); recovering",
            path.display()
        );
        let report = recover(path, device.clone(), &problem)?;
        let store = SqliteStore::open(path, device)?;
        return Ok((store, Some(report)));
    }
    let problem = match SqliteStore::open(path, device.clone()) {
        Ok(store) => match store.quick_check() {
            Ok(()) => return Ok((store, None)),
            Err(e) => e.to_string(),
        },
        Err(e) if is_corruption(&e) => e.to_string(),
        Err(e) => return Err(e),
    };
    tracing::error!(
        "database {} is damaged ({problem}); recovering",
        path.display()
    );
    let report = recover(path, device.clone(), &problem)?;
    let store = SqliteStore::open(path, device)?;
    Ok((store, Some(report)))
}

/// `Some(reason)` if the file at `path` fails `PRAGMA quick_check` (or can't be read at all).
fn precheck(path: &Path) -> Option<String> {
    let c = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(e) => return Some(e.to_string()),
    };
    match c.query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0)) {
        Ok(r) if r == "ok" => None,
        Ok(r) => Some(r),
        Err(e) => Some(e.to_string()),
    }
}

fn is_corruption(e: &Error) -> bool {
    let s = e.to_string().to_lowercase();
    s.contains("malformed") || s.contains("not a database") || s.contains("corrupt")
}

/// Move the damaged files aside and salvage them into a fresh database at `path`.
pub fn recover(path: &Path, device: DeviceId, reason: &str) -> Result<RecoveryReport> {
    let at = now_ms();
    let dir = path
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("corrupt-{at}"));
    std::fs::create_dir_all(&dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::Invalid("database path has no file name".into()))?
        .to_string_lossy()
        .to_string();
    for suffix in ["", "-wal", "-shm"] {
        let from = path.with_file_name(format!("{name}{suffix}"));
        if from.exists() {
            std::fs::rename(&from, dir.join(format!("{name}{suffix}")))?;
        }
    }
    let mut report = RecoveryReport {
        reason: reason.to_string(),
        backup_dir: dir.clone(),
        at,
        ..Default::default()
    };

    // The backup copy opens read-only; its -wal (if any) sits next to it, so committed pages in
    // the WAL are read too.
    let old = Connection::open_with_flags(
        dir.join(&name),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlerr)?;

    let fresh = SqliteStore::open(path, device.clone())?;

    // 1. events
    let (events, lost, own_max) = salvage_events(&old, &device);
    report.events_recovered = events.len();
    report.events_lost = lost;
    for chunk in events.chunks(500) {
        fresh.apply_remote(chunk.to_vec())?;
    }

    // 2. view rows the events didn't rebuild
    let tables = fresh.with_conn(|c| user_tables(c))?;
    for t in tables {
        if matches!(t.as_str(), "events" | "vv" | "meta") || t.starts_with("fts") {
            continue;
        }
        let r = fresh.with_conn(|c| Ok(copy_table(&old, c, &t)))?;
        report.tables.insert(t, r);
    }

    // 3. sequence floor + report
    fresh.with_conn(|c| {
        c.execute(
            "INSERT INTO meta (key, value) VALUES ('min_next_seq', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![(own_max + 1).to_string()],
        )
        .map_err(sqlerr)?;
        Ok(())
    })?;
    fresh.kv_set(LAST_RECOVERY_KEY, &serde_json::to_string(&report)?)?;
    tracing::warn!(
        "database recovered: {} events kept, {} lost; damaged files in {}",
        report.events_recovered,
        report.events_lost,
        dir.display()
    );
    Ok(report)
}

/// Readable events with valid JSON bodies, how many known seqs were lost, and the highest seq this
/// device is known to have used.
fn salvage_events(old: &Connection, me: &DeviceId) -> (Vec<Event>, usize, u64) {
    // Highest known seq per device: the version vector and the event log, whichever reads.
    let mut max: BTreeMap<String, u64> = BTreeMap::new();
    let mut note = |d: String, s: i64| {
        let e = max.entry(d).or_insert(0);
        *e = (*e).max(s.max(0) as u64);
    };
    if let Ok(mut st) = old.prepare("SELECT device, seq FROM vv")
        && let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
    {
        for (d, s) in rows.flatten() {
            note(d, s);
        }
    }
    if let Ok(mut st) = old.prepare("SELECT device, MAX(seq) FROM events GROUP BY device")
        && let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
    {
        for (d, s) in rows.flatten() {
            note(d, s);
        }
    }
    let own_max = max.get(&me.0).copied().unwrap_or(0);

    let mut out = vec![];
    let mut lost = 0;
    let Ok(mut st) = old.prepare(
        "SELECT hlc_wall, hlc_logical, kind, body FROM events WHERE device = ?1 AND seq = ?2",
    ) else {
        return (out, max.values().sum::<u64>() as usize, own_max);
    };
    for (dev, &hi) in &max {
        for seq in 1..=hi {
            let row = st.query_row(params![dev, seq as i64], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            });
            match row {
                Ok((w, l, kind, body)) => match serde_json::from_str(&body) {
                    Ok(body) => out.push(Event {
                        device: DeviceId(dev.clone()),
                        seq,
                        hlc: Hlc {
                            wall_ms: w,
                            logical: l as u32,
                        },
                        kind: EventKind(kind),
                        body,
                    }),
                    Err(_) => lost += 1,
                },
                Err(_) => lost += 1,
            }
        }
    }
    (out, lost, own_max)
}

fn user_tables(c: &Connection) -> Result<Vec<String>> {
    let mut st = c
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .map_err(sqlerr)?;
    let rows = st
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(sqlerr)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sqlerr)?;
    Ok(rows)
}

/// Copy `table` from `old` into `new` with `INSERT OR IGNORE`: in one pass if the old table reads,
/// else row by row by primary key. Returns (rows inserted, rows unreadable).
fn copy_table(old: &Connection, new: &Connection, table: &str) -> (usize, usize) {
    let Ok(cols) = columns(new, table) else {
        return (0, 0);
    };
    let old_cols: BTreeSet<String> = columns(old, table)
        .map(|c| c.into_iter().map(|(n, _)| n).collect())
        .unwrap_or_default();
    // Only columns both schemas have (an old file may predate a migration).
    let cols: Vec<(String, bool)> = cols
        .into_iter()
        .filter(|(n, _)| old_cols.contains(n))
        .collect();
    if cols.is_empty() {
        return (0, 0);
    }
    let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();
    let list = names.join(", ");
    let insert = format!(
        "INSERT OR IGNORE INTO {table} ({list}) VALUES ({})",
        vec!["?"; names.len()].join(", ")
    );

    let read_all = || -> rusqlite::Result<Vec<Vec<rusqlite::types::Value>>> {
        let mut st = old.prepare(&format!("SELECT {list} FROM {table}"))?;
        let rows = st.query_map([], |r| {
            (0..names.len())
                .map(|i| r.get::<_, rusqlite::types::Value>(i))
                .collect()
        })?;
        rows.collect()
    };
    let (rows, mut bad) = match read_all() {
        Ok(rows) => (rows, 0),
        Err(_) => salvage_rows(old, table, &cols),
    };
    let mut copied = 0;
    for row in rows {
        if !json_ok(&names, &row) {
            bad += 1;
            continue;
        }
        if let Ok(n) = new.execute(&insert, rusqlite::params_from_iter(row.iter())) {
            copied += n;
        }
    }
    (copied, bad)
}

/// Row-by-row salvage through the primary key (whose index often survives when table pages don't).
fn salvage_rows(
    old: &Connection,
    table: &str,
    cols: &[(String, bool)],
) -> (Vec<Vec<rusqlite::types::Value>>, usize) {
    let pk: Vec<&str> = cols
        .iter()
        .filter(|(_, is_pk)| *is_pk)
        .map(|(n, _)| n.as_str())
        .collect();
    if pk.is_empty() {
        return (vec![], 0);
    }
    let keys: Vec<Vec<rusqlite::types::Value>> = old
        .prepare(&format!("SELECT {} FROM {table}", pk.join(", ")))
        .and_then(|mut st| {
            st.query_map([], |r| {
                (0..pk.len())
                    .map(|i| r.get::<_, rusqlite::types::Value>(i))
                    .collect()
            })?
            .collect()
        })
        .unwrap_or_default();
    let list = cols
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let cond = pk
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{c} = ?{}", i + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let Ok(mut st) = old.prepare(&format!("SELECT {list} FROM {table} WHERE {cond}")) else {
        return (vec![], keys.len());
    };
    let (mut rows, mut bad) = (vec![], 0);
    for k in keys {
        match st.query_row(rusqlite::params_from_iter(k.iter()), |r| {
            (0..cols.len())
                .map(|i| r.get::<_, rusqlite::types::Value>(i))
                .collect::<rusqlite::Result<Vec<_>>>()
        }) {
            Ok(row) => rows.push(row),
            Err(_) => bad += 1,
        }
    }
    (rows, bad)
}

/// JSON document columns must still parse (a damaged page can return garbage text).
fn json_ok(names: &[&str], row: &[rusqlite::types::Value]) -> bool {
    names.iter().zip(row).all(|(n, v)| match (*n, v) {
        ("data" | "body", rusqlite::types::Value::Text(s)) => {
            serde_json::from_str::<serde::de::IgnoredAny>(s).is_ok()
        }
        _ => true,
    })
}

/// (column name, part of the primary key)
fn columns(c: &Connection, table: &str) -> rusqlite::Result<Vec<(String, bool)>> {
    let mut st = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, String>(1)?, r.get::<_, i64>(5)? > 0))
    })?;
    rows.collect()
}
