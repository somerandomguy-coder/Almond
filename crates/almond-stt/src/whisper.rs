//! whisper.cpp engine (via `whisper-rs`): PhoWhisper (vi), whisper-large-v3-turbo (en/zh/...).
//!
//! * Offline [`SttEngine::transcribe`]: energy-VAD splits the audio into utterances, packs them
//!   into <= 28 s windows (Whisper's encoder always runs on 30 s, so tiny windows waste compute),
//!   identifies the language per window (restricted to `allowed_languages`), and decodes.
//! * Live [`SttEngine::start_stream`]: [`crate::streaming::PseudoStream`] (LocalAgreement-2)
//!   around a [`WhisperChunkDecoder`].

use crate::audio::{pack_windows, samples_to_ms};
use crate::streaming::{ChunkDecoder, ChunkResult, HypWord, PseudoStream, StreamConfig};
use almond_types::artifact::body::{Segment, Word};
use almond_types::meeting::AudioChannel;
use almond_types::stt::{SttEngine, SttModelInfo, SttOptions, SttStream};
use almond_types::{Error, LangTag, Result};
use async_trait::async_trait;
use std::path::Path;
use std::sync::{Arc, Once};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

/// Load-time options.
#[derive(Debug, Clone)]
pub struct WhisperConfig {
    pub use_gpu: bool,
    pub flash_attn: bool,
    /// CPU threads for decoding (also used by GPU builds for the non-offloaded parts).
    pub threads: i32,
    /// Beam size; 1 = greedy (faster, used for live captions).
    pub beam_size: i32,
    /// Live-caption tuning.
    pub stream: StreamConfig,
}

impl Default for WhisperConfig {
    fn default() -> Self {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().min(8) as i32)
            .unwrap_or(4);
        WhisperConfig {
            use_gpu: cfg!(any(feature = "cuda", feature = "vulkan")),
            flash_attn: cfg!(any(feature = "cuda", feature = "vulkan")),
            threads,
            beam_size: 5,
            stream: StreamConfig::default(),
        }
    }
}

/// A loaded ggml model, shareable between offline jobs and streams.
pub struct WhisperModel {
    ctx: WhisperContext,
    info: SttModelInfo,
    cfg: WhisperConfig,
}

static LOG_HOOK: Once = Once::new();

impl WhisperModel {
    /// Load `path`. `info.languages` with exactly one entry forces that language (PhoWhisper).
    pub fn load(path: impl AsRef<Path>, info: SttModelInfo, cfg: WhisperConfig) -> Result<Self> {
        // Route whisper.cpp / ggml logging away from stderr (silenced unless a log backend is on).
        LOG_HOOK.call_once(whisper_rs::install_logging_hooks);
        let path = path.as_ref();
        let mut p = WhisperContextParameters::default();
        p.use_gpu(cfg.use_gpu).flash_attn(cfg.flash_attn);
        let ctx = WhisperContext::new_with_params(
            path.to_str()
                .ok_or_else(|| Error::Invalid("non-UTF-8 model path".into()))?,
            p,
        )
        .map_err(|e| Error::Other(format!("loading {}: {e}", path.display())))?;
        Ok(WhisperModel { ctx, info, cfg })
    }

    /// Load a registry model from the [`crate::models::ModelManager`].
    pub fn from_manager(
        mgr: &crate::models::ModelManager,
        id: &str,
        cfg: WhisperConfig,
    ) -> Result<Self> {
        if !mgr.is_installed(id) {
            return Err(Error::NotFound(format!("model {id} is not installed")));
        }
        let mut info = mgr.info(id)?;
        info.streaming = true; // pseudo-streaming
        Self::load(mgr.file(id, "model")?, info, cfg)
    }

    pub fn info(&self) -> &SttModelInfo {
        &self.info
    }

    pub fn config(&self) -> &WhisperConfig {
        &self.cfg
    }

    pub fn create_state(&self) -> Result<WhisperState> {
        self.ctx.create_state().map_err(Error::other)
    }

    /// The single language this model is restricted to (PhoWhisper -> vi), if any.
    fn fixed_language(&self) -> Option<LangTag> {
        (self.info.languages.len() == 1).then(|| self.info.languages[0].clone())
    }

