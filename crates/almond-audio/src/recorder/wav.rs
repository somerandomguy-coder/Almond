//! Chunked 16-bit PCM WAV writer + merger (fallback codec, and the only one without the `opus`
//! feature). The header is rewritten on every flush (1 s), so an interrupted chunk is readable;
//! if it is not (torn header), the merger falls back to the raw `data` bytes.

use almond_types::{Error, Result};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const RATE: u32 = 16_000;

fn spec() -> hound::WavSpec {
    hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    }
}

fn wav_err(e: hound::Error) -> Error {
    match e {
        hound::Error::IoError(io) => Error::Io(io),
        other => Error::Other(format!("wav: {other}")),
    }
}

fn to_i16(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

pub struct WavChannelWriter {
    dir: PathBuf,
    prefix: String,
    chunk_samples: u64,
    chunk_idx: u32,
    in_chunk: u64,
    samples: u64,
    cur: Option<hound::WavWriter<BufWriter<File>>>,
    last_flush: Instant,
    last_sync: Instant,
    fsync_every: Duration,
}

impl WavChannelWriter {
    pub fn create(
        dir: &Path,
        prefix: &str,
        chunk_secs: u32,
        fsync_every: Duration,
    ) -> Result<Self> {
        Ok(Self {
            dir: dir.to_path_buf(),
            prefix: prefix.to_string(),
            chunk_samples: chunk_secs.max(1) as u64 * RATE as u64,
            chunk_idx: 0,
            in_chunk: 0,
            samples: 0,
            cur: None,
            last_flush: Instant::now(),
            last_sync: Instant::now(),
            fsync_every,
        })
    }

    pub fn samples(&self) -> u64 {
        self.samples
    }

    pub fn push(&mut self, samples: &[f32]) -> Result<()> {
        let mut rest = samples;
        while !rest.is_empty() {
            if self.cur.is_none() {
                let p = self
                    .dir
                    .join(format!("{}-{:05}.wav", self.prefix, self.chunk_idx));
                self.cur = Some(hound::WavWriter::create(p, spec()).map_err(wav_err)?);
                self.in_chunk = 0;
            }
            let room = (self.chunk_samples - self.in_chunk) as usize;
            let (now, later) = rest.split_at(room.min(rest.len()));
            let w = self.cur.as_mut().expect("open");
            for &x in now {
                w.write_sample(to_i16(x)).map_err(wav_err)?;
            }
            self.in_chunk += now.len() as u64;
            self.samples += now.len() as u64;
            rest = later;
            if self.in_chunk >= self.chunk_samples {
                self.close_chunk()?;
            }
        }
        if self.last_flush.elapsed() >= Duration::from_secs(1)
            && let Some(w) = self.cur.as_mut()
        {
            w.flush().map_err(wav_err)?;
            self.last_flush = Instant::now();
        }
        if self.last_sync.elapsed() >= self.fsync_every {
            self.sync_current()?;
        }
        Ok(())
    }

    fn sync_current(&mut self) -> Result<()> {
        if let Some(w) = self.cur.as_mut() {
            w.flush().map_err(wav_err)?;
            let p = self
                .dir
                .join(format!("{}-{:05}.wav", self.prefix, self.chunk_idx));
            super::fsync_path(&p)?;
        }
        self.last_sync = Instant::now();
        Ok(())
    }

    fn close_chunk(&mut self) -> Result<()> {
        if let Some(w) = self.cur.take() {
            w.finalize().map_err(wav_err)?;
            let p = self
                .dir
                .join(format!("{}-{:05}.wav", self.prefix, self.chunk_idx));
            super::fsync_path(&p)?;
            self.chunk_idx += 1;
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        if self.cur.is_none() && self.samples == 0 {
            // Produce an (empty) chunk so the channel has a file.
            let p = self.dir.join(format!("{}-{:05}.wav", self.prefix, 0));
            hound::WavWriter::create(p, spec())
                .map_err(wav_err)?
                .finalize()
                .map_err(wav_err)?;
            return Ok(());
        }
        self.close_chunk()
    }
}

/// Read all samples of a chunk we wrote, tolerating a stale/torn header: the RIFF/`data` sizes
/// are ignored and everything after the `data` chunk header is taken (we never write trailing
/// chunks), so samples flushed after the last header rewrite are not lost.
fn read_chunk(path: &Path) -> Result<Vec<i16>> {
    let mut f = File::open(path)?;
    let mut hdr = [0u8; 12];
    f.read_exact(&mut hdr)?;
    if &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
        return Err(Error::Invalid(format!(
            "{}: not a WAV file",
            path.display()
        )));
    }
    loop {
        let mut ch = [0u8; 8];
        if f.read_exact(&mut ch).is_err() {
            return Ok(vec![]); // header torn before `data`
        }
        let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as i64;
        if &ch[0..4] == b"data" {
            break;
        }
        f.seek(SeekFrom::Current(size + (size & 1)))?;
    }
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect())
}

/// Concatenate chunks into one WAV at `out`; returns the sample count.
pub fn merge(chunks: &[PathBuf], out: &Path) -> Result<u64> {
    let mut w = hound::WavWriter::create(out, spec()).map_err(wav_err)?;
    let mut n = 0u64;
    for c in chunks {
        for s in read_chunk(c)? {
            w.write_sample(s).map_err(wav_err)?;
            n += 1;
        }
    }
    w.finalize().map_err(wav_err)?;
    super::fsync_path(out)?;
    Ok(n)
}

/// Decode a WAV track back to f32 (tests / consumers).
pub fn decode(path: &Path) -> Result<Vec<f32>> {
    Ok(read_chunk(path)?
        .into_iter()
        .map(|s| s as f32 / 32768.0)
        .collect())
}
