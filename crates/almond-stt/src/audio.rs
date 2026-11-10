//! Small audio helpers: WAV loading, resampling to the 16 kHz pipeline rate, and an energy-based
//! voice-activity splitter used to (a) skip silence before decoding (Whisper hallucinates on
//! silence) and (b) cut long recordings into utterances for per-segment language ID.

use almond_types::audio::PIPELINE_SAMPLE_RATE;
use almond_types::{Error, Result};
use std::path::Path;

/// Pipeline sample rate as usize, for index arithmetic.
pub const SR: usize = PIPELINE_SAMPLE_RATE as usize;

/// Samples -> milliseconds at 16 kHz.
pub fn samples_to_ms(n: usize) -> u64 {
    (n as u64 * 1000) / SR as u64
}

/// Milliseconds -> samples at 16 kHz.
pub fn ms_to_samples(ms: u64) -> usize {
    (ms as usize * SR) / 1000
}

/// Load a WAV file as 16 kHz mono f32. Multi-channel files are down-mixed; other rates are
/// resampled (linear interpolation — fine for ASR input).
pub fn load_wav_16k_mono(path: impl AsRef<Path>) -> Result<Vec<f32>> {
    let (ch, sr) = load_wav_channels(path)?;
    let mono = downmix(&ch);
    Ok(resample_linear(&mono, sr, PIPELINE_SAMPLE_RATE))
}

/// Load a WAV file keeping its channels separate (for 2-channel mic/system recordings).
/// Returns (channels, sample_rate).
pub fn load_wav_channels(path: impl AsRef<Path>) -> Result<(Vec<Vec<f32>>, u32)> {
    let path = path.as_ref();
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| Error::Invalid(format!("cannot open wav {}: {e}", path.display())))?;
    let spec = reader.spec();
    let n_ch = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()
            .map_err(Error::other)?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.saturating_sub(1))) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<std::result::Result<_, _>>()
                .map_err(Error::other)?
        }
    };
    let mut chans = vec![Vec::with_capacity(interleaved.len() / n_ch); n_ch];
    for (i, s) in interleaved.into_iter().enumerate() {
        chans[i % n_ch].push(s);
    }
    Ok((chans, spec.sample_rate))
}

/// Average all channels into one.
pub fn downmix(chans: &[Vec<f32>]) -> Vec<f32> {
    match chans.len() {
        0 => vec![],
        1 => chans[0].clone(),
        n => {
            let len = chans.iter().map(Vec::len).min().unwrap_or(0);
            (0..len)
                .map(|i| chans.iter().map(|c| c[i]).sum::<f32>() / n as f32)
                .collect()
        }
    }
}

