//! The device-independent half of every capture backend.
//!
//! A producer (device callback thread, JNI bridge, file reader) pushes raw mono chunks at the
//! device's native rate through a [`RawSender`]. The consumer, [`PipelineStream`], implements
//! [`AudioStream`]:
//!
//! 1. **Alignment** to the shared [`CaptureClock`]: every chunk carries the capture instant of its
//!    first sample. If the stream position lags that instant by more than `tolerance` (a gap, e.g.
//!    WASAPI loopback delivers nothing while the system is silent, or a device was re-opened),
//!    zeros are inserted; if it runs ahead (device clock faster than the wall clock), samples are
//!    dropped. Drift between channels is thus bounded by ~2 x tolerance for any length of recording.
//! 2. **Wall-clock silence fill**: while live and no data arrives, zero frames are emitted once the
//!    wall clock is `max_latency` past the stream position, so silent loopback still produces
//!    frames and channels stay aligned.
//! 3. **Resampling** to 16 kHz mono ([`StreamResampler`]) and framing into `frame_ms` frames whose
//!    `t0_ms` is contiguous from the clock epoch.

use crate::clock::CaptureClock;
use crate::resample::StreamResampler;
use almond_types::audio::{AudioFrame, AudioStream, PIPELINE_SAMPLE_RATE};
use almond_types::meeting::AudioChannel;
use almond_types::{Error, Result};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Messages from a producer to its [`PipelineStream`].
#[derive(Debug)]
pub(crate) enum RawMsg {
    /// Mono samples at `rate`. `at` = seconds since the clock epoch of the first sample, or `None`
    /// for contiguous, untimed data (file import).
    Data {
        at: Option<f64>,
        rate: u32,
        samples: Vec<f32>,
    },
    /// Non-fatal condition (device re-opened, xrun...). Logged and counted.
    Warn(String),
    /// Fatal: the stream ends with this error.
    Fatal(String),
    /// Producer finished cleanly.
    End,
}

/// Tunables of the alignment stage.
#[derive(Debug, Clone, Copy)]
pub struct AlignConfig {
    /// Emit wall-clock zero frames when no data arrives (live devices). `false` for file import.
    pub live: bool,
    /// Max |position - capture time| tolerated before inserting zeros / dropping samples.
    pub tolerance: Duration,
    /// How far behind the wall clock a live stream may lag before silence is synthesized.
    pub max_latency: Duration,
}

impl AlignConfig {
    pub fn live() -> Self {
        Self {
            live: true,
            tolerance: Duration::from_millis(30),
            max_latency: Duration::from_millis(300),
        }
    }
    pub fn offline() -> Self {
        Self {
            live: false,
            ..Self::live()
        }
    }
}

/// Counters describing what the aligner had to do. Shared, cheap to clone.
#[derive(Debug, Default)]
pub struct StreamStats {
    /// Samples (16 kHz-equivalent) of zeros inserted for gaps / silence.
    pub zero_filled: AtomicU64,
    /// Samples (16 kHz-equivalent) dropped because the device ran ahead of the wall clock.
    pub dropped: AtomicU64,
    /// Number of gap/drift corrections (excluding wall-clock silence fill).
    pub corrections: AtomicU64,
    /// Raw chunks received from the device.
    pub chunks: AtomicU64,
    /// Chunks lost because the consumer did not keep up (queue full).
    pub overflow: AtomicU64,
    /// Warnings (device re-open, xruns).
    pub warnings: AtomicU64,
}

