//! Chunked Ogg Opus writer + merger.
//!
//! One `opus::Encoder` runs for the whole recording; its packet stream is cut into chunk files
//! (`<prefix>-00000.opus`, ...), each a complete Ogg Opus stream (own headers, own serial) whose
//! granule positions continue the *global* timeline. Pages are closed every second and flushed
//! to the OS, so a crash loses at most ~1 s; Ogg's page CRCs let the reader stop cleanly at a
//! torn page. [`merge`] re-muxes the chunks (no re-encode) into one continuous stream with exact
//! sample count, used both on normal finish and on crash recovery.

use almond_types::{Error, Result};
use ogg::writing::PacketWriteEndInfo;
use ogg::{PacketReader, PacketWriter};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Encoder input rate (= pipeline rate).
pub const RATE: u32 = 16_000;
/// 20 ms frames.
pub const FRAME: usize = 320;
/// Ogg Opus granules are always 48 kHz.
const GRANULE_PER_SAMPLE: u64 = 3;
const GRANULE_PER_FRAME: u64 = FRAME as u64 * GRANULE_PER_SAMPLE;
/// Close an Ogg page every 50 packets = 1 s.
const PACKETS_PER_PAGE: u32 = 50;
const MAX_PACKET: usize = 4000;

fn opus_err(e: opus::Error) -> Error {
    Error::Other(format!("opus: {e}"))
}

fn opus_head(pre_skip: u16) -> Vec<u8> {
    let mut h = Vec::with_capacity(19);
    h.extend_from_slice(b"OpusHead");
    h.push(1); // version
    h.push(1); // channels
    h.extend_from_slice(&pre_skip.to_le_bytes());
    h.extend_from_slice(&RATE.to_le_bytes()); // original input rate
    h.extend_from_slice(&0i16.to_le_bytes()); // output gain
    h.push(0); // mapping family
    h
}

fn opus_tags(chunk: u32) -> Vec<u8> {
    let vendor = format!(
        "almond-audio {} / libopus {}",
        env!("CARGO_PKG_VERSION"),
        opus::version()
    );
    let comment = format!("ALMOND_CHUNK={chunk}");
    let mut t = Vec::new();
    t.extend_from_slice(b"OpusTags");
    t.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    t.extend_from_slice(vendor.as_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&(comment.len() as u32).to_le_bytes());
    t.extend_from_slice(comment.as_bytes());
    t
}

fn serial_for(seed: u32) -> u32 {
    // Distinct, deterministic-per-run serials (only uniqueness within a file matters).
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    t ^ seed.wrapping_mul(0x9E37_79B9)
}

struct Chunk {
    pw: PacketWriter<'static, BufWriter<File>>,
    serial: u32,
    in_page: u32,
    packets: u64,
}

impl Chunk {
    fn file(&mut self) -> &mut BufWriter<File> {
        self.pw.inner_mut()
    }
}

/// Writes one channel as chunked Ogg Opus.
pub struct OpusChannelWriter {
    dir: PathBuf,
    prefix: String,
    chunk_packets: u64,
    enc: opus::Encoder,
    pre_skip: u16,
    lookahead: u64,
    pending: Vec<f32>,
    /// Real (non-padding) samples received.
    real_samples: u64,
    packets_total: u64,
    chunk_idx: u32,
    cur: Option<Chunk>,
    last_sync: Instant,
    fsync_every: Duration,
    pkt_buf: Vec<u8>,
}

