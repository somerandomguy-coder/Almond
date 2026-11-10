//! Crash-safe per-channel archive of a recording.
//!
//! Layout of a session directory:
//! ```text
//! <dir>/session.json          manifest (codec, start time, finished flag, resulting tracks)
//! <dir>/mic-00000.opus        chunk 0 of the mic channel (each chunk = complete Ogg Opus stream)
//! <dir>/mic-00001.opus        ...
//! <dir>/system-00000.opus
//! ```
//! [`SessionRecorder::finish`] closes the chunks, merges each channel into one file (no
//! re-encode), stores it with [`BlobStore::put_file`] and returns [`AudioTrack`]s. After a crash,
//! [`recover_session`] does the same from whatever reached the disk (≤ 1 s lost; ≤ `fsync_secs`
//! on power loss). [`unfinished_sessions`] finds sessions to recover at start-up.
//!
//! Why Opus: ~24 kbit/s speech-transparent at 16 kHz (≈ 11 MB/h/channel vs 115 MB/h for WAV),
//! and Ogg pages are self-delimiting with CRCs, which makes torn-write recovery trivial. WAV is
//! kept as a fallback codec (no C dependency).

#[cfg(feature = "opus")]
pub mod ogg_opus;
pub mod wav;

use almond_types::audio::{AudioFrame, PIPELINE_SAMPLE_RATE};
use almond_types::meeting::{AudioChannel, AudioTrack};
use almond_types::store::BlobStore;
use almond_types::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MANIFEST: &str = "session.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    Opus,
    Wav,
}

impl Codec {
    pub fn as_str(self) -> &'static str {
        match self {
            Codec::Opus => "opus",
            Codec::Wav => "wav",
        }
    }
    fn ext(self) -> &'static str {
        self.as_str()
    }
}

