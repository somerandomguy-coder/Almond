//! Engine-agnostic pseudo-streaming for offline ASR models (Whisper, offline Zipformer).
//!
//! Neither whisper.cpp nor the Vietnamese Zipformer is natively streaming, so live captions use
//! the *LocalAgreement-2* policy from whisper-streaming (Macháček et al., 2023):
//!
//! 1. Audio accumulates in a buffer that starts at the last commit point.
//! 2. Every `step_ms` the **whole buffer** is re-decoded (so later context can fix earlier words).
//! 3. Words on which two consecutive hypotheses agree (longest common prefix) are **committed**;
//!    the unconfirmed tail is shown as a partial (`is_final = false`).
//! 4. A segment is finalized (`is_final = true`) at sentence punctuation, at a pause, when it
//!    gets too long, or when the buffer hits `max_buffer_ms`; the buffer is then trimmed to the
//!    commit point, so the next decode overlaps only the unconfirmed audio.
//! 5. An energy gate skips decoding silence (Whisper hallucinates on silence — e.g. the classic
//!    "Hẹn gặp lại các bạn trong những video tiếp theo").
//!
//! Update protocol for the UI: updates carry `segment.idx`; every partial for idx N replaces the
//! previous partial for N; exactly one final is emitted per idx, after which N+1 starts.

use crate::audio::{SR, has_voice, ms_to_samples, samples_to_ms};
use almond_types::artifact::body::{Segment, Word};
use almond_types::audio::AudioFrame;
use almond_types::meeting::AudioChannel;
use almond_types::stt::{SttStream, SttUpdate};
use almond_types::{Error, LangTag, Result};
use async_trait::async_trait;

/// A recognized word with times **relative to the start of the decoded buffer**.
#[derive(Debug, Clone, PartialEq)]
pub struct HypWord {
    pub t0_ms: u64,
    pub t1_ms: u64,
    /// Word text **including** its leading space if the engine produced one (so Chinese, which
    /// has no spaces, round-trips: text = concat(words).trim()).
    pub text: String,
    pub p: Option<f32>,
}

/// Output of one decode call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChunkResult {
    pub words: Vec<HypWord>,
    pub lang: Option<LangTag>,
}

/// An offline recognizer that can decode a short buffer (<= ~30 s) into timed words.
pub trait ChunkDecoder: Send + 'static {
    /// `prompt`: previous committed text (context) + user vocabulary, may be ignored.
    fn decode(&mut self, samples: &[f32], prompt: Option<&str>) -> Result<ChunkResult>;
}

/// Tuning knobs.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Re-decode cadence.
    pub step_ms: u64,
    /// Don't decode buffers shorter than this.
    pub min_decode_ms: u64,
    /// Hard cap on buffered audio; on overflow everything hypothesized is committed + finalized.
    pub max_buffer_ms: u64,
    /// Finalize a segment after this much committed speech.
    pub max_segment_ms: u64,
    /// A gap this long between committed words and the next word finalizes the segment.
    pub pause_ms: u64,
    /// RMS gate for "is there any voice in the buffer".
    pub vad_threshold: f32,
    /// Static prompt (vocabulary) prepended to the rolling context.
    pub prompt: Option<String>,
}

impl Default for StreamConfig {
    fn default() -> Self {
        StreamConfig {
            step_ms: 1000,
            min_decode_ms: 1000,
            max_buffer_ms: 20_000,
            // Keep segments short so finals land quickly (measured: 15 s / 800 ms gave a p50 final
            // latency of 7.9 s on conversational audio with Zipformer).
            max_segment_ms: 8_000,
            pause_ms: 500,
            vad_threshold: 0.001,
            prompt: None,
        }
    }
}