/// Linear-interpolation resampler.
pub fn resample_linear(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || x.is_empty() {
        return x.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let out_len = ((x.len() as f64) / ratio).floor() as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let i0 = pos.floor() as usize;
            let frac = (pos - i0 as f64) as f32;
            let a = x[i0.min(x.len() - 1)];
            let b = x[(i0 + 1).min(x.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

/// Write 16 kHz mono f32 samples as a 16-bit PCM WAV (used by tests/eval tooling).
pub fn write_wav_16k(path: impl AsRef<Path>, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: PIPELINE_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(Error::other)?;
    for s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .map_err(Error::other)?;
    }
    w.finalize().map_err(Error::other)
}

/// Root-mean-square energy.
pub fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
}

/// Parameters for [`split_on_silence`].
#[derive(Debug, Clone)]
pub struct VadParams {
    /// Analysis frame (ms).
    pub frame_ms: u64,
    /// Frames with RMS below `max(abs_threshold, noise_floor * rel_factor)` are silence.
    pub abs_threshold: f32,
    pub rel_factor: f32,
    /// Silence at least this long splits utterances.
    pub min_silence_ms: u64,
    /// Drop speech regions shorter than this.
    pub min_speech_ms: u64,
    /// Hard cap on a region (Whisper's window is 30 s); longer regions are split at the
    /// quietest frame in the last third.
    pub max_region_ms: u64,
    /// Padding added around each region.
    pub pad_ms: u64,
}

impl Default for VadParams {
    fn default() -> Self {
        VadParams {
            frame_ms: 30,
            // Very low on purpose: some FLEURS files peak at 0.002 frame RMS (-54 dBFS) and
            // quiet laptop mics are similar. The relative (noise-floor) term does the real work.
            abs_threshold: 0.0003,
            rel_factor: 2.5,
            min_silence_ms: 500,
            min_speech_ms: 250,
            max_region_ms: 28_000,
            pad_ms: 150,
        }
    }
}

/// Energy-based voice activity regions as (start_sample, end_sample), end exclusive.
pub fn split_on_silence(x: &[f32], p: &VadParams) -> Vec<(usize, usize)> {
    let frame = ms_to_samples(p.frame_ms).max(1);
    let energies: Vec<f32> = x.chunks(frame).map(rms).collect();
    if energies.is_empty() {
        return vec![];
    }
    // Noise floor = 10th percentile of frame energies; threshold = floor * rel_factor, lowered
    // towards half the 75th percentile (a typical speech level) for audio with few pauses, but
    // never below 1.5 x floor (very quiet recordings: speech only ~2x above the noise).
    let mut sorted = energies.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let floor = sorted[sorted.len() / 10];
    let p75 = sorted[sorted.len() * 3 / 4];
    let thr = p
        .abs_threshold
        .max((floor * p.rel_factor).min((p75 * 0.5).max(floor * 1.5)));
    // Pure low-level noise (no dynamic range, quiet) is not speech: don't hand it to a decoder.
    let max_e = sorted[sorted.len() - 1];
    if max_e < 0.01 && max_e < 4.0 * floor.max(1e-6) {
        return vec![];
    }
    let voiced: Vec<bool> = energies.iter().map(|e| *e >= thr).collect();

    let min_sil = (p.min_silence_ms / p.frame_ms).max(1) as usize;
    let mut regions: Vec<(usize, usize)> = vec![]; // in frames
    let mut i = 0;
    while i < voiced.len() {
        if !voiced[i] {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i + 1;
        let mut j = i + 1;
        let mut silent_run = 0;
        while j < voiced.len() {
            if voiced[j] {
                silent_run = 0;
                end = j + 1;
            } else {
                silent_run += 1;
                if silent_run >= min_sil {
                    break;
                }
            }
            j += 1;
        }
        regions.push((start, end));
        i = j;
    }

    let max_frames = (p.max_region_ms / p.frame_ms).max(1) as usize;
    let mut split: Vec<(usize, usize)> = vec![];
    for (mut s, e) in regions {
        while e - s > max_frames {
            // cut at the quietest frame within the last third of the allowed window
            let lo = s + max_frames * 2 / 3;
            let hi = s + max_frames;
            let cut = (lo..hi)
                .min_by(|a, b| energies[*a].total_cmp(&energies[*b]))
                .unwrap_or(hi);
            split.push((s, cut));
            s = cut;
        }
        split.push((s, e));
    }

    let min_speech = (p.min_speech_ms / p.frame_ms) as usize;
    let pad = ms_to_samples(p.pad_ms);
    split
        .into_iter()
        .filter(|(s, e)| e - s >= min_speech)
        .map(|(s, e)| {
            (
                (s * frame).saturating_sub(pad),
                (e * frame + pad).min(x.len()),
            )
        })
        .collect()
}

/// VAD regions packed into decode windows of at most 28 s (Whisper's encoder always runs on
/// 30 s, so tiny windows waste compute); a silence gap > 5 s starts a new window.
pub fn pack_windows(samples: &[f32]) -> Vec<(usize, usize)> {
    let regions = split_on_silence(samples, &VadParams::default());
    let max = ms_to_samples(28_000);
    let max_gap = 5 * SR;
    let mut out: Vec<(usize, usize)> = vec![];
    for (a, b) in regions {
        match out.last_mut() {
            Some((s, e)) if b - *s <= max && a.saturating_sub(*e) <= max_gap => *e = b,
            _ => out.push((a, b)),
        }
    }
    out
}

/// True if any analysis frame looks like speech (cheap gate before running a decoder).
pub fn has_voice(x: &[f32], abs_threshold: f32) -> bool {
    x.chunks(ms_to_samples(30).max(1))
        .any(|f| rms(f) >= abs_threshold)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 220 Hz tone with a 4 Hz "syllable" envelope (speech-like energy variation).
    fn tone(ms: u64, amp: f32) -> Vec<f32> {
        let tau = 2.0 * std::f32::consts::PI;
        (0..ms_to_samples(ms))
            .map(|i| {
                let t = i as f32 / SR as f32;
                amp * (0.55 + 0.45 * (tau * 4.0 * t).sin()) * (tau * 220.0 * t).sin()
            })
            .collect()
    }

    #[test]
    fn resample_halves_length() {
        let x = vec![0.5f32; 32_000];
        assert_eq!(resample_linear(&x, 32_000, 16_000).len(), 16_000);
    }

    #[test]
    fn splits_two_utterances() {
        let mut x = tone(1000, 0.3);
        x.extend(vec![0.0; ms_to_samples(1000)]);
        x.extend(tone(800, 0.3));
        let r = split_on_silence(&x, &VadParams::default());
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(samples_to_ms(r[1].0) > 1500);
    }

    #[test]
    fn quiet_speech_found_flat_noise_ignored() {
        // -54 dBFS "speech" between digital silence (a real FLEURS file looks like this)
        let mut x = vec![0.0; ms_to_samples(500)];
        x.extend(tone(1500, 0.0025));
        x.extend(vec![0.0; ms_to_samples(500)]);
        assert_eq!(split_on_silence(&x, &VadParams::default()).len(), 1);
        // stationary low-level noise only
        let mut s = 12345u32;
        let noise: Vec<f32> = (0..ms_to_samples(3000))
            .map(|_| {
                s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                ((s >> 16) as f32 / 65_536.0 - 0.5) * 0.004
            })
            .collect();
        assert!(split_on_silence(&noise, &VadParams::default()).is_empty());
    }

    #[test]
    fn long_region_is_capped() {
        let x = tone(70_000, 0.3);
        let p = VadParams::default();
        let r = split_on_silence(&x, &p);
        assert!(r.len() >= 3);
        for (s, e) in r {
            assert!(samples_to_ms(e - s) <= p.max_region_ms + 2 * p.pad_ms + p.frame_ms);
        }
    }

    #[test]
    fn packs_regions_into_windows() {
        // three 4 s tones separated by 1 s silence -> one window
        let mut x = vec![];
        for _ in 0..3 {
            x.extend(tone(4000, 0.3));
            x.extend(vec![0.0; SR]);
        }
        assert_eq!(pack_windows(&x).len(), 1);
        // 10 s gap splits
        x.extend(vec![0.0; 10 * SR]);
        x.extend(tone(2000, 0.3));
        assert_eq!(pack_windows(&x).len(), 2);
    }

    #[test]
    fn silence_has_no_voice() {
        assert!(!has_voice(&vec![0.0; 16_000], 0.006));
        assert!(has_voice(&tone(100, 0.2), 0.006));
    }

    #[test]
    fn wav_roundtrip() {
        let dir = std::env::temp_dir().join(format!("almond-stt-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.wav");
        let x = tone(200, 0.5);
        write_wav_16k(&p, &x).unwrap();
        let y = load_wav_16k_mono(&p).unwrap();
        assert_eq!(x.len(), y.len());
        assert!((x[100] - y[100]).abs() < 1e-3);
        std::fs::remove_dir_all(&dir).ok();
    }
}
