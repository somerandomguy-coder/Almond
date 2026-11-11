//! Phase 2 module "capture": recording, live captions, post-meeting transcription and speech
//! models (docs/PHASE2.md, "Module `capture`").
//!
//! | Piece | Where |
//! |---|---|
//! | `recording.start` / `stop` / `status`, `audio.devices`, crash recovery | [`recording`] |
//! | live captions (`transcript.update`) | [`live`] |
//! | `transcribe.local` processor (concurrent transcription + diarization) | [`transcribe`] |
//! | speech engines (routing table, model cache, test overrides) | [`engines`] |
//! | `speech_models` / `download_speech_model` (used by processing's `models.*`) | [`models`] |
//! | `audio.url` (Tauri asset protocol, mixed playback file) | [`playback`] |
//! | `meetings.import` / `meetings.importCancel` (audio/video file → imported meeting) | [`import`] |
//!
//! Files on disk (under `CoreConfig::data_dir`):
//! - `recordings/<meeting>/<started_at_ms>/`: crash-safe Opus chunks of a recording in progress
//!   (`almond_audio::SessionRecorder`) plus `almond.json` (meeting id, offset). Recovered at start-up
//!   and before every new recording; removed once the tracks are in the blob store.
//! - `playback/<meeting>.opus`: mic + system mixed for the audio player (a cache, safe to delete).

pub mod engines;
pub mod import;
pub mod live;
mod mictest;
pub mod models;
pub mod playback;
pub mod recording;
pub mod transcribe;

pub use engines::SpeechOverride;
pub use models::{download_speech_model, speech_models};
pub use transcribe::{TRANSCRIBE_LOCAL, speech_capabilities};

use crate::error::ApiResult;
use crate::router::{NoParams, Router};
use crate::{Core, Shared};
use almond_types::Result;
use almond_types::api::{RecordingStatus, StartRecordingParams};
use almond_types::audio::{AudioBackend, AudioDeviceInfo};
use almond_types::meeting::AudioChannel;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub struct State {
    pub(crate) rec: recording::Recorder,
    pub(crate) speech: Arc<engines::Speech>,
    pub(crate) downloads: std::sync::Mutex<std::collections::BTreeSet<String>>,
    pub(crate) mic_test: mictest::MicTest,
}

impl State {
    pub(crate) async fn init(shared: &Shared) -> Result<State> {
        let speech = Arc::new(engines::Speech::new(shared.cfg.models_dir.clone()));
        shared
            .registry
            .register(Arc::new(transcribe::TranscribeLocal::new(speech.clone())))?;
        Ok(State {
            rec: recording::Recorder::new(),
            speech,
            downloads: Default::default(),
            mic_test: Default::default(),
        })
    }

    /// Stop a recording that is still running (its chunks stay recoverable either way).
    pub(crate) async fn shutdown(&self) {
        self.rec.abort().await;
    }

    /// Record from `backend` instead of the system's devices (tests, demos, file import).
    /// `None` restores the default (cpal: microphone + WASAPI/PulseAudio loopback).
    pub fn set_audio_backend(&self, backend: Option<Arc<dyn AudioBackend>>) {
        self.rec.set_backend(backend);
    }

    /// Use these engines instead of the routed native ones (tests with mock engines).
    pub fn set_speech_override(&self, o: Option<SpeechOverride>) {
        self.speech.set_override(o);
    }

    /// Current recording status (same value as `recording.status`).
    pub fn status(&self) -> RecordingStatus {
        self.rec.status()
    }
}

#[derive(Debug, Deserialize)]
struct AudioUrlParams {
    meeting_id: almond_types::Id,
    #[serde(default)]
    channel: Option<AudioChannel>,
}

#[derive(Debug, Serialize)]
struct AudioUrl {
    url: Option<String>,
}

/// Setting `audio.mic_device`: the microphone chosen in the mic test (`audio.devices` id).
pub(crate) const MIC_DEVICE: &str = "audio.mic_device";

pub(crate) fn preferred_mic(core: &Core) -> Option<String> {
    core.setting_as::<String>(MIC_DEVICE)
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
}

pub(crate) fn register(r: &mut Router) {
    r.add(
        "recording.start",
        |core, p: StartRecordingParams| async move {
            ApiResult::Ok(recording::start(&core, p).await?)
        },
    );
    r.add("recording.stop", |core, _: NoParams| async move {
        ApiResult::Ok(recording::stop(&core).await?)
    });
    r.add("recording.status", |core, _: NoParams| async move {
        ApiResult::Ok(core.capture.status())
    });
    r.add("audio.devices", |core, _: NoParams| async move {
        let devs: Vec<AudioDeviceInfo> = core.capture.rec.list_devices().await?;
        ApiResult::Ok(devs)
    });
    r.add(
        "audio.testStart",
        |core, p: mictest::TestStartParams| async move {
            ApiResult::Ok(mictest::start(&core, p).await?)
        },
    );
    r.add("audio.testStop", |core, _: NoParams| async move {
        mictest::stop(&core).await;
        ApiResult::Ok(serde_json::Value::Null)
    });
    r.add("audio.url", |core, p: AudioUrlParams| async move {
        let path = playback::playable_path(&core, &p.meeting_id, p.channel).await?;
        ApiResult::Ok(AudioUrl {
            url: path.map(|p| playback::asset_url(&p, &core.cfg.platform)),
        })
    });
    r.add(import::MEETINGS_IMPORT, |core, p: import::ImportParams| async move {
        ApiResult::Ok(import::import(&core, p).await?)
    });
    r.add(
        import::MEETINGS_IMPORT_CANCEL,
        |_core, p: import::ImportCancelParams| async move {
            ApiResult::Ok(import::cancel(&p.meeting_id))
        },
    );
}

pub(crate) fn start(core: Arc<Core>) {
    // Finish recordings a crash interrupted, so their audio reaches the meeting and processing.
    tokio::spawn(async move {
        if let Err(e) = recording::recover_unfinished(&core).await {
            tracing::warn!("recovering unfinished recordings: {e}");
        }
    });
}
