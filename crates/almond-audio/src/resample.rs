//! Streaming mono resampler to the 16 kHz pipeline rate (rubato async sinc).

use almond_types::audio::PIPELINE_SAMPLE_RATE;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

/// Converts a continuous mono stream at `rate_in` to [`PIPELINE_SAMPLE_RATE`].
///
/// Input may arrive in any chunk size; output is appended to the caller's buffer. The
/// resampler's start-up delay is trimmed so output sample `n` corresponds to input time
/// `n / 16000` s (up to the sub-chunk still buffered inside).
pub struct StreamResampler {
    rate_in: u32,
    inner: Option<Inner>,
}

struct Inner {
    rs: Async<f32>,
    pending: Vec<f32>,
    out_buf: Vec<f32>,
    trim: usize,
}

impl StreamResampler {
    pub fn new(rate_in: u32) -> almond_types::Result<Self> {
        if rate_in == 0 {
            return Err(almond_types::Error::Invalid("sample rate 0".into()));
        }
        if rate_in == PIPELINE_SAMPLE_RATE {
            return Ok(Self {
                rate_in,
                inner: None,
            });
        }
        let ratio = PIPELINE_SAMPLE_RATE as f64 / rate_in as f64;
        // Speech-grade quality at modest CPU: 128-tap sinc, linear interpolation of the table.
        let params = SincInterpolationParameters::new(128, WindowFunction::Blackman2)
            .oversampling_factor(128)
            .interpolation(SincInterpolationType::Linear);
        let chunk = (rate_in as usize / 100).max(16); // 10 ms
        let rs = Async::<f32>::new_sinc(ratio, 1.1, &params, chunk, 1, FixedAsync::Input)
            .map_err(almond_types::Error::other)?;
        let out_buf = vec![0.0; rs.output_frames_max()];
        let trim = rs.output_delay();
        Ok(Self {
            rate_in,
            inner: Some(Inner {
                rs,
                pending: Vec::with_capacity(chunk * 4),
                out_buf,
                trim,
            }),
        })
    }

    pub fn rate_in(&self) -> u32 {
        self.rate_in
    }

    /// Feed input samples; resampled output is appended to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> almond_types::Result<()> {
        let Some(inner) = self.inner.as_mut() else {
            out.extend_from_slice(input);
            return Ok(());
        };
        inner.pending.extend_from_slice(input);
        let mut consumed = 0;
        loop {
            let need = inner.rs.input_frames_next();
            if inner.pending.len() - consumed < need {
                break;
            }
            let chunk = &inner.pending[consumed..consumed + need];
            let input =
                InterleavedSlice::new(chunk, 1, need).map_err(almond_types::Error::other)?;
            let cap = inner.out_buf.len();
            let mut output = InterleavedSlice::new_mut(&mut inner.out_buf, 1, cap)
                .map_err(almond_types::Error::other)?;
            let (n_in, n_out) = inner
                .rs
                .process_into_buffer(&input, &mut output, None)
                .map_err(almond_types::Error::other)?;
            consumed += n_in;
            let produced = &inner.out_buf[..n_out];
            let skip = inner.trim.min(produced.len());
            inner.trim -= skip;
            out.extend_from_slice(&produced[skip..]);
        }
        inner.pending.drain(..consumed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, secs: f32, hz: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin())
            .collect()
    }

    #[test]
    fn passthrough_at_16k() {
        let mut r = StreamResampler::new(16_000).unwrap();
        let mut out = vec![];
        r.process(&[0.1, 0.2, 0.3], &mut out).unwrap();
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn length_and_energy_preserved() {
        for rate in [44_100u32, 48_000, 8_000] {
            let input = tone(rate, 1.0, 440.0);
            let mut r = StreamResampler::new(rate).unwrap();
            let mut out = vec![];
            for c in input.chunks(333) {
                r.process(c, &mut out).unwrap();
            }
            // Within one 10 ms chunk + delay of the ideal length.
            let ideal = 16_000usize;
            assert!(
                out.len() <= ideal && out.len() > ideal - 500,
                "{rate}: {}",
                out.len()
            );
            let mid = &out[2000..out.len() - 2000];
            let rms = (mid.iter().map(|x| x * x).sum::<f32>() / mid.len() as f32).sqrt();
            assert!((rms - 0.5 / 2f32.sqrt()).abs() < 0.02, "{rate}: rms {rms}");
        }
    }

    #[test]
    fn phase_aligned_after_trim() {
        // An impulse at 0.5 s should come out near sample 8000.
        let rate = 48_000;
        let mut input = vec![0.0f32; rate as usize];
        input[rate as usize / 2] = 1.0;
        let mut r = StreamResampler::new(rate).unwrap();
        let mut out = vec![];
        r.process(&input, &mut out).unwrap();
        let (idx, _) = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap();
        assert!((idx as i64 - 8000).abs() <= 2, "peak at {idx}");
    }
}
