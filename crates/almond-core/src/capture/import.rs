//! `meetings.import`: turn an audio or video file into an imported meeting.
//!
//! The file is decoded as a stream (packet by packet, never loaded whole), down-mixed to mono,
//! resampled to the 16 kHz pipeline rate and written through the same
//! [`SessionRecorder`](almond_audio::SessionRecorder) a recording uses, so the result is one
//! `mixed` Ogg Opus track exactly like recorded audio (≈ 11 MB/h). The track goes into the blob
//! store (`blob.available`, `audio/ogg`) and a `MeetingKind::Imported` meeting is upserted with
//! it; the processing module's job loop then plans `transcribe.local` → summary, notes, … as for
//! any meeting with audio.
//!
//! Decoders:
//! - pure Rust ([symphonia]): WAV/AIFF/CAF (PCM, ADPCM), MP3, AAC and ALAC in MP4/M4A/MOV,
//!   FLAC, Vorbis, and the Ogg / Matroska / WebM containers;
//! - Opus packets (Ogg, WebM, MKV) are decoded with libopus (the `opus` crate almond-audio
//!   already builds), since symphonia has no Opus decoder;
//! - anything else (AC-3, WMA, AVI, …): `ffmpeg` from `PATH` when it is installed, otherwise a
//!   clear `Unsupported` error naming the file type.
//!
//! Progress: `import.progress` notifications (≈ 4/s) with the new meeting's id, which
//! `meetings.importCancel { meeting_id }` takes to stop the import (nothing is stored then).
//!
//! Times: the file's modification time is taken as the end of the meeting (what a recorder app
//! leaves behind), `started_at = ended_at - duration`; without one, the import time is the end.
//! Work files live in `imports/<meeting>/` under the data dir and are removed afterwards.

