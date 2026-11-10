//! Speech models for the processing module's `models.list` / `models.download`
//! (docs/PHASE2.md, "Cross-module interfaces").

use crate::Core;
use almond_stt::models::{ModelManager, ModelRole, SourceKind};
use almond_types::api::ModelEntry;
use almond_types::stt::ModelDownloadProgress;
use almond_types::{Error, Result};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Notification name for [`ModelDownloadProgress`] (docs/API.md; not yet a constant in
/// `almond_types::api::notifications`, see docs/contract-requests/i-capture.md).
pub const MODELS_DOWNLOAD_PROGRESS: &str = "models.downloadProgress";

/// Every speech model this build knows (registry + `registry.local.json`), with install state.
/// `kind`: "stt" or "diarize". `note` says when a model is a default for this device or must be
/// converted locally.
pub fn speech_models(core: &Core) -> Result<Vec<ModelEntry>> {
    let mgr = ModelManager::new(&core.cfg.models_dir)?;
    let defaults = core.capture.speech.table().model_ids();
    Ok(mgr
        .registry
        .models
        .iter()
        .map(|e| {
            let mut notes = vec![];
            if defaults.contains(&e.id) {
                notes.push("default on this device".to_string());
            }
            if e.source.kind == SourceKind::Convert {
                notes.push("converted locally, not downloadable".to_string());
            }
            if let Some(n) = &e.notes {
                notes.push(n.clone());
            }
            ModelEntry {
                id: e.id.clone(),
                kind: match e.role {
                    ModelRole::Stt => "stt",
                    ModelRole::Segmentation | ModelRole::Embedding => "diarize",
                }
                .into(),
                installed: mgr.is_installed(&e.id),
                size_bytes: e.source.size,
                languages: e.languages.iter().map(|l| l.0.clone()).collect(),
                note: (!notes.is_empty()).then(|| notes.join("; ")),
            }
        })
        .collect())
}

/// Download, verify (SHA-256) and install a speech model; resumes a partial download. Emits
/// `models.downloadProgress` (at most ~4 per second, plus the final one). A second call for a
/// model already downloading fails with `Invalid`.
pub async fn download_speech_model(core: &Arc<Core>, id: &str) -> Result<()> {
    {
        let mut d = core
            .capture
            .downloads
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if !d.insert(id.to_string()) {
            return Err(Error::Invalid(format!("{id} is already downloading")));
        }
    }
    let c = core.clone();
    let id2 = id.to_string();
    let r = tokio::task::spawn_blocking(move || -> Result<()> {
        let mgr = ModelManager::new(&c.cfg.models_dir)?;
        mgr.entry(&id2)?;
        let mut last = Instant::now() - Duration::from_secs(1);
        let mut last_done = 0;
        let mut total_seen = None;
        let notifier = c.notifier.clone();
        let mut progress = |done: u64, total: Option<u64>| {
            last_done = done;
            total_seen = total;
            if last.elapsed() >= Duration::from_millis(250) {
                last = Instant::now();
                notifier.emit(
                    MODELS_DOWNLOAD_PROGRESS,
                    ModelDownloadProgress {
                        id: id2.clone(),
                        done_bytes: done,
                        total_bytes: total,
                    },
                );
            }
        };
        mgr.ensure(&id2, &mut progress)?;
        let total = total_seen.or(Some(last_done));
        c.notifier.emit(
            MODELS_DOWNLOAD_PROGRESS,
            ModelDownloadProgress {
                id: id2.clone(),
                done_bytes: total.unwrap_or(last_done),
                total_bytes: total,
            },
        );
        Ok(())
    })
    .await
    .map_err(Error::other)
    .and_then(|r| r);
    core.capture
        .downloads
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(id);
    r
}
