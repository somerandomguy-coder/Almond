//! Disk space: deleting meeting audio once the text is good enough.
//!
//! Audio is ~99 % of Almond's data (~22 MB per meeting hour for mic + system; transcripts,
//! summaries and notes are ~0.1–0.3 MB). So the text always stays (citations, chat sources,
//! speaker labels and search point into it) and the audio can go:
//!
//! - `storage.deleteAudio { meeting_ids, force? }`: delete the recordings of meetings, on every
//!   device. Each track blob gets a synced tombstone (`almond_types::store::DELETED_BLOBS`); every
//!   device deletes its copy when it sees one and sync never fetches it again. Refused for a
//!   meeting without a transcript unless `force`.
//! - `storage.keepAudio { meeting_id, keep }`: ⭐ — automatic cleanup skips this meeting.
//! - Setting `storage.audio_retention_days` (number; unset/0 = off): delete audio this many days
//!   after the meeting's transcript was made, except kept meetings.
//! - `storage.meetings {}`: per meeting, the audio on this device, newest-largest first.
//! - `storage.cleanup {}`: run retention + playback-cache pruning now.
//! - The playback cache (`playback/`, mixed copies for the player) is pruned: copies of meetings
//!   whose audio is gone, copies unused for [`PLAYBACK_MAX_AGE`], and beyond [`PLAYBACK_MAX_BYTES`].
//!
//! Per-meeting state is a synced record (collection [`COLLECTION`], id = meeting id).

use crate::error::ApiResult;
use crate::router::{NoParams, Router};
use crate::{Core, Shared};
use almond_types::artifact::ArtifactKind;
use almond_types::meeting::Meeting;
use almond_types::store::{BlobStore, DELETED_BLOBS, MeetingQuery, Store};
use almond_types::time::{UnixMs, now_ms};
use almond_types::{BlobHash, DeviceId, Id, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Record collection: per-meeting audio state ([`AudioState`]).
pub const COLLECTION: &str = "meeting_audio";
/// Setting: delete audio this many days after the transcript (unset or 0 = never).
pub const RETENTION_KEY: &str = "storage.audio_retention_days";
/// Playback copies unused for this long are removed.
pub const PLAYBACK_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
/// The playback cache is trimmed (oldest first) to this size.
pub const PLAYBACK_MAX_BYTES: u64 = 500 * 1024 * 1024;
const SWEEP_EVERY: Duration = Duration::from_secs(10 * 60);

/// Synced per-meeting audio state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioState {
    pub meeting_id: Id,
    /// ⭐ keep the audio: automatic cleanup skips it.
    #[serde(default)]
    pub keep: bool,
    /// When the user (or the retention rule) deleted the audio.
    #[serde(default)]
    pub deleted_at: Option<UnixMs>,
    #[serde(default)]
    pub deleted_by: Option<DeviceId>,
    /// `manual` | `retention`.
    #[serde(default)]
    pub deleted_reason: Option<String>,
}

/// `storage.meetings` row.
#[derive(Debug, Clone, Serialize)]
pub struct MeetingStorage {
    pub meeting_id: Id,
    pub title: String,
    pub started_at: UnixMs,
    /// Bytes of this meeting's audio on this device.
    pub audio_bytes: u64,
    /// Audio tracks recorded for the meeting (whether or not they're on this device).
    pub tracks: usize,
    pub keep_audio: bool,
    pub audio_deleted_at: Option<UnixMs>,
    /// When the (latest) transcript was made; `None` = not transcribed yet.
    pub transcribed_at: Option<UnixMs>,
}

pub struct State {}

impl State {
    pub(crate) async fn init(_shared: &Shared) -> Result<State> {
        Ok(State {})
    }
    pub(crate) async fn shutdown(&self) {}
}

