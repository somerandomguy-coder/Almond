//! Voice activity detection on 16 kHz pipeline frames.
//!
//! * [`EnergyVad`] — pure Rust, adaptive noise floor + hangover. Always available, no models.
//! * [`WebRtcVad`] — Google's WebRTC GMM VAD (feature `webrtc-vad`, C compiled with `cc`).
//!
//! Silero (ONNX) is deliberately not bundled here: it needs `ort` + native onnxruntime, which WS-B
//! already pulls in via sherpa-onnx; implement [`Vad`] there if a neural VAD is wanted.
//!
//! VAD is advisory (skip STT on silence, speech/silence UI); capture itself never drops audio.

use almond_types::audio::PIPELINE_SAMPLE_RATE;

/// Window size used by the VADs: 30 ms at 16 kHz.
pub const VAD_WINDOW: usize = (PIPELINE_SAMPLE_RATE as usize * 30) / 1000;

pub trait Vad: Send {
    /// Classify one 30 ms window ([`VAD_WINDOW`] samples, 16 kHz).
    fn is_speech_window(&mut self, window: &[f32]) -> bool;

    /// Fraction (0..=1) of 30 ms windows in `frame` classified as speech.
    fn speech_ratio(&mut self, frame: &[f32]) -> f32 {
        let mut n = 0;
        let mut s = 0;
        for w in frame.as_chunks::<VAD_WINDOW>().0 {
            n += 1;
            if self.is_speech_window(w) {
                s += 1;
            }
        }
        if n == 0 { 0.0 } else { s as f32 / n as f32 }
    }
}

/// Energy VAD with an adaptive noise floor.
#[derive(Debug, Clone)]
pub struct EnergyVad {
    noise_floor_db: f32,
    /// Speech if window energy exceeds the floor by this many dB.
    pub threshold_db: f32,
    /// Absolute floor below which nothing is speech (digital silence / zero-fill).
    pub min_db: f32,
    hangover: u32,
    hang_left: u32,
}

impl Default for EnergyVad {
    fn default() -> Self {
        Self {
            noise_floor_db: -60.0,
            threshold_db: 9.0,
            min_db: -55.0,
            hangover: 8, // 240 ms
            hang_left: 0,
        }
    }
}

impl Vad for EnergyVad {
    fn is_speech_window(&mut self, window: &[f32]) -> bool {
        let db = crate::level::rms_dbfs(window);
        let active = db > self.min_db && db > self.noise_floor_db + self.threshold_db;
        // Track the floor: fall fast, rise slowly (only on non-speech). Digital silence
        // (zero-filled gaps) says nothing about the room's noise floor.
        if db > -100.0 {
            if db < self.noise_floor_db {
                self.noise_floor_db = 0.7 * self.noise_floor_db + 0.3 * db;
            } else if !active {
                self.noise_floor_db = 0.98 * self.noise_floor_db + 0.02 * db;
            }
        }
        self.noise_floor_db = self.noise_floor_db.max(-90.0);
        if active {
            self.hang_left = self.hangover;
            true
        } else if self.hang_left > 0 {
            self.hang_left -= 1;
            true
        } else {
            false
        }
    }
}

/// WebRTC VAD wrapper (16 kHz, 30 ms windows).
#[cfg(feature = "webrtc-vad")]
pub struct WebRtcVad {
    inner: webrtc_vad::Vad,
    buf: Vec<i16>,
}

#[cfg(feature = "webrtc-vad")]
// SAFETY: `webrtc_vad::Vad` holds a heap pointer to C state that is only touched through
// `&mut self`; it has no thread-local or thread-affine state, so transferring ownership is sound.
unsafe impl Send for WebRtcVad {}

#[cfg(feature = "webrtc-vad")]
impl WebRtcVad {
    /// `aggressiveness` 0 (quality) ..= 3 (very aggressive).
    pub fn new(aggressiveness: u8) -> Self {
        use webrtc_vad::{SampleRate, VadMode};
        let mode = match aggressiveness {
            0 => VadMode::Quality,
            1 => VadMode::LowBitrate,
            2 => VadMode::Aggressive,
            _ => VadMode::VeryAggressive,
        };
        Self {
            inner: webrtc_vad::Vad::new_with_rate_and_mode(SampleRate::Rate16kHz, mode),
            buf: Vec::with_capacity(VAD_WINDOW),
        }
    }
}

#[cfg(feature = "webrtc-vad")]
impl Vad for WebRtcVad {
    fn is_speech_window(&mut self, window: &[f32]) -> bool {
        self.buf.clear();
        self.buf
            .extend(window.iter().map(|x| (x.clamp(-1.0, 1.0) * 32767.0) as i16));
        self.inner.is_voice_segment(&self.buf).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, amp: f32, seed: &mut u32) -> Vec<f32> {
        (0..n)
            .map(|_| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((*seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 2.0 * amp
            })
            .collect()
    }

    fn voiced(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f32 / 16_000.0;
                0.3 * (2.0 * std::f32::consts::PI * 150.0 * t).sin()
                    + 0.15 * (2.0 * std::f32::consts::PI * 450.0 * t).sin()
            })
            .collect()
    }

    #[test]
    fn energy_vad_separates_speechlike_from_noise() {
        let mut v = EnergyVad::default();
        let mut seed = 1;
        assert_eq!(v.speech_ratio(&vec![0.0; 16_000]), 0.0);
        let r_noise = v.speech_ratio(&noise(32_000, 0.003, &mut seed));
        assert!(r_noise < 0.1, "noise {r_noise}");
        let r_voice = v.speech_ratio(&voiced(16_000));
        assert!(r_voice > 0.9, "voice {r_voice}");
    }

    #[cfg(feature = "webrtc-vad")]
    #[test]
    fn webrtc_vad_runs() {
        let mut v = WebRtcVad::new(2);
        assert_eq!(v.speech_ratio(&vec![0.0; 16_000]), 0.0);
        let _ = v.speech_ratio(&voiced(16_000));
    }
}
