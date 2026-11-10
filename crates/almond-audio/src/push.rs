//! Push-based source: the hook for capture code that lives outside Rust (the Android Kotlin
//! `AudioRecord` service via JNI, WS-G) or any other producer that hands us PCM buffers.
//!
//! ```no_run
//! # use almond_audio::{CaptureClock, PushAudioSource};
//! # use almond_types::meeting::AudioChannel;
//! let (source, stream) = PushAudioSource::new(AudioChannel::Mic, CaptureClock::start(), 100);
//! // JNI thread, for every AudioRecord.read():
//! source.push_i16(&[0i16; 960], 48_000, 1);
//! // when the service stops:
//! source.finish();
//! // `stream` is a Box-able `AudioStream` producing aligned 16 kHz frames.
//! # drop(stream);
//! ```

use crate::clock::CaptureClock;
use crate::pipeline::{AlignConfig, PipelineStream, RawSender, channel_pair, downmix};
use almond_types::meeting::AudioChannel;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Producer handle; `Clone + Send + Sync`. All methods are non-blocking except [`finish`] and
/// [`fail`] (which may wait briefly for queue space; never call them from an async task).
///
/// [`finish`]: PushAudioSource::finish
/// [`fail`]: PushAudioSource::fail
#[derive(Clone)]
pub struct PushAudioSource {
    tx: RawSender,
    stopped: Arc<AtomicBool>,
}

impl PushAudioSource {
    /// Create a live source and its consumer stream. `frame_ms` as in `CaptureConfig`.
    pub fn new(
        channel: AudioChannel,
        clock: CaptureClock,
        frame_ms: u32,
    ) -> (PushAudioSource, PipelineStream) {
        let (tx, mut stream) = channel_pair(channel, clock, frame_ms, AlignConfig::live());
        let stopped = Arc::new(AtomicBool::new(false));
        let s2 = stopped.clone();
        stream.set_on_stop(move || s2.store(true, Ordering::SeqCst));
        (PushAudioSource { tx, stopped }, stream)
    }

    /// True once the consumer called `AudioStream::stop` (or was dropped): the producer should
    /// stop recording and call [`PushAudioSource::finish`].
    pub fn stop_requested(&self) -> bool {
        self.stopped.load(Ordering::SeqCst) || self.tx.is_closed()
    }

    /// Interleaved f32 PCM that was captured just now (the buffer's last sample = now).
    pub fn push_f32(&self, data: &[f32], sample_rate: u32, channels: u16) -> bool {
        let ch = channels.max(1) as usize;
        let frames = data.len() / ch;
        let dur = Duration::from_secs_f64(frames as f64 / sample_rate.max(1) as f64);
        let now = Instant::now();
        let at = now.checked_sub(dur).unwrap_or(now);
        self.push_f32_at(at, data, sample_rate, channels)
    }

    /// Interleaved f32 PCM whose first sample was captured at `at`.
    pub fn push_f32_at(&self, at: Instant, data: &[f32], sample_rate: u32, channels: u16) -> bool {
        self.tx
            .push(at, sample_rate, downmix(data, channels.max(1) as usize))
    }

    /// Interleaved i16 PCM (Android `ENCODING_PCM_16BIT`) captured just now.
    pub fn push_i16(&self, data: &[i16], sample_rate: u32, channels: u16) -> bool {
        let f: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
        self.push_f32(&f, sample_rate, channels)
    }

    /// Clean end of stream.
    pub fn finish(&self) {
        self.tx.end();
    }

    /// End the stream with an error (e.g. permission revoked).
    pub fn fail(&self, msg: &str) {
        self.tx.fatal(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::audio::AudioStream;

    #[test]
    fn push_then_finish() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let clock = CaptureClock::start();
            let (src, mut stream) = PushAudioSource::new(AudioChannel::Mic, clock, 100);
            let at = clock.epoch();
            let t = std::thread::spawn(move || {
                src.push_f32_at(at, &vec![0.3f32; 16_000], 16_000, 1);
                src.finish();
            });
            let mut n = 0;
            while let Some(f) = stream.next().await {
                let f = f.unwrap();
                assert!(
                    f.samples
                        .iter()
                        .all(|&x| (x - 0.3).abs() < 1e-6 || x == 0.0)
                );
                n += f.samples.len();
            }
            t.join().unwrap();
            assert!(n >= 16_000, "{n}");
        });
    }
}