    /// Language ID on (up to) the first 30 s of `samples`, restricted to `allowed` if non-empty.
    /// Returns (language, probability among allowed, renormalized).
    pub fn identify_language(
        &self,
        state: &mut WhisperState,
        samples: &[f32],
        allowed: &[LangTag],
    ) -> Result<(LangTag, f32)> {
        if let Some(l) = self.fixed_language() {
            return Ok((l, 1.0));
        }
        state
            .pcm_to_mel(samples, self.cfg.threads as usize)
            .map_err(Error::other)?;
        let (best, probs) = state
            .lang_detect(0, self.cfg.threads as usize)
            .map_err(Error::other)?;
        if allowed.is_empty() {
            let code = whisper_rs::get_lang_str(best).unwrap_or("en");
            return Ok((LangTag(code.into()), probs[best as usize]));
        }
        let mut cands: Vec<(LangTag, f32)> = allowed
            .iter()
            .filter_map(|l| {
                whisper_rs::get_lang_id(&l.primary())
                    .map(|id| (LangTag(l.primary()), probs[id as usize]))
            })
            .collect();
        let total: f32 = cands.iter().map(|c| c.1).sum::<f32>().max(1e-9);
        cands.sort_by(|a, b| b.1.total_cmp(&a.1));
        let (l, p) = cands
            .into_iter()
            .next()
            .ok_or_else(|| Error::Invalid("no supported language in allowed_languages".into()))?;
        Ok((l, p / total))
    }

    /// Run `whisper_full` on `samples` (<= 30 s recommended) and return timed segments with
    /// word-level tokens. Times are relative to `samples[0]`.
    fn decode_window(
        &self,
        state: &mut WhisperState,
        samples: &[f32],
        lang: &LangTag,
        prompt: Option<&str>,
        beam_size: i32,
    ) -> Result<Vec<RawSegment>> {
        let strategy = if beam_size > 1 {
            SamplingStrategy::BeamSearch {
                beam_size,
                patience: -1.0,
            }
        } else {
            SamplingStrategy::Greedy { best_of: 1 }
        };
        let lang_code = lang.primary();
        let mut params = FullParams::new(strategy);
        params.set_n_threads(self.cfg.threads);
        params.set_language(Some(&lang_code));
        params.set_translate(false);
        params.set_no_context(true);
        params.set_token_timestamps(true);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        if let Some(p) = prompt.filter(|p| !p.trim().is_empty()) {
            params.set_initial_prompt(p);
        }
        state.full(params, samples).map_err(Error::other)?;

        let eot = self.ctx.token_eot();
        let mut out = vec![];
        for seg in state.as_iter() {
            let n = seg.n_tokens();
            let mut words: Vec<HypWord> = vec![];
            let mut cur: Vec<u8> = vec![];
            let mut cur_t0 = 0i64;
            let mut cur_t1 = 0i64;
            let mut cur_p: Vec<f32> = vec![];
            let flush = |words: &mut Vec<HypWord>,
                         cur: &mut Vec<u8>,
                         t0: i64,
                         t1: i64,
                         ps: &mut Vec<f32>| {
                if !cur.is_empty() {
                    let text = String::from_utf8_lossy(cur).into_owned();
                    if !text.trim().is_empty() {
                        words.push(HypWord {
                            t0_ms: (t0.max(0) * 10) as u64,
                            t1_ms: (t1.max(t0).max(0) * 10) as u64,
                            text,
                            p: (!ps.is_empty()).then(|| ps.iter().sum::<f32>() / ps.len() as f32),
                        });
                    }
                }
                cur.clear();
                ps.clear();
            };
            for i in 0..n {
                let Some(tok) = seg.get_token(i) else {
                    continue;
                };
                if tok.token_id() >= eot {
                    continue; // special / timestamp token
                }
                let data = tok.token_data();
                let bytes = tok.to_bytes().unwrap_or(&[]);
                let starts_word = bytes.first() == Some(&b' ');
                if starts_word || cur.is_empty() {
                    flush(&mut words, &mut cur, cur_t0, cur_t1, &mut cur_p);
                    cur_t0 = data.t0;
                }
                cur.extend_from_slice(bytes);
                cur_t1 = data.t1;
                cur_p.push(data.p);
            }
            flush(&mut words, &mut cur, cur_t0, cur_t1, &mut cur_p);
            let text = seg
                .to_str_lossy()
                .map(|s| s.into_owned())
                .unwrap_or_default();
            out.push(RawSegment {
                t0_ms: (seg.start_timestamp().max(0) * 10) as u64,
                t1_ms: (seg.end_timestamp().max(0) * 10) as u64,
                text,
                words,
                no_speech_p: seg.no_speech_probability(),
            });
        }
        Ok(out)
    }

