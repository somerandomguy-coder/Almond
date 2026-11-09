//! Run several channel streams at once: every frame goes to the [`SessionRecorder`] (on a
//! dedicated writer thread, so disk I/O never stalls capture) and to a bounded subscriber
//! channel for live consumers (STT, level meters, UI). A slow subscriber loses frames (counted);
//! the recording never does.

use crate::recorder::SessionRecorder;
use almond_types::audio::{AudioFrame, AudioStream};
use almond_types::meeting::AudioTrack;
use almond_types::store::BlobStore;
use almond_types::{Error, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// How long `stop` waits for a stream to drain after being stopped.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

pub struct CaptureSession {
    stop_tx: watch::Sender<bool>,
    pumps: Vec<JoinHandle<Result<()>>>,
    writer: Option<std::thread::JoinHandle<(Option<SessionRecorder>, Option<Error>)>>,
    dropped_live: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
pub struct CaptureSummary {
    /// Archived tracks (empty when no recorder was given).
    pub tracks: Vec<AudioTrack>,
    /// Per-stream / writer errors that ended a channel early.
    pub errors: Vec<String>,
    /// Frames the live subscriber did not take in time.
    pub live_frames_dropped: u64,
}

impl CaptureSession {
    /// Start pumping `streams`. Returns the session and the live frame receiver (capacity
    /// `live_capacity` frames).
    pub fn start(
        streams: Vec<Box<dyn AudioStream>>,
        recorder: Option<SessionRecorder>,
        live_capacity: usize,
    ) -> Result<(CaptureSession, mpsc::Receiver<AudioFrame>)> {
        let (stop_tx, stop_rx) = watch::channel(false);
        let (live_tx, live_rx) = mpsc::channel(live_capacity.max(1));
        let (rec_tx, rec_rx) = std::sync::mpsc::channel::<AudioFrame>();
        let writer = std::thread::Builder::new()
            .name("almond-recorder".into())
            .spawn(move || {
                let mut rec = recorder;
                let mut err = None;
                for frame in rec_rx {
                    if let (Some(r), None) = (rec.as_mut(), err.as_ref())
                        && let Err(e) = r.push(&frame)
                    {
                        tracing::error!("recorder: {e}");
                        err = Some(e);
                    }
                }
                (rec, err)
            })
            .map_err(Error::Io)?;
        let dropped_live = Arc::new(AtomicU64::new(0));
        let pumps = streams
            .into_iter()
            .map(|s| {
                tokio::spawn(pump(
                    s,
                    stop_rx.clone(),
                    rec_tx.clone(),
                    live_tx.clone(),
                    dropped_live.clone(),
                ))
            })
            .collect();
        Ok((
            CaptureSession {
                stop_tx,
                pumps,
                writer: Some(writer),
                dropped_live,
            },
            live_rx,
        ))
    }

    /// Stop all streams, drain them, then finish the recording into `blobs`.
    /// The final merge does blocking file I/O (a few hundred ms for an hour of audio).
    pub async fn stop(mut self, blobs: &dyn BlobStore) -> Result<CaptureSummary> {
        let _ = self.stop_tx.send(true);
        let mut summary = CaptureSummary::default();
        for p in self.pumps.drain(..) {
            match p.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => summary.errors.push(e.to_string()),
                Err(e) => summary.errors.push(format!("capture task: {e}")),
            }
        }
        let writer = self.writer.take().expect("writer");
        let (rec, err) = tokio::task::spawn_blocking(move || writer.join())
            .await
            .map_err(Error::other)?
            .map_err(|_| Error::Other("recorder thread panicked".into()))?;
        if let Some(e) = err {
            summary.errors.push(format!("recorder: {e}"));
        }
        if let Some(rec) = rec {
            summary.tracks = rec.finish(blobs)?;
        }
        summary.live_frames_dropped = self.dropped_live.load(Ordering::Relaxed);
        Ok(summary)
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        // Dropped without `stop`: stop devices; chunks on disk stay recoverable.
        let _ = self.stop_tx.send(true);
    }
}

async fn pump(
    mut stream: Box<dyn AudioStream>,
    mut stop_rx: watch::Receiver<bool>,
    rec_tx: std::sync::mpsc::Sender<AudioFrame>,
    live_tx: mpsc::Sender<AudioFrame>,
    dropped: Arc<AtomicU64>,
) -> Result<()> {
    let mut stopping = false;
    loop {
        let next = if stopping {
            match tokio::time::timeout(DRAIN_TIMEOUT, stream.next()).await {
                Ok(n) => n,
                Err(_) => return Err(Error::Other("stream did not end after stop".into())),
            }
        } else {
            tokio::select! {
                biased;
                _ = async { let _ = stop_rx.wait_for(|s| *s).await.is_ok(); } => {
                    stopping = true;
                    stream.stop().await?;
                    continue;
                }
                n = stream.next() => n,
            }
        };
        match next {
            Some(Ok(frame)) => {
                if live_tx.try_send(frame.clone()).is_err() {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
                if rec_tx.send(frame).is_err() {
                    return Err(Error::Other("recorder thread gone".into()));
                }
            }
            Some(Err(e)) => return Err(e),
            None => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::{Codec, RecorderConfig};
    use almond_types::audio::{AudioBackend, CaptureConfig};
    use almond_types::meeting::AudioChannel;
    use almond_types::mock::{MemBlobStore, MockAudioBackend};

    #[tokio::test]
    async fn records_two_mock_channels() {
        let dir = std::env::temp_dir().join(format!("almond-capture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let backend = MockAudioBackend { frames: 30 };
        let cfg = CaptureConfig::default();
        let mic = backend.open(AudioChannel::Mic, cfg.clone()).await.unwrap();
        let sys = backend.open(AudioChannel::System, cfg).await.unwrap();
        let rec = SessionRecorder::create(
            &dir,
            0,
            RecorderConfig {
                codec: Codec::Wav,
                ..Default::default()
            },
        )
        .unwrap();
        let (session, mut live) = CaptureSession::start(vec![mic, sys], Some(rec), 1024).unwrap();
        let mut n = 0;
        while let Some(f) = live.recv().await {
            n += 1;
            if n == 60 {
                break;
            }
            assert_eq!(f.samples.len(), 1600);
        }
        let blobs = MemBlobStore::default();
        let summary = session.stop(&blobs).await.unwrap();
        assert!(summary.errors.is_empty(), "{:?}", summary.errors);
        assert_eq!(summary.tracks.len(), 2);
        for t in &summary.tracks {
            assert_eq!(t.duration_ms, 3000);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