impl Default for Codec {
    fn default() -> Self {
        if cfg!(feature = "opus") {
            Codec::Opus
        } else {
            Codec::Wav
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecorderConfig {
    pub codec: Codec,
    /// Seconds per chunk file.
    pub chunk_secs: u32,
    /// Opus target bitrate (bit/s).
    pub opus_bitrate: i32,
    /// fsync interval (pages are flushed to the OS every second regardless).
    pub fsync_secs: u32,
    /// Keep chunk + merged files after they were stored in the blob store.
    pub keep_files: bool,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        Self {
            codec: Codec::default(),
            chunk_secs: 300,
            opus_bitrate: 24_000,
            fsync_secs: 5,
            keep_files: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    started_at_unix_ms: u64,
    codec: Codec,
    sample_rate: u32,
    finished: bool,
    #[serde(default)]
    tracks: Vec<AudioTrack>,
}

fn read_manifest(dir: &Path) -> Result<Manifest> {
    let bytes = std::fs::read(dir.join(MANIFEST))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// fsync a file by path (Windows' FlushFileBuffers needs a writable handle).
pub(crate) fn fsync_path(p: &Path) -> Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(p)?
        .sync_all()?;
    Ok(())
}

fn write_manifest(dir: &Path, m: &Manifest) -> Result<()> {
    // Write-then-rename so the manifest is never torn.
    let tmp = dir.join(format!("{MANIFEST}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(m)?)?;
    fsync_path(&tmp)?;
    std::fs::rename(&tmp, dir.join(MANIFEST))?;
    Ok(())
}

pub fn channel_prefix(c: AudioChannel) -> &'static str {
    match c {
        AudioChannel::Mic => "mic",
        AudioChannel::System => "system",
        AudioChannel::Mixed => "mixed",
    }
}

const ALL_CHANNELS: [AudioChannel; 3] =
    [AudioChannel::Mic, AudioChannel::System, AudioChannel::Mixed];

enum Writer {
    #[cfg(feature = "opus")]
    Opus(ogg_opus::OpusChannelWriter),
    Wav(wav::WavChannelWriter),
}

impl Writer {
    fn create(dir: &Path, ch: AudioChannel, cfg: &RecorderConfig) -> Result<Self> {
        let fsync = Duration::from_secs(cfg.fsync_secs.max(1) as u64);
        let prefix = channel_prefix(ch);
        match cfg.codec {
            #[cfg(feature = "opus")]
            Codec::Opus => Ok(Writer::Opus(ogg_opus::OpusChannelWriter::create(
                dir,
                prefix,
                cfg.chunk_secs,
                cfg.opus_bitrate,
                fsync,
            )?)),
            #[cfg(not(feature = "opus"))]
            Codec::Opus => Err(Error::Unsupported(
                "almond-audio built without the `opus` feature".into(),
            )),
            Codec::Wav => Ok(Writer::Wav(wav::WavChannelWriter::create(
                dir,
                prefix,
                cfg.chunk_secs,
                fsync,
            )?)),
        }
    }
    fn samples(&self) -> u64 {
        match self {
            #[cfg(feature = "opus")]
            Writer::Opus(w) => w.samples(),
            Writer::Wav(w) => w.samples(),
        }
    }
    fn push(&mut self, s: &[f32]) -> Result<()> {
        match self {
            #[cfg(feature = "opus")]
            Writer::Opus(w) => w.push(s),
            Writer::Wav(w) => w.push(s),
        }
    }
    fn finish(self) -> Result<()> {
        match self {
            #[cfg(feature = "opus")]
            Writer::Opus(w) => w.finish(),
            Writer::Wav(w) => w.finish(),
        }
    }
}

/// Records pipeline frames of any number of channels into a session directory.
pub struct SessionRecorder {
    dir: PathBuf,
    cfg: RecorderConfig,
    writers: Vec<(AudioChannel, Writer)>,
}

impl SessionRecorder {
    /// Create `dir` (must not already hold a session) and write the manifest.
    /// `started_at_unix_ms` = wall-clock time of `t0_ms == 0` (see `CaptureClock::epoch_unix_ms`).
    pub fn create(dir: &Path, started_at_unix_ms: u64, cfg: RecorderConfig) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        if dir.join(MANIFEST).exists() {
            return Err(Error::Invalid(format!(
                "{} already contains a recording session",
                dir.display()
            )));
        }
        write_manifest(
            dir,
            &Manifest {
                version: 1,
                started_at_unix_ms,
                codec: cfg.codec,
                sample_rate: PIPELINE_SAMPLE_RATE,
                finished: false,
                tracks: vec![],
            },
        )?;
        Ok(Self {
            dir: dir.to_path_buf(),
            cfg,
            writers: vec![],
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append a frame. Frames of one channel must arrive in order; a frame starting after the
    /// channel's current end is preceded by silence, overlapping samples are discarded, so the
    /// file timeline always equals the capture clock.
    pub fn push(&mut self, frame: &AudioFrame) -> Result<()> {
        let idx = match self.writers.iter().position(|(c, _)| *c == frame.channel) {
            Some(i) => i,
            None => {
                let w = Writer::create(&self.dir, frame.channel, &self.cfg)?;
                self.writers.push((frame.channel, w));
                self.writers.len() - 1
            }
        };
        let w = &mut self.writers[idx].1;
        let start = frame.t0_ms * PIPELINE_SAMPLE_RATE as u64 / 1000;
        let have = w.samples();
        if start > have {
            let gap = (start - have) as usize;
            w.push(&vec![0.0; gap])?;
            w.push(&frame.samples)
        } else {
            let skip = ((have - start) as usize).min(frame.samples.len());
            w.push(&frame.samples[skip..])
        }
    }

    /// Close all channels, merge, store in `blobs`, and return the tracks.
    pub fn finish(self, blobs: &dyn BlobStore) -> Result<Vec<AudioTrack>> {
        let mut first_err = None;
        for (_, w) in self.writers {
            if let Err(e) = w.finish() {
                first_err.get_or_insert(e);
            }
        }
        if let Some(e) = first_err {
            return Err(e);
        }
        finalize(&self.dir, blobs, self.cfg.keep_files)
    }
}

/// Finish a session that was interrupted (crash, power loss). Idempotent: a finished session
/// returns its recorded tracks.
pub fn recover_session(
    dir: &Path,
    blobs: &dyn BlobStore,
    keep_files: bool,
) -> Result<Vec<AudioTrack>> {
    let m = read_manifest(dir)?;
    if m.finished {
        return Ok(m.tracks);
    }
    finalize(dir, blobs, keep_files)
}

/// Session directories under `root` whose recording never finished.
pub fn unfinished_sessions(root: &Path) -> Result<Vec<PathBuf>> {
    let mut v = vec![];
    if !root.exists() {
        return Ok(v);
    }
    for e in std::fs::read_dir(root)? {
        let p = e?.path();
        if p.is_dir()
            && let Ok(m) = read_manifest(&p)
            && !m.finished
        {
            v.push(p);
        }
    }
    v.sort();
    Ok(v)
}

fn chunk_files(dir: &Path, ch: AudioChannel, codec: Codec) -> Result<Vec<PathBuf>> {
    let prefix = format!("{}-", channel_prefix(ch));
    let ext = codec.ext();
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.starts_with(&prefix)
                && p.extension().and_then(|x| x.to_str()) == Some(ext)
                && name[prefix.len()..]
                    .trim_end_matches(&format!(".{ext}"))
                    .chars()
                    .all(|c| c.is_ascii_digit())
        })
        .collect();
    v.sort();
    Ok(v)
}

fn finalize(dir: &Path, blobs: &dyn BlobStore, keep_files: bool) -> Result<Vec<AudioTrack>> {
    let mut m = read_manifest(dir)?;
    let mut tracks = vec![];
    let mut to_delete = vec![];
    for ch in ALL_CHANNELS {
        let chunks = chunk_files(dir, ch, m.codec)?;
        if chunks.is_empty() {
            continue;
        }
        let out = dir.join(format!("{}.{}", channel_prefix(ch), m.codec.ext()));
        let samples = match m.codec {
            #[cfg(feature = "opus")]
            Codec::Opus => {
                let info = ogg_opus::merge(&chunks, &out)?;
                if info.truncated {
                    tracing::warn!(dir = %dir.display(), ?ch, "recovered interrupted recording");
                }
                info.samples
            }
            #[cfg(not(feature = "opus"))]
            Codec::Opus => {
                return Err(Error::Unsupported(
                    "session uses opus but almond-audio was built without it".into(),
                ));
            }
            Codec::Wav => wav::merge(&chunks, &out)?,
        };
        let blob = blobs.put_file(&out)?;
        tracks.push(AudioTrack {
            channel: ch,
            blob,
            codec: m.codec.as_str().to_string(),
            sample_rate: m.sample_rate,
            offset_ms: 0,
            duration_ms: (samples * 1000 / m.sample_rate as u64) as u32,
        });
        to_delete.extend(chunks);
        to_delete.push(out);
    }
    m.finished = true;
    m.tracks = tracks.clone();
    write_manifest(dir, &m)?;
    if !keep_files {
        for p in to_delete {
            let _ = std::fs::remove_file(p);
        }
    }
    Ok(tracks)
}

/// Decode an archived track file back to 16 kHz mono f32.
pub fn decode_track(path: &Path, codec: Codec) -> Result<Vec<f32>> {
    match codec {
        #[cfg(feature = "opus")]
        Codec::Opus => ogg_opus::decode(path),
        #[cfg(not(feature = "opus"))]
        Codec::Opus => Err(Error::Unsupported("built without opus".into())),
        Codec::Wav => wav::decode(path),
    }
}

#[cfg(test)]
mod tests;
