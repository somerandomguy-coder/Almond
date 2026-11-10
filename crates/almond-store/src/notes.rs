//! Loro notes docs: helpers to build `notes.update` events, materialize a doc, and compact the
//! stored updates into a snapshot (local cache only — the update events stay in the log so new
//! peers can still be served).

use crate::sqlite::{SqliteStore, sqlerr};
use almond_types::event::EventBody;
use almond_types::store::Store;
use almond_types::{Error, Id, Result};
use base64::Engine;
use loro::{ExportMode, LoroDoc, LoroValue};
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

/// Check that `update` is a decodable Loro update (or snapshot) before it enters the log.
/// Updates whose causal dependencies are missing still pass (Loro keeps them pending).
pub fn validate_update(update: &[u8]) -> Result<()> {
    if update.is_empty() {
        return Err(Error::Invalid("empty notes update".into()));
    }
    LoroDoc::new()
        .import(update)
        .map(|_| ())
        .map_err(|e| Error::Invalid(format!("not a Loro update: {e}")))
}

/// Inline ProseMirror nodes (no line break after them).
const INLINE_NODES: &[&str] = &["image", "mention", "emoji", "text"];

/// Plain text of a notes doc, one line per block, list items prefixed with `- `.
///
/// Understands the layout the desktop editor writes (loro-prosemirror: root map `doc` with
/// `nodeName` / `attributes` / `children`, text runs as `LoroText`). Any other root containers
/// are flattened after it (strings in key order), so plain `LoroText` notes work too.
pub fn doc_text(doc: &LoroDoc) -> String {
    let mut out = String::new();
    if let LoroValue::Map(m) = doc.get_deep_value() {
        let mut keys: Vec<&String> = m.keys().collect();
        keys.sort_by_key(|k| (k.as_str() != "doc", k.to_string()));
        for k in keys {
            walk(&m[k], &mut out);
            newline(&mut out);
        }
    }
    tidy(&out)
}

fn newline(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn walk(v: &LoroValue, out: &mut String) {
    match v {
        LoroValue::String(s) => out.push_str(s),
        LoroValue::List(l) => l.iter().for_each(|x| walk(x, out)),
        LoroValue::Map(m) => match m.get("children") {
            Some(children) => {
                let name = match m.get("nodeName") {
                    Some(LoroValue::String(s)) => s.to_string(),
                    _ => String::new(),
                };
                match name.as_str() {
                    "hardBreak" => return out.push('\n'),
                    "listItem" | "taskItem" => {
                        newline(out);
                        out.push_str("- ");
                    }
                    _ => {}
                }
                walk(children, out);
                if !INLINE_NODES.contains(&name.as_str()) {
                    newline(out);
                }
            }
            None => {
                let mut keys: Vec<&String> = m.keys().filter(|k| *k != "attributes").collect();
                keys.sort();
                for k in keys {
                    walk(&m[k], out);
                }
            }
        },
        _ => {}
    }
}

/// Trim line ends, collapse runs of blank lines, trim the whole text.
fn tidy(s: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in s.lines().map(str::trim_end) {
        if line.trim().is_empty() || line.trim() == "-" {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.push_str(line);
    }
    out.trim().to_string()
}

impl SqliteStore {
    /// Plain text of a meeting's notes ([`doc_text`] of the materialized doc).
    pub fn notes_text(&self, doc: &Id) -> Result<String> {
        Ok(doc_text(&self.notes_doc(doc)?))
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use loro::{LoroList, LoroMap, LoroText};

    /// Build a block the way loro-prosemirror does: `{ nodeName, attributes, children }`.
    fn block(list: &LoroList, idx: usize, name: &str) -> LoroList {
        let m = list.insert_container(idx, LoroMap::new()).unwrap();
        m.insert("nodeName", name).unwrap();
        m.insert_container("attributes", LoroMap::new()).unwrap();
        m.insert_container("children", LoroList::new()).unwrap()
    }

    fn text(list: &LoroList, idx: usize, s: &str) {
        let t = list.insert_container(idx, LoroText::new()).unwrap();
        t.insert(0, s).unwrap();
    }

    #[test]
    fn prosemirror_layout_to_text() {
        let d = LoroDoc::new();
        let root = d.get_map("doc");
        root.insert("nodeName", "doc").unwrap();
        let kids = root.insert_container("children", LoroList::new()).unwrap();
        let h = block(&kids, 0, "heading");
        text(&h, 0, "Họp nhóm");
        let p = block(&kids, 1, "paragraph");
        text(&p, 0, "Chốt ngân sách ");
        text(&p, 1, "Q4");
        let ul = block(&kids, 2, "bulletList");
        let li = block(&ul, 0, "listItem");
        let lp = block(&li, 0, "paragraph");
        text(&lp, 0, "Lan gửi báo cáo");
        let li2 = block(&ul, 1, "listItem");
        let lp2 = block(&li2, 0, "paragraph");
        text(&lp2, 0, "Ship v2");
        d.commit();
        assert_eq!(
            doc_text(&d),
            "Họp nhóm\nChốt ngân sách Q4\n- Lan gửi báo cáo\n- Ship v2"
        );
    }

    #[test]
    fn plain_text_root_and_empty_doc() {
        let d = LoroDoc::new();
        assert_eq!(doc_text(&d), "");
        d.get_text("notes")
            .insert(0, "  hello\n\n\n\nworld  ")
            .unwrap();
        d.commit();
        assert_eq!(doc_text(&d), "hello\n\nworld");
    }
}