    /// Synchronous offline transcription (see module docs).
    pub fn transcribe_sync(
        &self,
        samples: &[f32],
        t_offset_ms: u64,
        channel: AudioChannel,
        opts: &SttOptions,
    ) -> Result<Vec<Segment>> {
        let mut state = self.create_state()?;
        let windows = pack_windows(samples);
        let mut segs: Vec<Segment> = vec![];
        let mut prev: Option<(LangTag, String)> = None;
        for (a, b) in windows {
            let win = &samples[a..b];
            let lang = match (&opts.language, self.fixed_language()) {
                (_, Some(l)) => l,
                (Some(l), None) => LangTag(l.primary()),
                (None, None) => {
                    self.identify_language(&mut state, win, &opts.allowed_languages)?
                        .0
                }
            };
            // Context: user vocabulary + previous window's text when in the same language.
            let mut prompt = opts.prompt.clone().unwrap_or_default();
            if let Some((pl, pt)) = &prev
                && pl == &lang
            {
                let tail: String = {
                    let c: Vec<char> = pt.chars().collect();
                    c[c.len().saturating_sub(150)..].iter().collect()
                };
                prompt = format!("{prompt} {tail}");
            }
            let raw = self.decode_window(
                &mut state,
                win,
                &lang,
                Some(prompt.trim()),
                self.cfg.beam_size,
            )?;
            let base = t_offset_ms + samples_to_ms(a);
            let mut window_text = String::new();
            for r in raw {
                if r.text.trim().is_empty() || is_hallucination(&r) {
                    continue;
                }
                window_text.push_str(&r.text);
                let ps: Vec<f32> = r.words.iter().filter_map(|w| w.p).collect();
                segs.push(Segment {
                    idx: segs.len() as u32,
                    t0_ms: (base + r.t0_ms) as u32,
                    t1_ms: (base + r.t1_ms.min(samples_to_ms(b - a))) as u32,
                    channel,
                    speaker: None,
                    lang: Some(lang.clone()),
                    text: r.text.trim().to_string(),
                    confidence: (!ps.is_empty()).then(|| ps.iter().sum::<f32>() / ps.len() as f32),
                    words: opts.word_timestamps.then(|| {
                        r.words
                            .iter()
                            .map(|w| Word {
                                t0_ms: (base + w.t0_ms) as u32,
                                t1_ms: (base + w.t1_ms) as u32,
                                text: w.text.trim().to_string(),
                                p: w.p,
                            })
                            .collect()
                    }),
                });
            }
            prev = Some((lang, window_text));
        }
        Ok(segs)
    }
}

/// One whisper.cpp segment with word groups (times relative to the window).
#[derive(Debug, Clone)]
struct RawSegment {
    t0_ms: u64,
    t1_ms: u64,
    text: String,
    words: Vec<HypWord>,
    no_speech_p: f32,
}

/// Known silence/noise hallucinations of Whisper-family models (YouTube outros etc.).
const HALLUCINATIONS: &[&str] = &[
    "hẹn gặp lại các bạn trong những video tiếp theo",
    "hãy subscribe cho kênh",
    "cảm ơn các bạn đã theo dõi",
    "ghiền mì gõ",
    "thanks for watching",
    "thank you for watching",
    "subtitles by the amara.org community",
];

