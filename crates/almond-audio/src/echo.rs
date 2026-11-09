//! Speaker-bleed ("echo") detection between the `System` and `Mic` channels.
//!
//! When the user is not on headphones, the mic re-captures the remote side from the speakers
//! (measured on the dev PC: ~117 ms later, ~-25 dB). Full acoustic echo cancellation (WebRTC
//! AEC3) needs `webrtc-audio-processing`, whose build requires meson/ninja/clang — not available
//! on the target machines yet, so it is left as a follow-up. This module provides the cheap,
//! dependency-free part that downstream consumers need today:
//!
//! * [`EchoDetector::estimate`] — is there bleed at all (speakers vs headphones), with what bulk
//!   delay and coupling, from 10 ms log-energy envelopes (normalized cross-correlation);
//! * [`EchoDetector::mic_is_bleed`] — whether a mic window is explained by delayed system audio
//!   (so STT/diarization can avoid attributing the remote speaker to "Me").
//!
//! Frames are indexed by `t0_ms`, so the channels may arrive in any interleaving.

use crate::level::rms_dbfs;
use almond_types::audio::AudioFrame;
use almond_types::meeting::AudioChannel;

const WIN_MS: u64 = 10;
const WIN: usize = 160;
const FLOOR_DB: f32 = -80.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EchoEstimate {
    /// Mic lags system by this much.
    pub delay_ms: u32,
    /// Normalized cross-correlation of the envelopes at `delay_ms` (0..1).
    pub correlation: f32,
    /// Typical mic level minus system level while the system is active (dB, negative).
    pub coupling_db: f32,
}

#[derive(Debug, Clone)]
pub struct EchoDetector {
    sys: Vec<f32>,
    mic: Vec<f32>,
    /// Max bulk delay searched.
    pub max_delay_ms: u32,
    /// Analysis window (most recent audio present in both channels).
    pub window_ms: u32,
    /// Minimum correlation to report bleed.
    pub min_correlation: f32,
}

impl Default for EchoDetector {
    fn default() -> Self {
        Self {
            sys: vec![],
            mic: vec![],
            max_delay_ms: 500,
            window_ms: 20_000,
            min_correlation: 0.5,
        }
    }
}

impl EchoDetector {
    pub fn push(&mut self, frame: &AudioFrame) {
        let env = match frame.channel {
            AudioChannel::System => &mut self.sys,
            AudioChannel::Mic => &mut self.mic,
            AudioChannel::Mixed => return,
        };
        let start = (frame.t0_ms / WIN_MS) as usize;
        for (k, w) in frame.samples.chunks(WIN).enumerate() {
            let i = start + k;
            if env.len() <= i {
                env.resize(i + 1, FLOOR_DB);
            }
            env[i] = rms_dbfs(w).max(FLOOR_DB);
        }
    }

    /// Estimate bleed from the most recent `window_ms` of audio present in both channels.
    pub fn estimate(&self) -> Option<EchoEstimate> {
        let end = self.sys.len().min(self.mic.len());
        let max_lag = (self.max_delay_ms as u64 / WIN_MS) as usize;
        let win = (self.window_ms as u64 / WIN_MS) as usize;
        if end < max_lag + 100 {
            return None;
        }
        let start = end.saturating_sub(win).max(max_lag);
        let mic = &self.mic[start..end];
        let (m_mean, m_var) = mean_var(mic);
        if m_var < 1.0 {
            return None;
        }
        let mut best: Option<(usize, f32)> = None;
        for lag in 0..=max_lag {
            let sys = &self.sys[start - lag..end - lag];
            let (s_mean, s_var) = mean_var(sys);
            if s_var < 1.0 {
                continue;
            }
            let cov = sys
                .iter()
                .zip(mic)
                .map(|(s, m)| (s - s_mean) * (m - m_mean))
                .sum::<f32>()
                / mic.len() as f32;
            let r = cov / (s_var.sqrt() * m_var.sqrt());
            if best.is_none_or(|(_, b)| r > b) {
                best = Some((lag, r));
            }
        }
        let (lag, r) = best?;
        if r < self.min_correlation {
            return None;
        }
        let sys = &self.sys[start - lag..end - lag];
        let diffs: Vec<f32> = sys
            .iter()
            .zip(mic)
            .filter(|(s, _)| **s > -50.0)
            .map(|(s, m)| m - s)
            .collect();
        if diffs.is_empty() {
            return None;
        }
        let coupling_db = median(diffs);
        Some(EchoEstimate {
            delay_ms: (lag as u64 * WIN_MS) as u32,
            correlation: r,
            coupling_db,
        })
    }