impl StreamStats {
    pub fn snapshot(&self) -> StatsSnapshot {
        let ms = |v: &AtomicU64| v.load(Ordering::Relaxed) * 1000 / PIPELINE_SAMPLE_RATE as u64;
        StatsSnapshot {
            zero_filled_ms: ms(&self.zero_filled),
            dropped_ms: ms(&self.dropped),
            corrections: self.corrections.load(Ordering::Relaxed),
            chunks: self.chunks.load(Ordering::Relaxed),
            overflow: self.overflow.load(Ordering::Relaxed),
            warnings: self.warnings.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatsSnapshot {
    pub zero_filled_ms: u64,
    pub dropped_ms: u64,
    pub corrections: u64,
    pub chunks: u64,
    pub overflow: u64,
    pub warnings: u64,
}

const QUEUE_CAPACITY: usize = 4096;

/// Producer handle. Cheap to clone; never blocks in [`RawSender::push`].
#[derive(Clone)]
pub struct RawSender {
    tx: mpsc::Sender<RawMsg>,
    clock: CaptureClock,
    stats: Arc<StreamStats>,
}

impl RawSender {
    pub fn clock(&self) -> CaptureClock {
        self.clock
    }

    /// Push mono samples whose first sample was captured at `at`. Non-blocking (safe in audio
    /// callbacks); returns `false` if the consumer is gone.
    pub fn push(&self, at: Instant, rate: u32, samples: Vec<f32>) -> bool {
        self.send(RawMsg::Data {
            at: Some(self.clock.secs_at(at)),
            rate,
            samples,
        })
    }

    /// Push interleaved multi-channel samples (downmixed to mono by averaging).
    pub fn push_interleaved(&self, at: Instant, rate: u32, channels: usize, data: &[f32]) -> bool {
        self.push(at, rate, downmix(data, channels))
    }

    /// Push contiguous untimed samples, waiting for queue space (file import; not for callbacks
    /// or async contexts).
    pub fn push_blocking(&self, rate: u32, samples: Vec<f32>) -> bool {
        self.tx
            .blocking_send(RawMsg::Data {
                at: None,
                rate,
                samples,
            })
            .is_ok()
    }

    pub fn warn(&self, msg: impl Into<String>) {
        self.stats.warnings.fetch_add(1, Ordering::Relaxed);
        self.send(RawMsg::Warn(msg.into()));
    }

    pub fn fatal(&self, msg: impl Into<String>) {
        // Must not be lost to a full queue.
        let _ = self.tx.blocking_send(RawMsg::Fatal(msg.into()));
    }

    pub fn end(&self) {
        let _ = self.tx.try_send(RawMsg::End).or_else(|e| match e {
            mpsc::error::TrySendError::Full(m) => self.tx.blocking_send(m).map_err(|_| ()),
            mpsc::error::TrySendError::Closed(_) => Err(()),
        });
    }

    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    fn send(&self, msg: RawMsg) -> bool {
        match self.tx.try_send(msg) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.stats.overflow.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

/// Average interleaved channels into mono.
pub fn downmix(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    let inv = 1.0 / channels as f32;
    data.chunks_exact(channels)
        .map(|f| f.iter().sum::<f32>() * inv)
        .collect()
}

type StopFn = Box<dyn FnOnce() + Send>;

/// Consumer side; implements [`AudioStream`]. See module docs.
pub struct PipelineStream {
    channel: AudioChannel,
    clock: CaptureClock,
    cfg: AlignConfig,
    frame_ms: u32,
    frame_len: usize,
    rx: mpsc::Receiver<RawMsg>,
    on_stop: Option<StopFn>,
    stats: Arc<StreamStats>,
    resampler: Option<StreamResampler>,
    /// Seconds of audio (incl. inserted zeros) fed into the resampler so far.
    pos_secs: f64,
    out: Vec<f32>,
    frames_emitted: u64,
    ended: bool,
    pending_err: Option<Error>,
}

/// Create a connected producer/consumer pair.
pub fn channel_pair(
    channel: AudioChannel,
    clock: CaptureClock,
    frame_ms: u32,
    cfg: AlignConfig,
) -> (RawSender, PipelineStream) {
    let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
    let stats = Arc::new(StreamStats::default());
    let frame_ms = frame_ms.clamp(10, 10_000);
    let frame_len = (PIPELINE_SAMPLE_RATE as usize * frame_ms as usize) / 1000;
    let sender = RawSender {
        tx,
        clock,
        stats: stats.clone(),
    };
    let stream = PipelineStream {
        channel,
        clock,
        cfg,
        frame_ms,
        frame_len,
        rx,
        on_stop: None,
        stats,
        resampler: None,
        pos_secs: 0.0,
        out: Vec::with_capacity(frame_len * 2),
        frames_emitted: 0,
        ended: false,
        pending_err: None,
    };
    (sender, stream)
}

impl PipelineStream {
    /// Called once by [`AudioStream::stop`] to tell the producer to stop.
    pub fn set_on_stop(&mut self, f: impl FnOnce() + Send + 'static) {
        self.on_stop = Some(Box::new(f));
    }

    pub fn stats(&self) -> Arc<StreamStats> {
        self.stats.clone()
    }

    pub fn channel(&self) -> AudioChannel {
        self.channel
    }

    pub fn clock(&self) -> CaptureClock {
        self.clock
    }

    fn handle(&mut self, msg: RawMsg) {
        match msg {
            RawMsg::Data { at, rate, samples } => {
                self.stats.chunks.fetch_add(1, Ordering::Relaxed);
                if let Err(e) = self.feed(at, rate, &samples) {
                    self.pending_err = Some(e);
                    self.ended = true;
                }
            }
            RawMsg::Warn(w) => tracing::warn!(channel = ?self.channel, "{w}"),
            RawMsg::Fatal(e) => {
                self.pending_err = Some(Error::Other(e));
                self.ended = true;
            }
            RawMsg::End => self.ended = true,
        }
    }

    fn ensure_rate(&mut self, rate: u32) -> Result<()> {
        if self.resampler.as_ref().map(|r| r.rate_in()) != Some(rate) {
            self.resampler = Some(StreamResampler::new(rate)?);
        }
        Ok(())
    }

    fn feed(&mut self, at: Option<f64>, rate: u32, samples: &[f32]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        self.ensure_rate(rate)?;
        let mut samples = samples;
        if let Some(at) = at {
            let gap = at - self.pos_secs;
            let tol = self.cfg.tolerance.as_secs_f64();
            if gap > tol {
                self.stats.corrections.fetch_add(1, Ordering::Relaxed);
                self.insert_zeros(gap, rate)?;
            } else if gap < -tol {
                let drop = ((-gap) * rate as f64).round() as usize;
                let drop = drop.min(samples.len());
                if drop > 0 {
                    self.stats.corrections.fetch_add(1, Ordering::Relaxed);
                    self.stats.dropped.fetch_add(
                        drop as u64 * PIPELINE_SAMPLE_RATE as u64 / rate as u64,
                        Ordering::Relaxed,
                    );
                    samples = &samples[drop..];
                }
            }
        }
        if samples.is_empty() {
            return Ok(());
        }
        self.pos_secs += samples.len() as f64 / rate as f64;
        let rs = self.resampler.as_mut().expect("rate set");
        rs.process(samples, &mut self.out)
    }

    fn insert_zeros(&mut self, secs: f64, rate: u32) -> Result<()> {
        let n = (secs * rate as f64).round() as usize;
        if n == 0 {
            return Ok(());
        }
        self.stats.zero_filled.fetch_add(
            n as u64 * PIPELINE_SAMPLE_RATE as u64 / rate as u64,
            Ordering::Relaxed,
        );
        self.pos_secs += n as f64 / rate as f64;
        let zeros = vec![0.0f32; n];
        match self.resampler.as_mut() {
            Some(rs) => rs.process(&zeros, &mut self.out),
            None => {
                // No device format known yet: zeros are rate-independent.
                self.out.extend_from_slice(&zeros);
                Ok(())
            }
        }
    }

    /// Wall-clock silence fill for live streams.
    fn fill_to_wall_clock(&mut self) -> Result<()> {
        if !self.cfg.live || self.ended {
            return Ok(());
        }
        let target = self.clock.now_secs() - self.cfg.max_latency.as_secs_f64();
        let gap = target - self.pos_secs;
        if gap > 0.0 {
            let rate = self
                .resampler
                .as_ref()
                .map(|r| r.rate_in())
                .unwrap_or(PIPELINE_SAMPLE_RATE);
            self.insert_zeros(gap, rate)?;
        }
        Ok(())
    }

    fn take_frame(&mut self, len: usize) -> AudioFrame {
        let len = len.min(self.out.len());
        let samples: Vec<f32> = self.out.drain(..len).collect();
        let t0_ms = self.frames_emitted * self.frame_ms as u64;
        self.frames_emitted += 1;
        AudioFrame {
            channel: self.channel,
            t0_ms,
            samples,
        }
    }

    fn drain_queue(&mut self) {
        while !self.ended {
            match self.rx.try_recv() {
                Ok(m) => self.handle(m),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => self.ended = true,
            }
        }
    }
}

#[async_trait]
impl AudioStream for PipelineStream {
    async fn next(&mut self) -> Option<Result<AudioFrame>> {
        loop {
            if self.out.len() >= self.frame_len {
                return Some(Ok(self.take_frame(self.frame_len)));
            }
            if self.ended {
                if let Some(e) = self.pending_err.take() {
                    return Some(Err(e));
                }
                if !self.out.is_empty() {
                    let n = self.out.len();
                    return Some(Ok(self.take_frame(n)));
                }
                return None;
            }
            let msg = if self.cfg.live {
                let wait = Duration::from_millis((self.frame_ms as u64 / 2).max(10));
                tokio::time::timeout(wait, self.rx.recv()).await.ok()
            } else {
                Some(self.rx.recv().await)
            };
            match msg {
                Some(Some(m)) => self.handle(m),
                Some(None) => self.ended = true,
                None => {}
            }
            self.drain_queue();
            if let Err(e) = self.fill_to_wall_clock() {
                self.pending_err = Some(e);
                self.ended = true;
            }
        }
    }

    async fn stop(&mut self) -> Result<()> {
        if let Some(f) = self.on_stop.take() {
            f();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    async fn collect(mut s: PipelineStream) -> Vec<AudioFrame> {
        let mut v = vec![];
        while let Some(f) = s.next().await {
            v.push(f.unwrap());
        }
        v
    }

    #[test]
    fn offline_contiguous_frames() {
        rt().block_on(async {
            let clock = CaptureClock::start();
            let (tx, s) = channel_pair(AudioChannel::Mixed, clock, 100, AlignConfig::offline());
            std::thread::spawn(move || {
                for _ in 0..10 {
                    tx.push_blocking(48_000, vec![0.25; 4_800]);
                }
                tx.end();
            });
            let frames = collect(s).await;
            let total: usize = frames.iter().map(|f| f.samples.len()).sum();
            assert!(total > 15_500 && total <= 16_000, "{total}");
            for (i, f) in frames.iter().enumerate() {
                assert_eq!(f.t0_ms, i as u64 * 100);
            }
        });
    }

    #[test]
    fn gap_is_zero_filled_and_aligned() {
        rt().block_on(async {
            let clock = CaptureClock::start();
            let mut cfg = AlignConfig::offline();
            cfg.tolerance = Duration::from_millis(20);
            let (tx, mut s) = channel_pair(AudioChannel::System, clock, 100, cfg);
            let e = clock.epoch();
            // 0.0-0.5 s audio, then a 1 s gap, then 0.5 s audio at t=1.5.
            tx.push(e, 16_000, vec![0.5; 8_000]);
            tx.push(e + Duration::from_millis(1500), 16_000, vec![0.5; 8_000]);
            tx.end();
            let mut all = vec![];
            while let Some(f) = s.next().await {
                all.extend(f.unwrap().samples);
            }
            assert_eq!(all.len(), 32_000);
            assert!(all[..8_000].iter().all(|&x| x == 0.5));
            assert!(all[8_000..24_000].iter().all(|&x| x == 0.0));
            assert!(all[24_000..].iter().all(|&x| x == 0.5));
            let st = s.stats().snapshot();
            assert_eq!(st.zero_filled_ms, 1000);
        });
    }

    #[test]
    fn fast_device_clock_is_trimmed() {
        rt().block_on(async {
            let clock = CaptureClock::start();
            let (tx, s) = channel_pair(AudioChannel::Mic, clock, 100, AlignConfig::offline());
            let e = clock.epoch();
            // Device claims 1.1 s of audio per 1.0 s of wall time.
            for i in 0..100u64 {
                tx.push(e + Duration::from_millis(i * 100), 16_000, vec![0.1; 1_760]);
            }
            tx.end();
            let frames = collect(s).await;
            let total: usize = frames.iter().map(|f| f.samples.len()).sum();
            let drift_ms = (total as f64 / 16.0) - 10_000.0;
            assert!(drift_ms.abs() < 200.0, "drift {drift_ms} ms");
        });
    }

    #[test]
    fn live_silence_produces_frames() {
        rt().block_on(async {
            let clock = CaptureClock::start();
            let mut cfg = AlignConfig::live();
            cfg.max_latency = Duration::from_millis(50);
            let (_tx, mut s) = channel_pair(AudioChannel::System, clock, 100, cfg);
            let f = tokio::time::timeout(Duration::from_secs(2), s.next())
                .await
                .expect("frame within 2s")
                .unwrap()
                .unwrap();
            assert_eq!(f.t0_ms, 0);
            assert_eq!(f.samples.len(), 1_600);
            assert!(f.samples.iter().all(|&x| x == 0.0));
        });
    }

    #[test]
    fn downmix_stereo() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
    }
}