/// Normalized comparison key for agreement (case/punctuation-insensitive).
fn key(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn ends_sentence(w: &str) -> bool {
    w.trim_end()
        .ends_with(['.', '?', '!', '…', '。', '？', '！'])
}

/// Absolute-time word.
#[derive(Debug, Clone)]
struct AbsWord {
    t0: u64,
    t1: u64,
    text: String,
    p: Option<f32>,
}

/// The synchronous LocalAgreement state machine. Wrapped by [`PseudoStream`] for async use.
pub struct LocalAgreement<D: ChunkDecoder> {
    decoder: D,
    cfg: StreamConfig,
    channel: AudioChannel,
    buf: Vec<f32>,
    /// Absolute ms of buf[0]; None until the first frame.
    buf_t0: Option<u64>,
    since_decode: usize,
    committed_upto: u64,
    prev_hyp: Vec<AbsWord>,
    seg_words: Vec<AbsWord>,
    seg_idx: u32,
    lang: Option<LangTag>,
    /// Tail of finalized text, used as decoding context.
    context: String,
}

impl<D: ChunkDecoder> LocalAgreement<D> {
    pub fn new(decoder: D, cfg: StreamConfig) -> Self {
        LocalAgreement {
            decoder,
            cfg,
            channel: AudioChannel::Mixed,
            buf: vec![],
            buf_t0: None,
            since_decode: 0,
            committed_upto: 0,
            prev_hyp: vec![],
            seg_words: vec![],
            seg_idx: 0,
            lang: None,
            context: String::new(),
        }
    }

    pub fn decoder_mut(&mut self) -> &mut D {
        &mut self.decoder
    }

    /// Feed one frame; returns updates (possibly none).
    pub fn push(&mut self, frame: AudioFrame) -> Result<Vec<SttUpdate>> {
        self.channel = frame.channel;
        if self.buf_t0.is_none() {
            self.buf_t0 = Some(frame.t0_ms);
            self.committed_upto = frame.t0_ms;
        }
        self.since_decode += frame.samples.len();
        self.buf.extend_from_slice(&frame.samples);
        if samples_to_ms(self.since_decode) < self.cfg.step_ms
            || samples_to_ms(self.buf.len()) < self.cfg.min_decode_ms
        {
            return Ok(vec![]);
        }
        self.process(false)
    }

    /// Flush: decode what is left, commit everything, finalize.
    pub fn finish(&mut self) -> Result<Vec<SttUpdate>> {
        if self.buf.is_empty() && self.seg_words.is_empty() {
            return Ok(vec![]);
        }
        self.process(true)
    }

    fn buf_end(&self) -> u64 {
        self.buf_t0.unwrap_or(0) + samples_to_ms(self.buf.len())
    }

    fn prompt(&self) -> Option<String> {
        let ctx: String = {
            let chars: Vec<char> = self.context.chars().collect();
            let start = chars.len().saturating_sub(200);
            chars[start..].iter().collect()
        };
        match (&self.cfg.prompt, ctx.trim().is_empty()) {
            (Some(p), true) => Some(p.clone()),
            (Some(p), false) => Some(format!("{p} {}", ctx.trim())),
            (None, false) => Some(ctx.trim().to_string()),
            (None, true) => None,
        }
    }

    fn process(&mut self, flush: bool) -> Result<Vec<SttUpdate>> {
        self.since_decode = 0;
        let mut out = vec![];
        let t0 = self.buf_t0.unwrap_or(0);

        if !has_voice(&self.buf, self.cfg.vad_threshold) {
            // Silence: close any open segment, drop audio (keep a little for onsets).
            self.commit_all_tentative();
            if !self.seg_words.is_empty() {
                out.push(self.finalize());
            }
            let keep = if flush { 0 } else { ms_to_samples(300) };
            let end = self.buf_end();
            self.trim_to(end.saturating_sub(samples_to_ms(keep)));
            self.committed_upto = self.committed_upto.max(self.buf_t0.unwrap_or(end));
            return Ok(out);
        }

        let prompt = self.prompt();
        let res = self.decoder.decode(&self.buf, prompt.as_deref())?;
        if res.lang.is_some() {
            self.lang = res.lang.clone();
        }
        let hyp: Vec<AbsWord> = res
            .words
            .into_iter()
            .map(|w| AbsWord {
                t0: t0 + w.t0_ms,
                t1: t0 + w.t1_ms,
                text: w.text,
                p: w.p,
            })
            // words fully before the commit point were already emitted
            .filter(|w| w.t1 > self.committed_upto + 40 || w.t0 >= self.committed_upto)
            .filter(|w| !key(&w.text).is_empty() || !w.text.trim().is_empty())
            .collect();

        let n_agree = if flush {
            hyp.len()
        } else {
            self.prev_hyp
                .iter()
                .zip(&hyp)
                .take_while(|(a, b)| key(&a.text) == key(&b.text))
                .count()
        };
        let overflow = samples_to_ms(self.buf.len()) >= self.cfg.max_buffer_ms;
        let n_commit = if overflow { hyp.len() } else { n_agree };

        // Commit agreed words, finalizing at sentence ends / pauses / max length.
        let mut tentative = hyp;
        let committed: Vec<AbsWord> = tentative.drain(..n_commit).collect();
        for w in committed {
            if let Some(last) = self.seg_words.last()
                && w.t0.saturating_sub(last.t1) >= self.cfg.pause_ms
            {
                out.push(self.finalize());
            }
            self.committed_upto = self.committed_upto.max(w.t1);
            let end_here = ends_sentence(&w.text);
            self.seg_words.push(w);
            let seg_len = self.seg_len();
            if (end_here && seg_len >= 1000) || seg_len >= self.cfg.max_segment_ms {
                out.push(self.finalize());
            }
        }
        // A long pause before the next (tentative) word also closes the segment.
        if let (Some(last), Some(next)) = (self.seg_words.last(), tentative.first())
            && next.t0.saturating_sub(last.t1) >= self.cfg.pause_ms
        {
            out.push(self.finalize());
        }
        if flush || overflow {
            if !self.seg_words.is_empty() {
                out.push(self.finalize());
            }
            tentative.clear();
        }
        self.prev_hyp = tentative;

        // Trim the buffer: everything before the start of the open segment (or the commit point
        // if no segment is open) has been emitted as final.
        let trim_at = if flush || overflow {
            self.buf_end()
        } else if let Some(first) = self.seg_words.first() {
            first.t0.min(self.committed_upto)
        } else {
            self.committed_upto
        };
        // Never trim into audio we still need for tentative words.
        let trim_at = self
            .prev_hyp
            .first()
            .map(|w| trim_at.min(w.t0))
            .unwrap_or(trim_at);
        self.trim_to(trim_at);

        if !self.seg_words.is_empty() || !self.prev_hyp.is_empty() {
            out.push(self.partial());
        }
        Ok(out)
    }

    fn commit_all_tentative(&mut self) {
        let t = std::mem::take(&mut self.prev_hyp);
        for w in t {
            self.committed_upto = self.committed_upto.max(w.t1);
            self.seg_words.push(w);
        }
    }

    fn seg_len(&self) -> u64 {
        match (self.seg_words.first(), self.seg_words.last()) {
            (Some(a), Some(b)) => b.t1.saturating_sub(a.t0),
            _ => 0,
        }
    }

    fn trim_to(&mut self, abs_ms: u64) {
        let Some(t0) = self.buf_t0 else { return };
        if abs_ms <= t0 {
            return;
        }
        let n = ms_to_samples(abs_ms - t0).min(self.buf.len());
        self.buf.drain(..n);
        self.buf_t0 = Some(t0 + samples_to_ms(n));
    }

    fn make_segment(&self, words: &[AbsWord]) -> Segment {
        let text: String = words.iter().map(|w| w.text.as_str()).collect();
        let ps: Vec<f32> = words.iter().filter_map(|w| w.p).collect();
        Segment {
            idx: self.seg_idx,
            t0_ms: words.first().map(|w| w.t0).unwrap_or(0) as u32,
            t1_ms: words.last().map(|w| w.t1).unwrap_or(0) as u32,
            channel: self.channel,
            speaker: None,
            lang: self.lang.clone(),
            text: text.trim().to_string(),
            confidence: (!ps.is_empty()).then(|| ps.iter().sum::<f32>() / ps.len() as f32),
            words: Some(
                words
                    .iter()
                    .map(|w| Word {
                        t0_ms: w.t0 as u32,
                        t1_ms: w.t1 as u32,
                        text: w.text.trim().to_string(),
                        p: w.p,
                    })
                    .collect(),
            ),
        }
    }

    fn finalize(&mut self) -> SttUpdate {
        let words = std::mem::take(&mut self.seg_words);
        let seg = self.make_segment(&words);
        self.context.push(' ');
        self.context.push_str(&seg.text);
        if self.context.len() > 2000 {
            let cut = self.context.len() - 1000;
            let cut = (cut..self.context.len())
                .find(|i| self.context.is_char_boundary(*i))
                .unwrap_or(0);
            self.context.drain(..cut);
        }
        self.seg_idx += 1;
        SttUpdate {
            segment: seg,
            is_final: true,
        }
    }

    fn partial(&self) -> SttUpdate {
        let mut words = self.seg_words.clone();
        words.extend(self.prev_hyp.iter().cloned());
        SttUpdate {
            segment: self.make_segment(&words),
            is_final: false,
        }
    }
}

/// Async [`SttStream`] adapter that runs decoding on the blocking thread pool.
pub struct PseudoStream<D: ChunkDecoder> {
    inner: Option<LocalAgreement<D>>,
}

impl<D: ChunkDecoder> PseudoStream<D> {
    pub fn new(decoder: D, cfg: StreamConfig) -> Self {
        PseudoStream {
            inner: Some(LocalAgreement::new(decoder, cfg)),
        }
    }

    async fn run<F>(&mut self, f: F) -> Result<Vec<SttUpdate>>
    where
        F: FnOnce(&mut LocalAgreement<D>) -> Result<Vec<SttUpdate>> + Send + 'static,
    {
        let mut la = self
            .inner
            .take()
            .ok_or_else(|| Error::Other("stream poisoned by an earlier panic".into()))?;
        let (la, r) = tokio::task::spawn_blocking(move || {
            let r = f(&mut la);
            (la, r)
        })
        .await
        .map_err(Error::other)?;
        self.inner = Some(la);
        r
    }
}

#[async_trait]
impl<D: ChunkDecoder> SttStream for PseudoStream<D> {
    async fn push(&mut self, frame: AudioFrame) -> Result<Vec<SttUpdate>> {
        self.run(move |la| la.push(frame)).await
    }
    async fn finish(&mut self) -> Result<Vec<SttUpdate>> {
        self.run(|la| la.finish()).await
    }
}

/// Split samples into `frame_ms` frames (helper for tests, examples and file replay).
pub fn frames(samples: &[f32], frame_ms: u64, channel: AudioChannel) -> Vec<AudioFrame> {
    let n = ms_to_samples(frame_ms).max(1);
    samples
        .chunks(n)
        .enumerate()
        .map(|(i, c)| AudioFrame {
            channel,
            t0_ms: (i * n * 1000 / SR) as u64,
            samples: c.to_vec(),
            synthesized: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake recognizer: a fixed script where word i occupies [i*400, i*400+300) ms (absolute,
    /// relative to stream start). It "hears" a word once the buffer covers it fully; the last
    /// heard word is garbled on the first decode that sees it (unstable tail), like Whisper.
    struct Script {
        words: Vec<&'static str>,
        /// absolute start of the decoder's current buffer, tracked by the test via buffer length
        total_ms: u64,
        seen_last: Option<usize>,
        calls: usize,
    }

    impl ChunkDecoder for Script {
        fn decode(&mut self, samples: &[f32], _: Option<&str>) -> Result<ChunkResult> {
            self.calls += 1;
            let buf_ms = samples_to_ms(samples.len());
            let buf_t0 = self.total_ms - buf_ms;
            let mut words = vec![];
            let mut last_idx = None;
            for (i, w) in self.words.iter().enumerate() {
                let (a, b) = (i as u64 * 400, i as u64 * 400 + 300);
                if a >= buf_t0 && b <= self.total_ms {
                    words.push(HypWord {
                        t0_ms: a - buf_t0,
                        t1_ms: b - buf_t0,
                        text: format!(" {w}"),
                        p: Some(0.9),
                    });
                    last_idx = Some(i);
                }
            }
            if let Some(li) = last_idx
                && self.seen_last != Some(li)
            {
                // first time we see this tail word: garble it
                words.last_mut().unwrap().text = " ???".into();
                self.seen_last = Some(li);
            }
            Ok(ChunkResult {
                words,
                lang: Some(LangTag::vi()),
            })
        }
    }

    fn loud(ms: u64) -> Vec<f32> {
        (0..ms_to_samples(ms))
            .map(|i| 0.2 * ((i as f32) * 0.07).sin())
            .collect()
    }

    fn run_script(words: Vec<&'static str>, total_ms: u64) -> Vec<SttUpdate> {
        let mut la = LocalAgreement::new(
            Script {
                words,
                total_ms: 0,
                seen_last: None,
                calls: 0,
            },
            StreamConfig {
                step_ms: 500,
                min_decode_ms: 500,
                ..Default::default()
            },
        );
        let audio = loud(total_ms);
        let mut ups = vec![];
        for f in frames(&audio, 100, AudioChannel::Mic) {
            la.decoder_mut().total_ms = f.t0_ms + samples_to_ms(f.samples.len());
            ups.extend(la.push(f).unwrap());
        }
        ups.extend(la.finish().unwrap());
        ups
    }

    #[test]
    fn finals_reconstruct_script_and_partials_precede_finals() {
        let script = vec![
            "xin", "chào", "các", "bạn.", "hôm", "nay", "chúng", "ta", "họp.",
        ];
        let ups = run_script(script.clone(), 9 * 400 + 200);
        let finals: Vec<_> = ups.iter().filter(|u| u.is_final).collect();
        let text = finals
            .iter()
            .map(|u| u.segment.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(text, script.join(" "));
        // sentence punctuation splits into two finals
        assert_eq!(finals.len(), 2, "{finals:#?}");
        assert_eq!(finals[0].segment.idx, 0);
        assert_eq!(finals[1].segment.idx, 1);
        assert!(finals[0].segment.t0_ms < finals[1].segment.t0_ms);
        // partials were emitted, and no final contains the garbled token
        assert!(ups.iter().any(|u| !u.is_final));
        assert!(finals.iter().all(|u| !u.segment.text.contains("???")));
        // idx monotonic: once N is final, no further updates for N
        for (i, u) in ups.iter().enumerate() {
            if u.is_final {
                assert!(ups[i + 1..].iter().all(|v| v.segment.idx > u.segment.idx));
            }
        }
        // words carry absolute times
        let w = finals[1].segment.words.as_ref().unwrap();
        assert_eq!(w[0].t0_ms, 4 * 400);
        assert_eq!(finals[0].segment.channel, AudioChannel::Mic);
    }

    #[test]
    fn silence_produces_nothing_and_skips_decoder() {
        let mut la = LocalAgreement::new(
            Script {
                words: vec!["x"],
                total_ms: 0,
                seen_last: None,
                calls: 0,
            },
            StreamConfig::default(),
        );
        let silence = vec![0.0f32; ms_to_samples(5000)];
        let mut ups = vec![];
        for f in frames(&silence, 100, AudioChannel::System) {
            ups.extend(la.push(f).unwrap());
        }
        ups.extend(la.finish().unwrap());
        assert!(ups.is_empty());
        assert_eq!(la.decoder_mut().calls, 0);
        // buffer was trimmed, not accumulated
        assert!(samples_to_ms(la.buf.len()) <= 1000);
    }

    #[tokio::test]
    async fn async_adapter_works() {
        let mut s = PseudoStream::new(
            Script {
                words: vec!["a", "b", "c"],
                total_ms: 0,
                seen_last: None,
                calls: 0,
            },
            StreamConfig::default(),
        );
        // The fake decoder needs total_ms; drive it through finish with full audio at once.
        if let Some(la) = s.inner.as_mut() {
            la.decoder_mut().total_ms = 1300;
        }
        let ups = s
            .push(AudioFrame {
                channel: AudioChannel::Mic,
                t0_ms: 0,
                samples: loud(1300),
                synthesized: false,
            })
            .await
            .unwrap();
        assert!(ups.iter().all(|u| !u.is_final));
        let fin = s.finish().await.unwrap();
        assert_eq!(fin.iter().filter(|u| u.is_final).count(), 1);
        assert_eq!(
            fin.iter().find(|u| u.is_final).unwrap().segment.text,
            "a b c"
        );
    }
}
