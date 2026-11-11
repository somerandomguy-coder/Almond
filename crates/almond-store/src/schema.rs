//! SQLite schema and forward-only migrations (tracked with `PRAGMA user_version`).

use rusqlite::Connection;

/// Each entry upgrades the schema from version `i` to `i + 1`. Never edit a shipped entry; append.
const MIGRATIONS: &[&str] = &[
    // ---- v1 ----
    r#"
    -- device-local metadata (never synced)
    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE kv   (key TEXT PRIMARY KEY, value TEXT NOT NULL);

    -- the replicated event log; unknown kinds are stored verbatim
    CREATE TABLE events (
        device      TEXT    NOT NULL,
        seq         INTEGER NOT NULL,
        hlc_wall    INTEGER NOT NULL,
        hlc_logical INTEGER NOT NULL,
        kind        TEXT    NOT NULL,
        body        TEXT    NOT NULL,
        received_at INTEGER NOT NULL,
        PRIMARY KEY (device, seq)
    ) WITHOUT ROWID;

    -- highest *contiguous* seq per author
    CREATE TABLE vv (device TEXT PRIMARY KEY, seq INTEGER NOT NULL) WITHOUT ROWID;

    -- LWW register per meeting id over upsert/delete, ordered by (hlc, device)
    CREATE TABLE meetings (
        id          TEXT PRIMARY KEY,
        data        TEXT,
        started_at  INTEGER,
        deleted     INTEGER NOT NULL DEFAULT 0,
        hlc_wall    INTEGER NOT NULL,
        hlc_logical INTEGER NOT NULL,
        device      TEXT    NOT NULL
    );
    CREATE INDEX meetings_started ON meetings (started_at);

    -- immutable
    CREATE TABLE artifacts (
        id              TEXT PRIMARY KEY,
        meeting_id      TEXT    NOT NULL,
        kind            TEXT    NOT NULL,
        created_wall    INTEGER NOT NULL,
        created_logical INTEGER NOT NULL,
        device          TEXT    NOT NULL,
        data            TEXT    NOT NULL
    );
    CREATE INDEX artifacts_meeting ON artifacts (meeting_id, kind);

    -- Loro updates per notes doc, in local apply order
    CREATE TABLE notes (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        doc         TEXT    NOT NULL,
        device      TEXT    NOT NULL,
        seq         INTEGER NOT NULL,
        update_blob BLOB    NOT NULL,
        UNIQUE (device, seq)
    );
    CREATE INDEX notes_doc ON notes (doc, id);
    -- local compaction cache: snapshot covering notes rows with id <= covered_id
    CREATE TABLE notes_snapshots (
        doc        TEXT PRIMARY KEY,
        snapshot   BLOB    NOT NULL,
        covered_id INTEGER NOT NULL
    );

    -- LWW per (source, event_id)
    CREATE TABLE calendar (
        source      TEXT    NOT NULL,
        event_id    TEXT    NOT NULL,
        data        TEXT,
        start_ms    INTEGER,
        end_ms      INTEGER,
        deleted     INTEGER NOT NULL DEFAULT 0,
        hlc_wall    INTEGER NOT NULL,
        hlc_logical INTEGER NOT NULL,
        device      TEXT    NOT NULL,
        PRIMARY KEY (source, event_id)
    );
    CREATE INDEX calendar_start ON calendar (start_ms);

    -- jobs: status is derived at query time from requests, claims and outcomes so that the
    -- result does not depend on the order events arrived in
    CREATE TABLE jobs (id TEXT PRIMARY KEY, priority INTEGER NOT NULL, data TEXT NOT NULL);
    CREATE TABLE job_claims (
        job_id          TEXT    NOT NULL,
        device          TEXT    NOT NULL,
        claimed_wall    INTEGER NOT NULL,
        claimed_logical INTEGER NOT NULL,
        expires_at      INTEGER NOT NULL,
        data            TEXT    NOT NULL,
        PRIMARY KEY (job_id, device, claimed_wall, claimed_logical)
    );
    CREATE TABLE job_outcomes (
        job_id      TEXT    NOT NULL,
        hlc_wall    INTEGER NOT NULL,
        hlc_logical INTEGER NOT NULL,
        device      TEXT    NOT NULL,
        status      TEXT    NOT NULL,
        data        TEXT    NOT NULL,
        PRIMARY KEY (job_id, hlc_wall, hlc_logical, device)
    );

    -- LWW per device id
    CREATE TABLE devices (
        id          TEXT PRIMARY KEY,
        data        TEXT    NOT NULL,
        hlc_wall    INTEGER NOT NULL,
        hlc_logical INTEGER NOT NULL,
        author      TEXT    NOT NULL
    );

    -- blobs announced on any device (bytes live in the BlobStore)
    CREATE TABLE blobs (hash TEXT PRIMARY KEY, size INTEGER NOT NULL, mime TEXT NOT NULL);
    CREATE TABLE blob_holders (hash TEXT NOT NULL, device TEXT NOT NULL, PRIMARY KEY (hash, device));

    -- full-text search. `folded` is the indexed copy (đ→d etc., char-for-char with `text`);
    -- unicode61 + remove_diacritics 2 folds the remaining Vietnamese tone/vowel marks.
    CREATE VIRTUAL TABLE fts USING fts5 (
        text UNINDEXED,
        folded,
        meeting_id UNINDEXED,
        artifact_id UNINDEXED,
        kind UNINDEXED,
        segment UNINDEXED,
        tokenize = 'unicode61 remove_diacritics 2'
    );
    "#,
    // 2: generic synced records (people, recipes, chat threads, ...), LWW per (collection, id)
    r#"
    CREATE TABLE IF NOT EXISTS records (
        collection  TEXT    NOT NULL,
        id          TEXT    NOT NULL,
        body        TEXT,
        deleted     INTEGER NOT NULL DEFAULT 0,
        hlc_wall    INTEGER NOT NULL,
        hlc_logical INTEGER NOT NULL,
        device      TEXT    NOT NULL,
        PRIMARY KEY (collection, id)
    );
    "#,
];

pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

pub fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as u32)?;
        tx.commit()?;
    }
    Ok(())
}