impl OpusChannelWriter {
    pub fn create(
        dir: &Path,
        prefix: &str,
        chunk_secs: u32,
        bitrate: i32,
        fsync_every: Duration,
    ) -> Result<Self> {
        let mut enc = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)
            .map_err(opus_err)?;
        enc.set_bitrate(opus::Bitrate::Bits(bitrate))
            .map_err(opus_err)?;
        enc.set_vbr(true).map_err(opus_err)?;
        let lookahead = enc.get_lookahead().map_err(opus_err)?.max(0) as u64;
        let pre_skip = (lookahead * GRANULE_PER_SAMPLE) as u16;
        Ok(Self {
            dir: dir.to_path_buf(),
            prefix: prefix.to_string(),
            chunk_packets: (chunk_secs.max(1) as u64 * RATE as u64 / FRAME as u64).max(1),
            enc,
            pre_skip,
            lookahead,
            pending: Vec::with_capacity(FRAME * 8),
            real_samples: 0,
            packets_total: 0,
            chunk_idx: 0,
            cur: None,
            last_sync: Instant::now(),
            fsync_every,
            pkt_buf: vec![0u8; MAX_PACKET],
        })
    }

    pub fn samples(&self) -> u64 {
        self.real_samples
    }

    pub fn push(&mut self, samples: &[f32]) -> Result<()> {
        self.real_samples += samples.len() as u64;
        self.pending.extend_from_slice(samples);
        let mut off = 0;
        while self.pending.len() - off >= FRAME {
            let frame: Vec<f32> = self.pending[off..off + FRAME].to_vec();
            off += FRAME;
            self.encode_and_write(&frame, None)?;
        }
        self.pending.drain(..off);
        Ok(())
    }

    /// Flush the tail (padded so the encoder's look-ahead is emitted), end the stream and fsync.
    pub fn finish(mut self) -> Result<()> {
        let final_granule = self.pre_skip as u64 + self.real_samples * GRANULE_PER_SAMPLE;
        let encoded = self.packets_total * FRAME as u64;
        let needed = self.real_samples + self.lookahead;
        let frames_left = needed.saturating_sub(encoded).div_ceil(FRAME as u64).max(1);
        let mut tail = std::mem::take(&mut self.pending);
        tail.resize(frames_left as usize * FRAME, 0.0);
        for (i, frame) in tail.chunks(FRAME).enumerate() {
            let last = i as u64 + 1 == frames_left;
            self.encode_and_write(frame, Some((final_granule, last)))?;
        }
        Ok(())
    }

    fn open_chunk(&mut self) -> Result<()> {
        let path = self
            .dir
            .join(format!("{}-{:05}.opus", self.prefix, self.chunk_idx));
        let f = File::create(&path)?;
        let mut pw = PacketWriter::new(BufWriter::new(f));
        let serial = serial_for(self.chunk_idx + 1);
        pw.write_packet(
            opus_head(self.pre_skip),
            serial,
            PacketWriteEndInfo::EndPage,
            0,
        )?;
        pw.write_packet(
            opus_tags(self.chunk_idx),
            serial,
            PacketWriteEndInfo::EndPage,
            0,
        )?;
        pw.inner_mut().flush()?;
        self.cur = Some(Chunk {
            pw,
            serial,
            in_page: 0,
            packets: 0,
        });
        Ok(())
    }

    /// `fin = Some((final_granule, is_last))` while flushing the tail.
    fn encode_and_write(&mut self, frame: &[f32], fin: Option<(u64, bool)>) -> Result<()> {
        let n = self
            .enc
            .encode_float(frame, &mut self.pkt_buf)
            .map_err(opus_err)?;
        let packet = self.pkt_buf[..n].to_vec();
        if self.cur.is_none() {
            self.open_chunk()?;
        }
        let mut granule = self.pre_skip as u64 + (self.packets_total + 1) * GRANULE_PER_FRAME;
        let mut last = false;
        if let Some((final_granule, is_last)) = fin {
            granule = granule.min(final_granule);
            if is_last {
                granule = final_granule;
                last = true;
            }
        }
        let chunk_packets = self.chunk_packets;
        let c = self.cur.as_mut().expect("chunk open");
        let end_stream = last || c.packets + 1 >= chunk_packets;
        let info = if end_stream {
            PacketWriteEndInfo::EndStream
        } else if c.in_page + 1 >= PACKETS_PER_PAGE {
            PacketWriteEndInfo::EndPage
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        c.pw.write_packet(packet, c.serial, info, granule)?;
        c.packets += 1;
        self.packets_total += 1;
        if info == PacketWriteEndInfo::NormalPacket {
            c.in_page += 1;
            return Ok(());
        }
        c.in_page = 0;
        c.file().flush()?;
        if end_stream {
            c.file().get_ref().sync_data()?;
            self.cur = None;
            self.chunk_idx += 1;
            self.last_sync = Instant::now();
        } else if self.last_sync.elapsed() >= self.fsync_every {
            c.file().get_ref().sync_data()?;
            self.last_sync = Instant::now();
        }
        Ok(())
    }
}

/// Result of merging chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeInfo {
    pub samples: u64,
    pub packets: u64,
    /// The last chunk had no end-of-stream page (recording was interrupted).
    pub truncated: bool,
}