use crate::Core;
use almond_audio::{RecorderConfig, SessionRecorder, StreamResampler};
use almond_types::audio::{AudioFrame, PIPELINE_SAMPLE_RATE};
use almond_types::event::EventBody;
use almond_types::lang::LangTag;
use almond_types::meeting::{AudioChannel, AudioTrack, Meeting, MeetingKind};
use almond_types::store::{BlobStore, Store};
use almond_types::time::now_ms;
use almond_types::{Error, Id, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, CODEC_TYPE_OPUS, CodecParameters, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Method names and the notification (docs/API.md).
pub const MEETINGS_IMPORT: &str = "meetings.import";
pub const MEETINGS_IMPORT_CANCEL: &str = "meetings.importCancel";
pub const IMPORT_PROGRESS: &str = "import.progress";

const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// Frames handed to the recorder: 100 ms at 16 kHz (whole ms, so `t0_ms` is exact).
const FRAME: usize = PIPELINE_SAMPLE_RATE as usize / 10;
const SAMPLES_PER_MS: u64 = PIPELINE_SAMPLE_RATE as u64 / 1000;

/// `meetings.import` params.
#[derive(Debug, Clone, Deserialize)]
pub struct ImportParams {
    /// Absolute path of the audio/video file on this device.
    pub path: String,
    /// Meeting title (default: the file name without extension).
    #[serde(default)]
    pub title: Option<String>,
    /// Spoken language, `"vi"` or `"en"` (default: detected by transcription).
    #[serde(default)]
    pub language: Option<String>,
}

/// `meetings.importCancel` params.
#[derive(Debug, Clone, Deserialize)]
pub struct ImportCancelParams {
    pub meeting_id: Id,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportStage {
    /// Decoding + encoding (the long part).
    Decoding,
    /// Storing the track and creating the meeting.
    Storing,
    Done,
    Failed,
    Cancelled,
}

/// `import.progress` payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportProgress {
    /// Id the imported meeting gets (pass it to `meetings.importCancel`).
    pub meeting_id: Id,
    pub path: String,
    pub file_name: String,
    pub stage: ImportStage,
    /// 0..=1 when the length is known (from the container, else from bytes read).
    pub progress: Option<f32>,
    /// Audio decoded so far.
    pub processed_ms: u64,
    /// Length of the file's audio, when the container says.
    pub duration_ms: Option<u64>,
    /// `"symphonia"`, `"opus"` or `"ffmpeg"`, once known.
    pub decoder: Option<String>,
    /// Set when `stage` is `failed`.
    pub error: Option<String>,
}

/// Imports in progress (meeting id → cancel flag). Process-wide: meeting ids are unique.
static ACTIVE: Mutex<BTreeMap<String, Arc<AtomicBool>>> = Mutex::new(BTreeMap::new());

fn active() -> std::sync::MutexGuard<'static, BTreeMap<String, Arc<AtomicBool>>> {
    ACTIVE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Removes the import from [`ACTIVE`] and its work directory when dropped.
struct ActiveGuard {
    id: String,
    dir: PathBuf,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        active().remove(&self.id);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `meetings.importCancel`: `true` if an import with this id was running.
pub(crate) fn cancel(meeting_id: &Id) -> bool {
    match active().get(meeting_id.as_str()) {
        Some(c) => {
            c.store(true, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn default_title(path: &Path) -> String {
    path.file_stem()
        .map(|n| n.to_string_lossy().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| file_name_of(path))
}

/// `meetings.import`.
pub(crate) async fn import(core: &Arc<Core>, p: ImportParams) -> Result<Meeting> {
    let language = match p.language.as_deref().map(str::trim) {
        None | Some("") => None,
        Some("vi") => Some(LangTag::vi()),
        Some("en") => Some(LangTag::en()),
        Some(other) => {
            return Err(Error::Invalid(format!(
                "language must be \"vi\" or \"en\", not {other:?}"
            )));
        }
    };
    let path = PathBuf::from(p.path.trim());
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| Error::NotFound(format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(Error::Invalid(format!("{} is not a file", path.display())));
    }
    let id = Id::new();
    let root = core.cfg.data_dir.join("imports");
    let dir = root.join(id.as_str());
    let cancel_flag = Arc::new(AtomicBool::new(false));
    active().insert(id.as_str().to_string(), cancel_flag.clone());
    let _guard = ActiveGuard {
        id: id.as_str().to_string(),
        dir: dir.clone(),
    };
    remove_stale_work_dirs(&root);

    let base = ImportProgress {
        meeting_id: id.clone(),
        path: path.to_string_lossy().into_owned(),
        file_name: file_name_of(&path),
        stage: ImportStage::Decoding,
        progress: Some(0.0),
        processed_ms: 0,
        duration_ms: None,
        decoder: None,
        error: None,
    };
    let notifier = core.notifier.clone();
    notifier.emit(IMPORT_PROGRESS, &base);
    tracing::info!(meeting = %id, path = %path.display(), "import started");
    let t0 = Instant::now();

    let core2 = core.clone();
    let (path2, dir2, cancel2, base2) =
        (path.clone(), dir.clone(), cancel_flag.clone(), base.clone());
    let decoded = tokio::task::spawn_blocking(move || {
        let mut last = Instant::now()
            .checked_sub(PROGRESS_EVERY)
            .unwrap_or_else(Instant::now);
        let mut on_progress = |pr: Progress| {
            if last.elapsed() < PROGRESS_EVERY {
                return;
            }
            last = Instant::now();
            core2.notifier.emit(
                IMPORT_PROGRESS,
                ImportProgress {
                    progress: pr.fraction(),
                    processed_ms: pr.processed_ms,
                    duration_ms: pr.duration_ms,
                    decoder: Some(pr.decoder.to_string()),
                    ..base2.clone()
                },
            );
        };
        decode_to_track(
            &path2,
            &dir2,
            core2.blobs().as_ref(),
            &cancel2,
            &mut on_progress,
        )
    })
    .await
    .map_err(Error::other)
    .and_then(|r| r);

    let (track, decoder) = match decoded {
        Ok(x) => x,
        Err(e) => {
            let cancelled = matches!(e, Error::Cancelled);
            if cancelled {
                tracing::info!(meeting = %id, "import cancelled");
            } else {
                tracing::warn!(meeting = %id, path = %path.display(), "import failed: {e}");
            }
            notifier.emit(
                IMPORT_PROGRESS,
                ImportProgress {
                    stage: if cancelled {
                        ImportStage::Cancelled
                    } else {
                        ImportStage::Failed
                    },
                    progress: None,
                    error: (!cancelled).then(|| e.to_string()),
                    ..base.clone()
                },
            );
            return Err(e);
        }
    };
    let dur = track.duration_ms as u64;
    notifier.emit(
        IMPORT_PROGRESS,
        ImportProgress {
            stage: ImportStage::Storing,
            progress: Some(1.0),
            processed_ms: dur,
            duration_ms: Some(dur),
            decoder: Some(decoder.to_string()),
            ..base.clone()
        },
    );

    let ended_at = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .filter(|&t| t > 0 && t <= now_ms())
        .unwrap_or_else(now_ms);
    let meeting = Meeting {
        id: id.clone(),
        title: p
            .title
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| default_title(&path)),
        started_at: ended_at - dur as i64,
        ended_at: Some(ended_at),
        kind: MeetingKind::Imported,
        calendar_event: None,
        languages: language.into_iter().collect(),
        audio: vec![track],
        notes_doc: Id::new(),
        tags: vec![],
        tz: iana_time_zone::get_timezone().ok(),
    };
    let core3 = core.clone();
    let m = meeting.clone();
    tokio::task::spawn_blocking(move || store_meeting(&core3, m))
        .await
        .map_err(Error::other)??;

    notifier.emit(
        IMPORT_PROGRESS,
        ImportProgress {
            stage: ImportStage::Done,
            progress: Some(1.0),
            processed_ms: dur,
            duration_ms: Some(dur),
            decoder: Some(decoder.to_string()),
            ..base
        },
    );
    tracing::info!(
        meeting = %id,
        audio_s = dur / 1000,
        took_s = t0.elapsed().as_secs_f32(),
        decoder,
        "import finished"
    );
    Ok(meeting)
}

/// Announce the blob and upsert the meeting (as `recording::attach_tracks` does for recordings).
fn store_meeting(core: &Core, m: Meeting) -> Result<()> {
    let store = core.store();
    let blobs = core.blobs();
    for t in &m.audio {
        let size = match blobs.path(&t.blob)? {
            Some(p) => std::fs::metadata(p)?.len(),
            None => blobs.get(&t.blob)?.map(|b| b.len() as u64).unwrap_or(0),
        };
        let mime = match t.codec.as_str() {
            "wav" => "audio/wav",
            "flac" => "audio/flac",
            _ => "audio/ogg",
        };
        store.append_local(EventBody::BlobAvailable {
            hash: t.blob.clone(),
            size,
            mime: mime.into(),
        })?;
    }
    store.append_local(EventBody::MeetingUpsert(m))?;
    Ok(())
}

/// Work directories of imports that were interrupted (app closed / crashed mid-import).
fn remove_stale_work_dirs(root: &Path) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    let running = active();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !running.contains_key(&name) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Decoding

/// Progress of the blocking decode loop.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Progress {
    pub processed_ms: u64,
    pub duration_ms: Option<u64>,
    /// Fraction of the file's bytes read (fallback when the duration is unknown).
    pub bytes_fraction: Option<f32>,
    pub decoder: &'static str,
}

impl Progress {
    fn fraction(&self) -> Option<f32> {
        match self.duration_ms {
            Some(d) if d > 0 => Some((self.processed_ms as f32 / d as f32).clamp(0.0, 1.0)),
            _ => self.bytes_fraction.map(|f| f.clamp(0.0, 1.0)),
        }
    }
}

/// A decoder producing mono audio at its own sample rate.
trait Source {
    fn name(&self) -> &'static str;
    /// Next block of mono samples and their rate; `None` at the end.
    fn next_block(&mut self) -> Result<Option<(u32, &[f32])>>;
    fn duration_ms(&self) -> Option<u64>;
    fn bytes_fraction(&self) -> Option<f32> {
        None
    }
}

/// Decode `path` into one `mixed` Opus track in the blob store (blocking).
pub(crate) fn decode_to_track(
    path: &Path,
    dir: &Path,
    blobs: &dyn BlobStore,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(AudioTrack, &'static str)> {
    let mut src = open_source(path)?;
    let decoder = src.name();
    let _ = std::fs::remove_dir_all(dir);
    let rec = SessionRecorder::create(
        dir,
        0,
        RecorderConfig {
            // nothing to recover from (the source file is still there): no periodic fsync
            fsync_secs: 3600,
            ..RecorderConfig::default()
        },
    )?;
    let mut sink = Sink::new(rec);
    while let Some((rate, block)) = src.next_block()? {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        sink.push(rate, block)?;
        on_progress(Progress {
            processed_ms: sink.written / SAMPLES_PER_MS,
            duration_ms: src.duration_ms(),
            bytes_fraction: src.bytes_fraction(),
            decoder,
        });
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let mut tracks = sink.finish(blobs)?;
    let track = tracks
        .pop()
        .filter(|t| t.duration_ms > 0)
        .ok_or_else(|| Error::Invalid(format!("{} contains no audio", file_name_of(path))))?;
    Ok((track, decoder))
}

/// Resamples to 16 kHz and feeds the recorder in whole 100 ms frames.
struct Sink {
    rec: SessionRecorder,
    rs: Option<StreamResampler>,
    /// Input samples at the current rate, and 16 kHz output samples produced before it.
    seg_in: u64,
    seg_base: u64,
    produced: u64,
    buf: Vec<f32>,
    /// 16 kHz samples handed to the recorder.
    written: u64,
}

impl Sink {
    fn new(rec: SessionRecorder) -> Self {
        Sink {
            rec,
            rs: None,
            seg_in: 0,
            seg_base: 0,
            produced: 0,
            buf: Vec::with_capacity(FRAME * 4),
            written: 0,
        }
    }

    fn push(&mut self, rate: u32, x: &[f32]) -> Result<()> {
        if self.rs.as_ref().map(|r| r.rate_in()) != Some(rate) {
            // first block, or the stream changed rate (chained Ogg, odd MP3s)
            self.flush_resampler()?;
            self.rs = Some(StreamResampler::new(rate)?);
            self.seg_in = 0;
            self.seg_base = self.produced;
        }
        let rs = self.rs.as_mut().expect("resampler");
        let before = self.buf.len();
        rs.process(x, &mut self.buf)?;
        self.produced += (self.buf.len() - before) as u64;
        self.seg_in += x.len() as u64;
        self.drain(false)
    }

    /// Push the samples still inside the resampler (fed zeros, cut at the exact length).
    fn flush_resampler(&mut self) -> Result<()> {
        let Some(mut rs) = self.rs.take() else {
            return Ok(());
        };
        let rate = rs.rate_in() as u64;
        let want = self.seg_base + self.seg_in * PIPELINE_SAMPLE_RATE as u64 / rate;
        let zeros = vec![0.0f32; (rate / 20).max(1) as usize];
        let mut guard = 0;
        while self.produced < want && guard < 100 {
            let before = self.buf.len();
            rs.process(&zeros, &mut self.buf)?;
            self.produced += (self.buf.len() - before) as u64;
            guard += 1;
        }
        if self.produced > want {
            let extra = (self.produced - want) as usize;
            let keep = self.buf.len().saturating_sub(extra);
            self.buf.truncate(keep);
            self.produced = want;
        }
        Ok(())
    }

    fn drain(&mut self, all: bool) -> Result<()> {
        let mut off = 0;
        while self.buf.len() - off >= FRAME || (all && off < self.buf.len()) {
            let n = FRAME.min(self.buf.len() - off);
            let frame = AudioFrame {
                channel: AudioChannel::Mixed,
                t0_ms: self.written / SAMPLES_PER_MS,
                samples: self.buf[off..off + n].to_vec(),
                synthesized: false,
            };
            self.rec.push(&frame)?;
            self.written += n as u64;
            off += n;
        }
        self.buf.drain(..off);
        Ok(())
    }

    fn finish(mut self, blobs: &dyn BlobStore) -> Result<Vec<AudioTrack>> {
        self.flush_resampler()?;
        self.drain(true)?;
        if self.written == 0 {
            return Ok(vec![]);
        }
        self.rec.finish(blobs)
    }
}

/// Why symphonia could not take the file (for the ffmpeg fallback / the error message).
#[derive(Debug)]
struct NotNative(String);

fn open_source(path: &Path) -> Result<Box<dyn Source>> {
    let reason = match SymphoniaSource::open(path) {
        Ok(Ok(s)) => return Ok(Box::new(s)),
        Ok(Err(NotNative(why))) => why,
        Err(e) => return Err(e),
    };
    tracing::info!(path = %path.display(), "not decodable in-process ({reason}); trying ffmpeg");
    match FfmpegSource::spawn(path) {
        Ok(s) => Ok(Box::new(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_uppercase())
                .unwrap_or_else(|| "this".into());
            Err(Error::Unsupported(format!(
                "cannot import {} ({ext} file: {reason}). Almond reads WAV, MP3, M4A/AAC, FLAC, \
                 OGG (Opus/Vorbis), MP4/MOV (AAC) and MKV/WebM (Opus/Vorbis/AAC) itself; for \
                 other formats install ffmpeg (on PATH) or convert the file first",
                file_name_of(path)
            )))
        }
        Err(e) => Err(Error::Other(format!("starting ffmpeg: {e}"))),
    }
}

/// `File` that counts the bytes read (progress when the container has no duration).
struct CountingFile {
    f: std::fs::File,
    pos: Arc<AtomicU64>,
    len: u64,
}

impl Read for CountingFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.f.read(buf)?;
        self.pos.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl Seek for CountingFile {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let p = self.f.seek(to)?;
        self.pos.store(p, Ordering::Relaxed);
        Ok(p)
    }
}

impl MediaSource for CountingFile {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

enum Dec {
    Sym(Box<dyn symphonia::core::codecs::Decoder>),
    Opus {
        dec: opus::Decoder,
        channels: usize,
        /// 48 kHz samples (per channel) still to drop at the start.
        skip: usize,
        pcm: Vec<f32>,
    },
}

struct SymphoniaSource {
    format: Box<dyn FormatReader>,
    track_id: u32,
    dec: Dec,
    duration_ms: Option<u64>,
    pos: Arc<AtomicU64>,
    len: u64,
    sample_buf: Option<SampleBuffer<f32>>,
    mono: Vec<f32>,
    decode_errors: u32,
}

fn duration_of(cp: &CodecParameters) -> Option<u64> {
    let n = cp.n_frames?;
    if let Some(tb) = cp.time_base {
        let t = tb.calc_time(n);
        return Some(t.seconds * 1000 + (t.frac * 1000.0) as u64);
    }
    cp.sample_rate
        .filter(|&r| r > 0)
        .map(|r| n * 1000 / r as u64)
}

/// `(channels, pre_skip)` from an `OpusHead` (RFC 7845 §5.1); `None` for multistream layouts.
fn opus_head(cp: &CodecParameters) -> std::result::Result<(usize, usize), NotNative> {
    match cp.extra_data.as_deref() {
        Some(h) if h.len() >= 19 && h.starts_with(b"OpusHead") => {
            let channels = h[9] as usize;
            let pre_skip = u16::from_le_bytes([h[10], h[11]]) as usize;
            let family = h[18];
            if family != 0 || !(1..=2).contains(&channels) {
                return Err(NotNative(format!(
                    "Opus with {channels} channels (mapping family {family})"
                )));
            }
            Ok((channels, pre_skip))
        }
        _ => {
            let channels = cp.channels.map(|c| c.count()).unwrap_or(2);
            if !(1..=2).contains(&channels) {
                return Err(NotNative(format!("Opus with {channels} channels")));
            }
            Ok((channels, cp.delay.unwrap_or(0) as usize))
        }
    }
}

impl SymphoniaSource {
    /// `Ok(Err(_))`: symphonia can't handle the format/codec (try ffmpeg); `Err`: I/O error.
    fn open(path: &Path) -> Result<std::result::Result<Self, NotNative>> {
        let f = std::fs::File::open(path)?;
        let len = f.metadata()?.len();
        let pos = Arc::new(AtomicU64::new(0));
        let mss = MediaSourceStream::new(
            Box::new(CountingFile {
                f,
                pos: pos.clone(),
                len,
            }),
            Default::default(),
        );
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(&ext.to_ascii_lowercase());
        }
        let probed = match symphonia::default::get_probe().format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        ) {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() != std::io::ErrorKind::UnexpectedEof => {
                return Err(e.into());
            }
            Err(e) => return Ok(Err(NotNative(format!("unknown container ({e})")))),
        };
        let format = probed.format;
        // First audio track (video tracks have no sample rate / an unknown codec here).
        let Some(track) = format
            .tracks()
            .iter()
            .find(|t| {
                t.codec_params.codec != CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some()
            })
            .or_else(|| {
                format
                    .tracks()
                    .iter()
                    .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            })
        else {
            return Ok(Err(NotNative(
                "no audio track in a codec Almond can decode".into(),
            )));
        };
        let cp = track.codec_params.clone();
        let track_id = track.id;
        let dec = if cp.codec == CODEC_TYPE_OPUS {
            let (channels, skip) = match opus_head(&cp) {
                Ok(x) => x,
                Err(e) => return Ok(Err(e)),
            };
            let ch = if channels == 1 {
                opus::Channels::Mono
            } else {
                opus::Channels::Stereo
            };
            let dec = opus::Decoder::new(48_000, ch)
                .map_err(|e| Error::Other(format!("opus decoder: {e}")))?;
            Dec::Opus {
                dec,
                channels,
                skip,
                pcm: vec![0.0; 5760 * channels],
            }
        } else {
            match symphonia::default::get_codecs().make(&cp, &DecoderOptions::default()) {
                Ok(d) => Dec::Sym(d),
                Err(e) => {
                    let name = symphonia::default::get_codecs()
                        .get_codec(cp.codec)
                        .map(|d| d.short_name.to_string())
                        .unwrap_or_else(|| format!("codec {}", cp.codec));
                    return Ok(Err(NotNative(format!("{name}: {e}"))));
                }
            }
        };
        let duration_ms = duration_of(&cp);
        Ok(Ok(SymphoniaSource {
            format,
            track_id,
            dec,
            duration_ms,
            pos,
            len,
            sample_buf: None,
            mono: Vec::with_capacity(8192),
            decode_errors: 0,
        }))
    }
}

impl Source for SymphoniaSource {
    fn name(&self) -> &'static str {
        match self.dec {
            Dec::Sym(_) => "symphonia",
            Dec::Opus { .. } => "opus",
        }
    }

    fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    fn bytes_fraction(&self) -> Option<f32> {
        (self.len > 0).then(|| self.pos.load(Ordering::Relaxed) as f32 / self.len as f32)
    }

    fn next_block(&mut self) -> Result<Option<(u32, &[f32])>> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    if self.decode_errors > 0 {
                        tracing::warn!(
                            packets = self.decode_errors,
                            "import: undecodable packets skipped"
                        );
                    }
                    return Ok(None);
                }
                // a new chained stream / reset: treat as the end of the first one
                Err(SymError::ResetRequired) => return Ok(None),
                Err(SymError::DecodeError(e)) => {
                    // A damaged tail (e.g. a cut-off download) ends the import with what we have.
                    tracing::warn!("import: demuxing stopped: {e}");
                    return Ok(None);
                }
                Err(e) => return Err(Error::Other(format!("reading the file: {e}"))),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            self.mono.clear();
            match &mut self.dec {
                Dec::Sym(dec) => {
                    let buf = match dec.decode(&packet) {
                        Ok(b) => b,
                        Err(SymError::DecodeError(e)) => {
                            self.decode_errors += 1;
                            tracing::debug!("import: skipping undecodable packet: {e}");
                            continue;
                        }
                        Err(e) => return Err(Error::Other(format!("decoding: {e}"))),
                    };
                    let spec = *buf.spec();
                    let frames = buf.frames();
                    if frames == 0 {
                        continue;
                    }
                    let need = buf.capacity() as u64;
                    let sb = match &mut self.sample_buf {
                        Some(sb) if sb.capacity() as u64 >= need * spec.channels.count() as u64 => {
                            sb
                        }
                        slot => slot.insert(SampleBuffer::new(need, spec)),
                    };
                    sb.copy_interleaved_ref(buf);
                    let ch = spec.channels.count().max(1);
                    downmix(sb.samples(), ch, &mut self.mono);
                    return Ok(Some((spec.rate, &self.mono)));
                }
                Dec::Opus {
                    dec,
                    channels,
                    skip,
                    pcm,
                } => {
                    let n = match dec.decode_float(&packet.data, pcm, false) {
                        Ok(n) => n,
                        Err(e) => {
                            self.decode_errors += 1;
                            tracing::debug!("import: skipping undecodable Opus packet: {e}");
                            continue;
                        }
                    };
                    let drop = (*skip).min(n);
                    *skip -= drop;
                    if n == drop {
                        continue;
                    }
                    downmix(
                        &pcm[drop * *channels..n * *channels],
                        *channels,
                        &mut self.mono,
                    );
                    return Ok(Some((48_000, &self.mono)));
                }
            }
        }
    }
}

