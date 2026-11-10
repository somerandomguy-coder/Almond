//! `meetings.*`, `notes.*`, `search.query`, `storage.usage`.

use super::add;
use crate::Core;
use crate::error::ApiResult;
use crate::router::{IdParams, NoParams, Router};
use almond_types::api::{CreateMeetingParams, MeetingDetail, NotesSnapshot, UpdateMeetingParams};
use almond_types::artifact::Artifact;
use almond_types::event::EventBody;
use almond_types::meeting::Meeting;
use almond_types::store::{MeetingQuery, Store};
use almond_types::time::now_ms;
use almond_types::{Error, Id, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

/// Compact a notes doc into a snapshot once it has this many stored updates (the editor sends
/// one update per transaction, so long notes accumulate thousands).
const COMPACT_NOTES_AFTER: usize = 64;

/// Plain text of a meeting's Loro notes (one line per block, list items as `- `). Used by the
/// processing module for `enhance_notes.llm` (`params.user_notes`).
pub fn notes_text(core: &Core, doc: &Id) -> Result<String> {
    core.store().notes_text(doc)
}

/// The device's IANA time zone, if the OS reports one.
pub(super) fn local_tz() -> Option<String> {
    iana_time_zone::get_timezone().ok()
}

fn meeting_or_404(core: &Core, id: &Id) -> Result<Meeting> {
    core.store()
        .get_meeting(id)?
        .ok_or_else(|| Error::NotFound(format!("meeting {id}")))
}

/// Latest non-superseded artifact per kind first (in the store's latest-first order), then all
/// older and superseded versions.
pub(super) fn order_artifacts(all: Vec<Artifact>) -> Vec<Artifact> {
    let superseded: HashSet<Id> = all.iter().filter_map(|a| a.supersedes.clone()).collect();
    let mut kinds = HashSet::new();
    let (mut heads, mut rest) = (vec![], vec![]);
    for a in all {
        if !superseded.contains(&a.id) && kinds.insert(a.kind.0.clone()) {
            heads.push(a);
        } else {
            rest.push(a);
        }
    }
    heads.extend(rest);
    heads
}

fn get_detail(core: &Core, id: &Id) -> Result<MeetingDetail> {
    let meeting = meeting_or_404(core, id)?;
    let artifacts = order_artifacts(core.store().list_artifacts(id, None)?);
    let calendar_event = match &meeting.calendar_event {
        Some(r) => core.store().calendar_event(&r.source, &r.event_id)?,
        None => None,
    };
    Ok(MeetingDetail {
        meeting,
        artifacts,
        calendar_event,
    })
}

fn create(core: &Core, p: CreateMeetingParams) -> Result<Meeting> {
    let event = match &p.calendar_event {
        Some(r) => core.store().calendar_event(&r.source, &r.event_id)?,
        None => None,
    };
    let title = p
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or_else(|| event.map(|e| e.title))
        .unwrap_or_default();
    let m = Meeting {
        id: Id::new(),
        title,
        started_at: now_ms(),
        ended_at: None,
        kind: p.kind,
        calendar_event: p.calendar_event,
        languages: vec![],
        audio: vec![],
        notes_doc: Id::new(),
        tags: vec![],
        tz: local_tz(),
    };
    core.store()
        .append_local(EventBody::MeetingUpsert(m.clone()))?;
    Ok(m)
}

fn update(core: &Core, p: UpdateMeetingParams) -> Result<Meeting> {
    let mut m = meeting_or_404(core, &p.id)?;
    if let Some(t) = p.title {
        m.title = t.trim().to_string();
    }
    if let Some(mut tags) = p.tags {
        tags.retain(|t| !t.trim().is_empty());
        tags.dedup();
        m.tags = tags;
    }
    core.store()
        .append_local(EventBody::MeetingUpsert(m.clone()))?;
    Ok(m)
}

#[derive(Deserialize)]
struct DocParams {
    doc: Id,
}

#[derive(Deserialize)]
struct ApplyUpdateParams {
    doc: Id,
    update_b64: String,
}

fn notes_get(core: &Core, doc: Id) -> Result<NotesSnapshot> {
    let store = core.store();
    let mut updates = store.notes_updates(&doc)?;
    if updates.len() > COMPACT_NOTES_AFTER {
        store.compact_notes(&doc)?;
        updates = store.notes_updates(&doc)?;
    }
    Ok(NotesSnapshot {
        doc,
        updates_b64: updates.iter().map(|u| B64.encode(u)).collect(),
    })
}

fn notes_apply(core: &Core, p: ApplyUpdateParams) -> Result<()> {
    let bytes = B64
        .decode(p.update_b64.trim())
        .map_err(|e| Error::Invalid(format!("update_b64: {e}")))?;
    almond_store::validate_update(&bytes)?;
    core.store()
        .append_local(almond_store::notes_update_event(&p.doc, &bytes))?;
    Ok(())
}

#[derive(Deserialize)]
struct SearchParams {
    text: String,
    limit: Option<u32>,
}

/// `storage.usage` result. `audio_bytes` is the part of `blobs_bytes` holding audio (by the
/// announced mime type); `models_bytes` is the speech models dir (LLMs live in Ollama).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageUsage {
    pub db_bytes: u64,
    pub blobs_bytes: u64,
    pub models_bytes: u64,
    pub audio_bytes: u64,
}

