//! Loro notes docs: helpers to build `notes.update` events, materialize a doc, and compact the
//! stored updates into a snapshot (local cache only — the update events stay in the log so new
//! peers can still be served).

use crate::sqlite::{SqliteStore, sqlerr};
use almond_types::event::EventBody;
use almond_types::store::Store;
use almond_types::{Error, Id, Result};
use base64::Engine;
use loro::{ExportMode, LoroDoc};
use rusqlite::{OptionalExtension, params};

/// Wrap raw Loro update bytes as a `notes.update` event body.
pub fn notes_update_event(doc: &Id, update: &[u8]) -> EventBody {
    EventBody::NotesUpdate {
        doc: doc.clone(),
        update_b64: base64::engine::general_purpose::STANDARD.encode(update),
    }
}

/// Export the changes in `doc` made since `since` as an update suitable for [`notes_update_event`].
pub fn export_since(doc: &LoroDoc, since: &loro::VersionVector) -> Result<Vec<u8>> {
    doc.export(ExportMode::updates(since))
        .map_err(|e| Error::Other(format!("loro export: {e}")))
}

impl SqliteStore {
    /// Materialize a notes doc from all stored updates (snapshot first if compacted).
    pub fn notes_doc(&self, doc: &Id) -> Result<LoroDoc> {
        let updates = self.notes_updates(doc)?;
        let d = LoroDoc::new();
        if !updates.is_empty() {
            d.import_batch(&updates)
                .map_err(|e| Error::Other(format!("loro import: {e}")))?;
        }
        Ok(d)
    }

    /// Fold all currently stored updates of `doc` into one snapshot so `notes_updates` returns
    /// `[snapshot, newer updates…]`. Returns the number of updates folded.
    pub fn compact_notes(&self, doc: &Id) -> Result<usize> {
        self.with_conn(|c| {
            let tx = c.transaction().map_err(sqlerr)?;
            let prev: Option<(Vec<u8>, i64)> = tx
                .query_row(
                    "SELECT snapshot, covered_id FROM notes_snapshots WHERE doc = ?1",
                    params![doc.0],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(sqlerr)?;
            let covered = prev.as_ref().map(|p| p.1).unwrap_or(0);
            let mut parts: Vec<Vec<u8>> = prev.map(|p| vec![p.0]).unwrap_or_default();
            let mut max_id = covered;
            let mut folded = 0usize;
            {
                let mut st = tx
                    .prepare("SELECT id, update_blob FROM notes WHERE doc = ?1 AND id > ?2 ORDER BY id")
                    .map_err(sqlerr)?;
                let mut rows = st.query(params![doc.0, covered]).map_err(sqlerr)?;
                while let Some(r) = rows.next().map_err(sqlerr)? {
                    max_id = r.get(0).map_err(sqlerr)?;
                    parts.push(r.get(1).map_err(sqlerr)?);
                    folded += 1;
                }
            }
            if folded == 0 {
                return Ok(0);
            }
            let d = LoroDoc::new();
            d.import_batch(&parts)
                .map_err(|e| Error::Other(format!("loro import: {e}")))?;
            let snapshot = d
                .export(ExportMode::Snapshot)
                .map_err(|e| Error::Other(format!("loro export: {e}")))?;
            tx.execute(
                "INSERT INTO notes_snapshots (doc, snapshot, covered_id) VALUES (?1, ?2, ?3)
                 ON CONFLICT(doc) DO UPDATE SET snapshot = excluded.snapshot, covered_id = excluded.covered_id",
                params![doc.0, snapshot, max_id],
            )
            .map_err(sqlerr)?;
            tx.commit().map_err(sqlerr)?;
            Ok(folded)
        })
    }
}