fn downmix(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels == 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    let k = 1.0 / channels as f32;
    out.extend(
        interleaved
            .chunks_exact(channels)
            .map(|f| f.iter().sum::<f32>() * k),
    );
}

/// `ffmpeg -i <file> -f f32le -ac 1 -ar 16000 -` (the fallback for what symphonia can't read).
struct FfmpegSource {
    child: std::process::Child,
    stdout: std::process::ChildStdout,
    duration_ms: Arc<AtomicU64>,
    log: Arc<Mutex<Vec<String>>>,
    bytes: Vec<u8>,
    carry: Vec<u8>,
    samples: Vec<f32>,
}

impl FfmpegSource {
    fn spawn(path: &Path) -> std::io::Result<Self> {
        let mut cmd = std::process::Command::new("ffmpeg");
        cmd.args(["-nostdin", "-hide_banner", "-i"])
            .arg(path)
            .args([
                "-vn", "-map", "0:a:0", "-ac", "1", "-ar", "16000", "-f", "f32le", "pipe:1",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let duration_ms = Arc::new(AtomicU64::new(0));
        let log = Arc::new(Mutex::new(Vec::new()));
        {
            let (duration_ms, log) = (duration_ms.clone(), log.clone());
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
                    if duration_ms.load(Ordering::Relaxed) == 0
                        && let Some(d) = parse_ffmpeg_duration(&line)
                    {
                        duration_ms.store(d, Ordering::Relaxed);
                    }
                    let mut l = log.lock().unwrap_or_else(|p| p.into_inner());
                    l.push(line);
                    if l.len() > 20 {
                        l.remove(0);
                    }
                }
            });
        }
        Ok(FfmpegSource {
            child,
            stdout,
            duration_ms,
            log,
            bytes: vec![0; 64 * 1024],
            carry: Vec::with_capacity(4),
            samples: Vec::with_capacity(16 * 1024),
        })
    }
}