fn file_len(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// Total size of the files under `dir` (0 if missing). Symlinks are not followed.
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
                _ => {}
            }
        }
    }
    total
}

fn storage_usage(core: &Core) -> Result<StorageUsage> {
    let data = &core.cfg.data_dir;
    let db_bytes = ["almond.db", "almond.db-wal", "almond.db-shm"]
        .iter()
        .map(|f| file_len(&data.join(f)))
        .sum();
    let blobs = core.blobs();
    let mut audio_bytes = 0;
    for b in core.store().known_blobs()? {
        if (b.mime.starts_with("audio/") || b.mime.starts_with("video/"))
            && let Ok(Some(n)) = blobs.size(&b.hash)
        {
            audio_bytes += n;
        }
    }
    Ok(StorageUsage {
        db_bytes,
        blobs_bytes: dir_size(blobs.root()),
        models_bytes: dir_size(&core.cfg.models_dir),
        audio_bytes,
    })
}

pub(super) fn register(r: &mut Router) {
    add(r, "meetings.list", |core, q: MeetingQuery| async move {
        ApiResult::Ok(core.store().list_meetings(&q)?)
    });
    add(r, "meetings.get", |core, p: IdParams| async move {
        ApiResult::Ok(get_detail(&core, &p.id)?)
    });
    add(
        r,
        "meetings.create",
        |core, p: CreateMeetingParams| async move { ApiResult::Ok(create(&core, p)?) },
    );
    add(
        r,
        "meetings.update",
        |core, p: UpdateMeetingParams| async move { ApiResult::Ok(update(&core, p)?) },
    );
    add(r, "meetings.delete", |core, p: IdParams| async move {
        meeting_or_404(&core, &p.id)?;
        core.store()
            .append_local(EventBody::MeetingDelete { id: p.id })?;
        ApiResult::Ok(Value::Null)
    });
    add(r, "notes.get", |core, p: DocParams| async move {
        ApiResult::Ok(notes_get(&core, p.doc)?)
    });
    add(
        r,
        "notes.applyUpdate",
        |core, p: ApplyUpdateParams| async move {
            notes_apply(&core, p)?;
            ApiResult::Ok(Value::Null)
        },
    );
    add(r, "search.query", |core, p: SearchParams| async move {
        let limit = p.limit.unwrap_or(20).clamp(1, 200);
        ApiResult::Ok(core.store().search(&p.text, limit)?)
    });
    add(r, "storage.usage", |core, _: NoParams| async move {
        let c = core.clone();
        let u = tokio::task::spawn_blocking(move || storage_usage(&c))
            .await
            .map_err(Error::other)??;
        ApiResult::Ok(u)
    });
}
