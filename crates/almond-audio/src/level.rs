//! Level metering helpers (UI meters, "is the mic dead?" checks, the `record` example).

/// RMS level in dBFS (-inf for digital silence is clamped to -120).
pub fn rms_dbfs(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return -120.0;
    }
    let ms = samples
        .iter()
        .map(|x| (*x as f64) * (*x as f64))
        .sum::<f64>()
        / samples.len() as f64;
    to_db(ms.sqrt() as f32)
}

/// Peak level in dBFS.
pub fn peak_dbfs(samples: &[f32]) -> f32 {
    to_db(samples.iter().fold(0.0f32, |m, x| m.max(x.abs())))
}

fn to_db(v: f32) -> f32 {
    if v <= 1e-6 { -120.0 } else { 20.0 * v.log10() }
}

/// Running level accumulator.
#[derive(Debug, Clone, Default)]
pub struct LevelMeter {
    sum_sq: f64,
    n: u64,
    peak: f32,
    /// Frames whose RMS exceeded -50 dBFS.
    pub active_frames: u64,
    pub frames: u64,
}

impl LevelMeter {
    pub fn add(&mut self, frame: &[f32]) {
        for x in frame {
            self.sum_sq += (*x as f64) * (*x as f64);
            self.peak = self.peak.max(x.abs());
        }
        self.n += frame.len() as u64;
        self.frames += 1;
        if rms_dbfs(frame) > -50.0 {
            self.active_frames += 1;
        }
    }
    pub fn rms_dbfs(&self) -> f32 {
        if self.n == 0 {
            return -120.0;
        }
        to_db((self.sum_sq / self.n as f64).sqrt() as f32)
    }
    pub fn peak_dbfs(&self) -> f32 {
        to_db(self.peak)
    }
    pub fn samples(&self) -> u64 {
        self.n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_square_is_0db() {
        let s: Vec<f32> = (0..100)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        assert!(rms_dbfs(&s).abs() < 1e-3);
        assert!(peak_dbfs(&s).abs() < 1e-3);
        assert_eq!(rms_dbfs(&[0.0; 10]), -120.0);
    }
}