#[derive(Deserialize)]
struct DeleteParams {
    meeting_ids: Vec<Id>,
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize)]
struct KeepParams {
    meeting_id: Id,
    keep: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct DeleteResult {
    pub deleted: usize,
    pub freed_bytes: u64,
    /// (meeting id, why it was skipped)
    pub skipped: Vec<(Id, String)>,
}

pub(crate) fn register(r: &mut Router) {
    r.add("storage.meetings", |core, _: NoParams| async move {
        ApiResult::Ok(meetings(&core)?)
    });
    r.add(
        "storage.audioState",
        |core, p: crate::router::IdParams| async move { ApiResult::Ok(audio_state(&core, &p.id)?) },
    );
    r.add("storage.deleteAudio", |core, p: DeleteParams| async move {
        ApiResult::Ok(delete_audio(&core, &p.meeting_ids, p.force, "manual")?)
    });
    r.add("storage.keepAudio", |core, p: KeepParams| async move {
        let mut st = audio_state(&core, &p.meeting_id)?;
        st.keep = p.keep;
        core.put_record(COLLECTION, p.meeting_id.as_str(), &st)?;
        ApiResult::Ok(Value::Null)
    });
    r.add("storage.cleanup", |core, _: NoParams| async move {
        ApiResult::Ok(sweep(&core)?)
    });
}

pub(crate) fn start(core: Arc<Core>) {
    tokio::spawn(async move {
        // Let start-up settle (sync, job loop) before touching files.
        tokio::time::sleep(Duration::from_secs(20)).await;
        loop {
            let c = core.clone();
            match tokio::task::spawn_blocking(move || sweep(&c)).await {
                Ok(Ok(r)) if r.freed_bytes > 0 => {
                    tracing::info!("storage: freed {} bytes", r.freed_bytes)
                }
                Ok(Err(e)) => tracing::warn!("storage sweep: {e}"),
                _ => {}
            }
            tokio::time::sleep(SWEEP_EVERY).await;
        }
    });
}

/// The meeting's audio state (default: not kept, not deleted).
pub fn audio_state(core: &Core, meeting: &Id) -> Result<AudioState> {
    Ok(core
        .record::<AudioState>(COLLECTION, meeting.as_str())?
        .unwrap_or(AudioState {
            meeting_id: meeting.clone(),
            ..Default::default()
        }))
}

fn transcribed_at(core: &Core, m: &Meeting) -> Result<Option<UnixMs>> {
    Ok(core
        .store()
        .list_artifacts(&m.id, Some(ArtifactKind::TRANSCRIPT))?
        .iter()
        .map(|a| a.created_at.wall_ms)
        .max())
}

fn blob_size(blobs: &dyn BlobStore, h: &BlobHash) -> u64 {
    match blobs.path(h) {
        Ok(Some(p)) => std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
        _ => 0,
    }
}

fn all_meetings(core: &Core) -> Result<Vec<Meeting>> {
    core.store().list_meetings(&MeetingQuery {
        limit: Some(100_000),
        ..Default::default()
    })
}

/// Meetings with audio on this device first (largest first), then the rest by date.
pub fn meetings(core: &Core) -> Result<Vec<MeetingStorage>> {
    let blobs = core.blobs().clone();
    let mut out = vec![];
    for m in all_meetings(core)? {
        let st = audio_state(core, &m.id)?;
        let audio_bytes = m
            .audio
            .iter()
            .map(|t| blob_size(blobs.as_ref(), &t.blob))
            .sum();
        out.push(MeetingStorage {
            transcribed_at: transcribed_at(core, &m)?,
            meeting_id: m.id.clone(),
            title: m.title.clone(),
            started_at: m.started_at,
            audio_bytes,
            tracks: m.audio.len(),
            keep_audio: st.keep,
            audio_deleted_at: st.deleted_at,
        });
    }
    out.sort_by(|a, b| {
        b.audio_bytes
            .cmp(&a.audio_bytes)
            .then(b.started_at.cmp(&a.started_at))
    });
    Ok(out)
}

/// Delete the audio of `ids` everywhere (synced tombstones) and here (files + playback copies).
pub fn delete_audio(core: &Core, ids: &[Id], force: bool, reason: &str) -> Result<DeleteResult> {
    let mut res = DeleteResult::default();
    let blobs = core.blobs().clone();
    // Blobs are content-addressed: a file shared with a meeting that keeps its audio stays.
    let batch: BTreeSet<&Id> = ids.iter().collect();
    let mut in_use: BTreeSet<BlobHash> = BTreeSet::new();
    for m in all_meetings(core)? {
        if batch.contains(&m.id) || audio_state(core, &m.id)?.deleted_at.is_some() {
            continue;
        }
        in_use.extend(m.audio.iter().map(|t| t.blob.clone()));
    }
    for id in ids {
        let Some(m) = core.store().get_meeting(id)? else {
            res.skipped.push((id.clone(), "meeting not found".into()));
            continue;
        };
        if m.audio.is_empty() {
            res.skipped.push((id.clone(), "no audio".into()));
            continue;
        }
        if audio_state(core, &m.id)?.deleted_at.is_some() {
            res.skipped
                .push((id.clone(), "audio already deleted".into()));
            continue;
        }
        if m.ended_at.is_none() {
            res.skipped.push((id.clone(), "still recording".into()));
            continue;
        }
        if !force && transcribed_at(core, &m)?.is_none() {
            res.skipped.push((
                id.clone(),
                "not transcribed yet (the audio is the only copy of what was said)".into(),
            ));
            continue;
        }
        for t in &m.audio {
            if in_use.contains(&t.blob) {
                continue;
            }
            res.freed_bytes += blob_size(blobs.as_ref(), &t.blob);
            core.put_record(
                DELETED_BLOBS,
                &t.blob.0,
                &serde_json::json!({ "meeting_id": m.id, "at": now_ms() }),
            )?;
            blobs.delete(&t.blob)?;
        }
        res.freed_bytes += remove_playback(&core.cfg.data_dir.join("playback"), Some(&m.id));
        let mut st = audio_state(core, &m.id)?;
        st.deleted_at = Some(now_ms());
        st.deleted_by = Some(core.device_id().clone());
        st.deleted_reason = Some(reason.into());
        core.put_record(COLLECTION, m.id.as_str(), &st)?;
        res.deleted += 1;
    }
    Ok(res)
}

/// Apply other devices' deletions, the retention rule, and prune the playback cache.
pub fn sweep(core: &Core) -> Result<DeleteResult> {
    let blobs = core.blobs().clone();
    let mut res = DeleteResult::default();

    // 1. deletions made on other devices (or interrupted here)
    let deleted: BTreeSet<String> = core
        .store()
        .list_records(DELETED_BLOBS)?
        .into_iter()
        .map(|r| r.id)
        .collect();
    for h in &deleted {
        let h = BlobHash(h.clone());
        if blobs.has(&h).unwrap_or(false) {
            res.freed_bytes += blob_size(blobs.as_ref(), &h);
            blobs.delete(&h)?;
        }
    }

    // 2. retention: N days after the transcript, unless kept
    let days = core
        .setting_as::<f64>(RETENTION_KEY)
        .ok()
        .flatten()
        .unwrap_or(0.0);
    if days > 0.0 {
        let cutoff = now_ms() - (days * 86_400_000.0) as i64;
        let due: Vec<Id> = all_meetings(core)?
            .into_iter()
            .filter(|m| m.audio.iter().any(|t| blobs.has(&t.blob).unwrap_or(false)))
            .filter(|m| {
                let st = audio_state(core, &m.id).unwrap_or_default();
                !st.keep
                    && st.deleted_at.is_none()
                    && transcribed_at(core, m)
                        .ok()
                        .flatten()
                        .is_some_and(|t| t < cutoff)
            })
            .map(|m| m.id)
            .collect();
        if !due.is_empty() {
            let r = delete_audio(core, &due, false, "retention")?;
            res.deleted += r.deleted;
            res.freed_bytes += r.freed_bytes;
            res.skipped.extend(r.skipped);
        }
    }

    // 3. playback cache
    res.freed_bytes += prune_playback(core, &core.cfg.data_dir.join("playback"));
    Ok(res)
}

/// Remove playback copies of one meeting (`Some`) — files are named `<meeting>-<key>.opus`.
fn remove_playback(dir: &Path, meeting: Option<&Id>) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut freed = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if meeting.is_some_and(|m| name.starts_with(&format!("{}-", m.as_str())))
            && let Ok(md) = e.metadata()
            && md.is_file()
            && std::fs::remove_file(e.path()).is_ok()
        {
            freed += md.len();
        }
    }
    freed
}