/// Re-mux chunk files (in order) into one continuous Ogg Opus stream at `out`.
pub fn merge(chunks: &[PathBuf], out: &Path) -> Result<MergeInfo> {
    if chunks.is_empty() {
        return Err(Error::NotFound("no opus chunks".into()));
    }
    let f = File::create(out)?;
    let mut pw = PacketWriter::new(BufWriter::new(f));
    let serial = serial_for(0xA1);
    let mut pre_skip: Option<u16> = None;
    let mut held: VecDeque<Vec<u8>> = VecDeque::new();
    let mut written: u64 = 0;
    let mut in_page = 0u32;
    let mut last_eos_granule: Option<u64> = None;
    let mut total_packets: u64 = 0;

    for (ci, path) in chunks.iter().enumerate() {
        let mut rdr = PacketReader::new(BufReader::new(File::open(path)?));
        let mut idx = 0u64;
        last_eos_granule = None;
        loop {
            let pkt = match rdr.read_packet() {
                Ok(Some(p)) => p,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(chunk = %path.display(), "stopping at damaged page: {e}");
                    break;
                }
            };
            let i = idx;
            idx += 1;
            if i == 0 {
                if !pkt.data.starts_with(b"OpusHead") || pkt.data.len() < 19 {
                    return Err(Error::Invalid(format!("{}: not Ogg Opus", path.display())));
                }
                if ci == 0 {
                    let ps = u16::from_le_bytes([pkt.data[10], pkt.data[11]]);
                    pre_skip = Some(ps);
                    pw.write_packet(pkt.data, serial, PacketWriteEndInfo::EndPage, 0)?;
                }
                continue;
            }
            if i == 1 {
                if ci == 0 {
                    pw.write_packet(opus_tags(0), serial, PacketWriteEndInfo::EndPage, 0)?;
                }
                continue;
            }
            if pkt.last_in_stream() {
                last_eos_granule = Some(pkt.absgp_page());
            }
            total_packets += 1;
            held.push_back(pkt.data);
            // Keep the last 3 packets back: their granules may need clamping to the final one.
            while held.len() > 3 {
                let p = held.pop_front().expect("non-empty");
                let g = pre_skip.unwrap_or(0) as u64 + (written + 1) * GRANULE_PER_FRAME;
                write_audio(&mut pw, serial, p, g, &mut in_page, false)?;
                written += 1;
            }
        }
    }
    let pre_skip = pre_skip.ok_or_else(|| Error::Invalid("first chunk has no OpusHead".into()))?;
    let truncated = last_eos_granule.is_none();
    let final_granule = last_eos_granule
        .unwrap_or(pre_skip as u64 + total_packets * GRANULE_PER_FRAME)
        .max(pre_skip as u64);
    if held.is_empty() {
        // Headers only: emit one silent-length packet is not possible without an encoder; write
        // an empty EOS page by re-using the header-less end marker.
        pw.write_packet(
            Vec::new(),
            serial,
            PacketWriteEndInfo::EndStream,
            pre_skip as u64,
        )?;
    }
    while let Some(p) = held.pop_front() {
        let last = held.is_empty();
        let g = if last {
            final_granule
        } else {
            (pre_skip as u64 + (written + 1) * GRANULE_PER_FRAME).min(final_granule)
        };
        write_audio(&mut pw, serial, p, g, &mut in_page, last)?;
        written += 1;
    }
    let mut w = pw.into_inner();
    w.flush()?;
    w.get_ref().sync_all()?;
    Ok(MergeInfo {
        samples: (final_granule - pre_skip as u64) / GRANULE_PER_SAMPLE,
        packets: written,
        truncated,
    })
}

fn write_audio(
    pw: &mut PacketWriter<'static, BufWriter<File>>,
    serial: u32,
    p: Vec<u8>,
    granule: u64,
    in_page: &mut u32,
    last: bool,
) -> Result<()> {
    let info = if last {
        PacketWriteEndInfo::EndStream
    } else if *in_page + 1 >= PACKETS_PER_PAGE {
        PacketWriteEndInfo::EndPage
    } else {
        PacketWriteEndInfo::NormalPacket
    };
    pw.write_packet(p, serial, info, granule)?;
    *in_page = if info == PacketWriteEndInfo::NormalPacket {
        *in_page + 1
    } else {
        0
    };
    Ok(())
}

/// Decode an Ogg Opus file to 16 kHz mono f32 (pre-skip and end trimming applied). Used by tests
/// and by consumers that need PCM back from an archived track.
pub fn decode(path: &Path) -> Result<Vec<f32>> {
    let mut rdr = PacketReader::new(BufReader::new(File::open(path)?));
    let mut dec = opus::Decoder::new(RATE, opus::Channels::Mono).map_err(opus_err)?;
    let mut out = Vec::new();
    let mut pre_skip = 0usize;
    let mut idx = 0u64;
    let mut final_granule = None;
    let mut buf = vec![0f32; 5760];
    loop {
        let pkt = match rdr.read_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(_) => break,
        };
        let i = idx;
        idx += 1;
        if i == 0 {
            if pkt.data.len() >= 12 {
                pre_skip = u16::from_le_bytes([pkt.data[10], pkt.data[11]]) as usize
                    / GRANULE_PER_SAMPLE as usize;
            }
            continue;
        }
        if i == 1 || pkt.data.is_empty() {
            if pkt.last_in_stream() {
                final_granule = Some(pkt.absgp_page());
            }
            continue;
        }
        let n = dec
            .decode_float(&pkt.data, &mut buf, false)
            .map_err(opus_err)?;
        out.extend_from_slice(&buf[..n]);
        if pkt.last_in_stream() {
            final_granule = Some(pkt.absgp_page());
        }
    }
    let start = pre_skip.min(out.len());
    let mut v = out.split_off(start);
    if let Some(g) = final_granule {
        let total = (g / GRANULE_PER_SAMPLE) as usize;
        v.truncate(total.saturating_sub(pre_skip));
    }
    Ok(v)
}