fn is_hallucination(r: &RawSegment) -> bool {
    let n = crate::metrics::normalize(&r.text);
    let conf = {
        let ps: Vec<f32> = r.words.iter().filter_map(|w| w.p).collect();
        if ps.is_empty() {
            0.0
        } else {
            ps.iter().sum::<f32>() / ps.len() as f32
        }
    };
    (r.no_speech_p > 0.6 && conf < 0.5)
        || HALLUCINATIONS
            .iter()
            .any(|h| n.contains(&crate::metrics::normalize(h)))
}

/// [`SttEngine`] over a shared [`WhisperModel`].
#[derive(Clone)]
pub struct WhisperEngine {
    model: Arc<WhisperModel>,
}

impl WhisperEngine {
    pub fn new(model: Arc<WhisperModel>) -> Self {
        WhisperEngine { model }
    }
    pub fn model_arc(&self) -> &Arc<WhisperModel> {
        &self.model
    }
}

#[async_trait]
impl SttEngine for WhisperEngine {
    fn model(&self) -> SttModelInfo {
        self.model.info.clone()
    }

    async fn transcribe(
        &self,
        samples: &[f32],
        t_offset_ms: u64,
        channel: AudioChannel,
        opts: &SttOptions,
    ) -> Result<Vec<Segment>> {
        let m = self.model.clone();
        let samples = samples.to_vec();
        let opts = opts.clone();
        tokio::task::spawn_blocking(move || {
            m.transcribe_sync(&samples, t_offset_ms, channel, &opts)
        })
        .await
        .map_err(Error::other)?
    }

    fn start_stream(&self, opts: SttOptions) -> Result<Box<dyn SttStream>> {
        let dec = WhisperChunkDecoder::new(self.model.clone(), opts.clone())?;
        let mut cfg = self.model.cfg.stream.clone();
        if opts.prompt.is_some() {
            cfg.prompt = opts.prompt;
        }
        Ok(Box::new(PseudoStream::new(dec, cfg)))
    }
}

/// Decoder used by live captions: greedy, token timestamps, language per call.
pub struct WhisperChunkDecoder {
    model: Arc<WhisperModel>,
    state: WhisperState,
    opts: SttOptions,
}

impl WhisperChunkDecoder {
    pub fn new(model: Arc<WhisperModel>, opts: SttOptions) -> Result<Self> {
        let state = model.create_state()?;
        Ok(WhisperChunkDecoder { model, state, opts })
    }
}

impl ChunkDecoder for WhisperChunkDecoder {
    fn decode(&mut self, samples: &[f32], prompt: Option<&str>) -> Result<ChunkResult> {
        let lang = match (&self.opts.language, self.model.fixed_language()) {
            (_, Some(l)) => l,
            (Some(l), None) => LangTag(l.primary()),
            (None, None) => {
                self.model
                    .identify_language(&mut self.state, samples, &self.opts.allowed_languages)?
                    .0
            }
        };
        let raw = self
            .model
            .decode_window(&mut self.state, samples, &lang, prompt, 1)?;
        let words = raw
            .into_iter()
            .filter(|r| !is_hallucination(r))
            .flat_map(|r| r.words)
            .collect();
        Ok(ChunkResult {
            words,
            lang: Some(lang),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ModelManager;

    /// Needs a ggml model + a Vietnamese WAV. Run:
    /// `ALMOND_TEST_WAV=path/to/vi.wav cargo test -p almond-stt --features whisper --release -- --ignored whisper_real`
    #[tokio::test]
    #[ignore = "needs model files"]
    async fn whisper_real_model_transcribes() {
        let mgr = ModelManager::open_default().unwrap();
        let id = std::env::var("ALMOND_TEST_MODEL").unwrap_or("whisper:phowhisper-small".into());
        let m = Arc::new(WhisperModel::from_manager(&mgr, &id, WhisperConfig::default()).unwrap());
        let wav = std::env::var("ALMOND_TEST_WAV").expect("set ALMOND_TEST_WAV");
        let x = crate::audio::load_wav_16k_mono(wav).unwrap();
        let e = WhisperEngine::new(m);
        let segs = e
            .transcribe(&x, 0, AudioChannel::Mixed, &SttOptions::default())
            .await
            .unwrap();
        assert!(!segs.is_empty());
        println!("{segs:#?}");
    }
}
