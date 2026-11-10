//! Cross-module interfaces this module calls (docs/PHASE2.md, "Cross-module interfaces"), stubbed
//! until the `capture` and `library` branches land.
//!
//! TODO(integrator): when merging, replace each body with a call to the provider:
//! - `speech_models` -> `crate::capture::speech_models(core)`
//! - `download_speech_model` -> `crate::capture::download_speech_model(core, id).await`
//!
//! `notes_text` is already wired to `crate::library::notes_text`.

use crate::Core;
use almond_types::api::ModelEntry;
use almond_types::{Error, Id, Result};
use std::sync::Arc;

/// Speech models (STT, diarization, VAD) known to `capture`, with install state.
pub(crate) fn speech_models(_core: &Core) -> Result<Vec<ModelEntry>> {
    // TODO(capture): crate::capture::speech_models(core)
    Ok(vec![])
}

/// Download a speech model (emits `models.downloadProgress`).
pub(crate) async fn download_speech_model(_core: &Arc<Core>, id: &str) -> Result<()> {
    // TODO(capture): crate::capture::download_speech_model(core, id).await
    Err(Error::Unsupported(format!(
        "speech model download is not wired yet ({id})"
    )))
}

/// Plain text of a meeting's Loro notes doc.
pub(crate) fn notes_text(core: &Core, doc: &Id) -> Result<String> {
    crate::library::notes_text(core, doc)
}