/// Drop copies whose meeting no longer has local audio, copies unused for a week, and the oldest
/// beyond the size cap. Leftover temp dirs (`.<meeting>-<key>`) of interrupted builds go too.
fn prune_playback(core: &Core, dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let blobs = core.blobs().clone();
    let now = std::time::SystemTime::now();
    let mut keep = vec![];
    let mut freed = 0;
    for e in rd.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            let old = md
                .modified()
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > Duration::from_secs(3600));
            if name.starts_with('.') && old {
                let _ = std::fs::remove_dir_all(&path);
            }
            continue;
        }
        let meeting = name.split('-').next().map(|s| Id(s.to_string()));
        let has_audio = meeting
            .and_then(|m| core.store().get_meeting(&m).ok().flatten())
            .is_some_and(|m| m.audio.iter().any(|t| blobs.has(&t.blob).unwrap_or(false)));
        // last use: the newer of modified / accessed
        let used = md
            .accessed()
            .ok()
            .into_iter()
            .chain(md.modified().ok())
            .max()
            .unwrap_or(now);
        let stale = now.duration_since(used).unwrap_or_default() > PLAYBACK_MAX_AGE;
        if !has_audio || stale {
            if std::fs::remove_file(&path).is_ok() {
                freed += md.len();
            }
        } else {
            keep.push((used, md.len(), path));
        }
    }
    // size cap, oldest first
    keep.sort_by_key(|(used, _, _)| *used);
    let mut total: u64 = keep.iter().map(|(_, n, _)| n).sum();
    for (_, n, path) in keep {
        if total <= PLAYBACK_MAX_BYTES {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            freed += n;
            total -= n;
        }
    }
    freed
}
