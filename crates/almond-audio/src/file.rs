//! File import: decode a WAV file into the same 16 kHz frame stream as live capture.
//! (Other containers are WS-B/import concerns; add `symphonia` here if needed.)

use crate::clock::CaptureClock;
use crate::pipeline::{AlignConfig, PipelineStream, channel_pair};
use almond_types::meeting::AudioChannel;
use almond_types::{Error, Result};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Open a WAV file (any rate / channel count / PCM or float format) as a [`PipelineStream`] of
/// `channel` frames (normally [`AudioChannel::Mixed`]). Frames are produced as fast as the
/// consumer reads them; `t0_ms` starts at 0.
pub fn open_wav(path: &Path, channel: AudioChannel, frame_ms: u32) -> Result<PipelineStream> {
    let reader = hound::WavReader::open(path)
        .map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))?;
    let spec = reader.spec();
    let (tx, mut stream) = channel_pair(
        channel,
        CaptureClock::start(),
        frame_ms,
        AlignConfig::offline(),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    stream.set_on_stop(move || stop2.store(true, Ordering::SeqCst));
    let ch = spec.channels.max(1) as usize;
    let rate = spec.sample_rate;
    std::thread::Builder::new()
        .name("almond-wav-import".into())
        .spawn(move || {
            let chunk = (rate as usize / 10).max(1) * ch; // 100 ms
            let mut buf = Vec::with_capacity(chunk);
            let mut reader = reader;
            let res: std::result::Result<(), hound::Error> = (|| {
                match spec.sample_format {
                    hound::SampleFormat::Float => {
                        for s in reader.samples::<f32>() {
                            buf.push(s?);
                            if buf.len() >= chunk && !flush(&tx, rate, ch, &mut buf, &stop) {
                                return Ok(());
                            }
                        }
                    }
                    hound::SampleFormat::Int => {
                        let scale = 1.0 / (1u64 << (spec.bits_per_sample.max(1) - 1)) as f32;
                        for s in reader.samples::<i32>() {
                            buf.push(s? as f32 * scale);
                            if buf.len() >= chunk && !flush(&tx, rate, ch, &mut buf, &stop) {
                                return Ok(());
                            }
                        }
                    }
                }
                flush(&tx, rate, ch, &mut buf, &stop);
                Ok(())
            })();
            match res {
                Ok(()) => tx.end(),
                Err(e) => tx.fatal(format!("wav decode: {e}")),
            }
        })
        .map_err(Error::Io)?;
    Ok(stream)
}

fn flush(
    tx: &crate::pipeline::RawSender,
    rate: u32,
    ch: usize,
    buf: &mut Vec<f32>,
    stop: &AtomicBool,
) -> bool {
    if stop.load(Ordering::SeqCst) {
        return false;
    }
    if buf.is_empty() {
        return true;
    }
    let mono = crate::pipeline::downmix(buf, ch);
    buf.clear();
    tx.push_blocking(rate, mono)
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::audio::AudioStream;

    #[test]
    fn imports_stereo_44k_wav() {
        let dir = std::env::temp_dir().join(format!("almond-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&p, spec).unwrap();
        for i in 0..44_100 * 2 {
            let v = ((i as f32 / 44_100.0 * 300.0).sin() * 10_000.0) as i16;
            w.write_sample(v).unwrap();
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let total = rt.block_on(async {
            let mut s = open_wav(&p, AudioChannel::Mixed, 100).unwrap();
            let mut n = 0;
            while let Some(f) = s.next().await {
                n += f.unwrap().samples.len();
            }
            n
        });
        assert!((31_500..=32_000).contains(&total), "{total}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