    /// Is the 10 ms mic window starting at `t_ms` explained by system audio leaking through the
    /// speakers (mic level within `margin_db` of the predicted echo level)? `false` when no
    /// bleed is detected or data is missing.
    pub fn mic_is_bleed(&self, est: &EchoEstimate, t_ms: u64, margin_db: f32) -> bool {
        let i = (t_ms / WIN_MS) as usize;
        let lag = (est.delay_ms as u64 / WIN_MS) as usize;
        let (Some(&m), Some(&s)) = (
            self.mic.get(i),
            i.checked_sub(lag).and_then(|j| self.sys.get(j)),
        ) else {
            return false;
        };
        s > -50.0 && m <= s + est.coupling_db + margin_db
    }
}

fn mean_var(x: &[f32]) -> (f32, f32) {
    let n = x.len().max(1) as f32;
    let mean = x.iter().sum::<f32>() / n;
    let var = x.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
    (mean, var)
}

fn median(mut v: Vec<f32>) -> f32 {
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(ch: AudioChannel, sig: &[f32]) -> Vec<AudioFrame> {
        sig.chunks(1600)
            .enumerate()
            .map(|(k, c)| AudioFrame {
                channel: ch,
                t0_ms: k as u64 * 100,
                samples: c.to_vec(),
            })
            .collect()
    }

    fn rand(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
    }

    #[test]
    fn finds_speaker_bleed_delay() {
        let mut seed = 7;
        let n = 16_000 * 15;
        // "Speech": noise bursts with random on/off envelope.
        let mut sys = vec![0.0f32; n];
        let mut on = false;
        for (i, s) in sys.iter_mut().enumerate() {
            if i % 1600 == 0 {
                on = rand(&mut seed) > 0.0;
            }
            if on {
                *s = rand(&mut seed) * 0.5;
            }
        }
        let delay = 16 * 120; // 120 ms
        let mic: Vec<f32> = (0..n)
            .map(|i| {
                let echo = if i >= delay {
                    sys[i - delay] * 0.05
                } else {
                    0.0
                };
                echo + rand(&mut seed) * 0.0005
            })
            .collect();
        let mut d = EchoDetector::default();
        for f in frames(AudioChannel::System, &sys)
            .iter()
            .chain(&frames(AudioChannel::Mic, &mic))
        {
            d.push(f);
        }
        let est = d.estimate().expect("bleed detected");
        assert!((est.delay_ms as i32 - 120).abs() <= 10, "{est:?}");
        assert!((est.coupling_db + 26.0).abs() < 3.0, "{est:?}");
        // Find a system-active window and check the bleed flag.
        let t = (0..14_000u64)
            .step_by(10)
            .find(|t| {
                let i = (*t as usize) * 16;
                sys[i..i + 160].iter().any(|x| x.abs() > 0.1)
            })
            .unwrap();
        assert!(d.mic_is_bleed(&est, t + 120, 6.0));
    }

    #[test]
    fn headphones_no_bleed() {
        let mut seed = 3;
        let n = 16_000 * 10;
        let sys: Vec<f32> = (0..n)
            .map(|i| {
                if (i / 4000) % 2 == 0 {
                    rand(&mut seed)
                } else {
                    0.0
                }
            })
            .collect();
        let mic: Vec<f32> = (0..n)
            .map(|i| {
                if (i / 7000) % 2 == 1 {
                    rand(&mut seed)
                } else {
                    0.0
                }
            })
            .collect();
        let mut d = EchoDetector::default();
        for f in frames(AudioChannel::System, &sys)
            .iter()
            .chain(&frames(AudioChannel::Mic, &mic))
        {
            d.push(f);
        }
        assert_eq!(d.estimate(), None);
    }
}