/// `  Duration: 01:02:03.45, start: ...` → ms.
fn parse_ffmpeg_duration(line: &str) -> Option<u64> {
    let rest = line.trim_start().strip_prefix("Duration:")?.trim_start();
    let hms = rest.split(',').next()?.trim();
    let mut it = hms.split(':');
    let h: u64 = it.next()?.parse().ok()?;
    let m: u64 = it.next()?.parse().ok()?;
    let s: f64 = it.next()?.parse().ok()?;
    Some(h * 3_600_000 + m * 60_000 + (s * 1000.0) as u64)
}

impl Source for FfmpegSource {
    fn name(&self) -> &'static str {
        "ffmpeg"
    }

    fn duration_ms(&self) -> Option<u64> {
        Some(self.duration_ms.load(Ordering::Relaxed)).filter(|&d| d > 0)
    }

    fn next_block(&mut self) -> Result<Option<(u32, &[f32])>> {
        loop {
            let n = self.stdout.read(&mut self.bytes)?;
            if n == 0 {
                let status = self.child.wait()?;
                if !status.success() {
                    let log = self
                        .log
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .join("\n");
                    return Err(Error::Unsupported(format!(
                        "ffmpeg could not decode the file ({status}): {log}"
                    )));
                }
                return Ok(None);
            }
            self.carry.extend_from_slice(&self.bytes[..n]);
            let whole = self.carry.len() / 4 * 4;
            if whole == 0 {
                continue;
            }
            self.samples.clear();
            self.samples.extend(
                self.carry[..whole]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b)),
            );
            self.carry.drain(..whole);
            return Ok(Some((PIPELINE_SAMPLE_RATE, &self.samples)));
        }
    }
}

impl Drop for FfmpegSource {
    fn drop(&mut self) {
        // cancelled / failed mid-way: don't leave ffmpeg running
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffmpeg_duration() {
        assert_eq!(
            parse_ffmpeg_duration("  Duration: 01:02:03.45, start: 0.000000, bitrate: 128 kb/s"),
            Some(3_723_450)
        );
        assert_eq!(parse_ffmpeg_duration("  Duration: N/A, bitrate: N/A"), None);
        assert_eq!(parse_ffmpeg_duration("Stream #0:0: Audio"), None);
    }

    #[test]
    fn downmix_averages_channels() {
        let mut out = vec![];
        downmix(&[1.0, 0.0, 0.5, 0.5], 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5]);
    }

    #[test]
    fn default_title_is_the_file_stem() {
        assert_eq!(
            default_title(Path::new("/x/Weekly sync.m4a")),
            "Weekly sync"
        );
        assert_eq!(default_title(Path::new("/x/.mp3")), ".mp3");
    }
}
