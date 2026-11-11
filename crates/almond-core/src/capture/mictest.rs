//! Microphone test (Settings → Microphone): open the mic without recording and stream its level
//! so the user can see whether Almond hears them before a meeting (a quiet laptop mic at -69 dBFS
//! made a whole recording "no speech detected" on the Fedora laptop).
//!
//! - `audio.testStart { device? }` → `{ device }`; emits `audio.testLevel` ~10/s:
//!   `{ rms_db, peak_db, level }` (`level` = RMS mapped from [-60, 0] dBFS to [0, 1], like the
//!   recording VU meter). Stops by itself after [`MAX_TEST`].
//! - `audio.testStop`; emits `audio.testStopped`.
//!
//! Never runs during a recording (both reset the capture clock); `recording.start` stops it.

use crate::Core;
use almond_types::api::StartRecordingParams;
use almond_types::meeting::AudioChannel;
use almond_types::{Error, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub(crate) const TEST_LEVEL: &str = "audio.testLevel";
pub(crate) const TEST_STOPPED: &str = "audio.testStopped";
/// A forgotten test must not keep the microphone open.
const MAX_TEST: Duration = Duration::from_secs(120);

#[derive(Default)]
pub(crate) struct MicTest(tokio::sync::Mutex<Option<(JoinHandle<()>, watch::Sender<bool>)>>);

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct TestStartParams {
    /// `audio.devices` id; None = the setting `audio.mic_device`, else the system default.
    device: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TestStarted {
    /// Name of the device being tested (best effort).
    device: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct TestLevel {
    rms_db: f32,
    peak_db: f32,
    level: f32,
}

pub(crate) async fn start(core: &Arc<Core>, p: TestStartParams) -> Result<TestStarted> {
    stop(core).await;
    let rec = &core.capture.rec;
    if rec.is_recording().await {
        return Err(Error::Invalid(
            "a recording is in progress; its level meter shows the microphone".into(),
        ));
    }
    let device = p.device.or_else(|| super::preferred_mic(core));
    let params = StartRecordingParams {
        meeting_id: None,
        create: None,
        channels: vec![AudioChannel::Mic],
        mic_device: device.clone(),
        system_device: None,
        live_transcript: false,
    };
    let (mut streams, _) = rec.open(&[AudioChannel::Mic], &params).await?;
    let mut stream = streams
        .pop()
        .ok_or_else(|| Error::NotFound("no microphone".into()))?;
    let name = rec.list_devices().await.ok().and_then(|ds| {
        ds.into_iter()
            .filter(|d| d.channel == AudioChannel::Mic)
            .find(|d| match &device {
                Some(id) => &d.id == id,
                None => d.is_default,
            })
            .map(|d| d.name)
    });

    let (tx, mut rx) = watch::channel(false);
    let notifier = core.notifier.clone();
    let task = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + MAX_TEST;
        loop {
            let next = tokio::select! {
                f = stream.next() => f,
                _ = rx.changed() => break,
                _ = tokio::time::sleep_until(deadline) => break,
            };
            match next {
                Some(Ok(f)) => {
                    let rms_db = almond_audio::level::rms_dbfs(&f.samples);
                    let peak_db = almond_audio::level::peak_dbfs(&f.samples);
                    notifier.emit(
                        TEST_LEVEL,
                        TestLevel {
                            rms_db,
                            peak_db,
                            level: ((rms_db + 60.0) / 60.0).clamp(0.0, 1.0),
                        },
                    );
                }
                Some(Err(e)) => {
                    tracing::warn!("microphone test: {e}");
                    break;
                }
                None => break,
            }
        }
        let _ = stream.stop().await;
        notifier.emit(TEST_STOPPED, serde_json::Value::Null);
    });
    *core.capture.mic_test.0.lock().await = Some((task, tx));
    Ok(TestStarted { device: name })
}

/// Stop a running test (no-op otherwise) and wait until the device is closed.
pub(crate) async fn stop(core: &Core) {
    let running = core.capture.mic_test.0.lock().await.take();
    if let Some((task, tx)) = running {
        let _ = tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
    }
}
