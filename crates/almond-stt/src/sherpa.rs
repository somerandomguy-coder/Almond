//! sherpa-onnx engines: Vietnamese Zipformer transducer (offline model, pseudo-streamed for live
//! captions — sherpa-onnx ships no *streaming* Vietnamese Zipformer as of 2026-11) and speaker
//! diarization (pyannote segmentation-3.0 + 3D-Speaker embeddings + fast clustering).
//!
//! The same models and code path work on Android (sherpa-onnx builds for aarch64-linux-android).

use crate::audio::{pack_windows, samples_to_ms};
use crate::models::ModelManager;
use crate::streaming::{ChunkDecoder, ChunkResult, HypWord, PseudoStream, StreamConfig};
use almond_types::artifact::body::{Segment, Word};
use almond_types::ids::SpeakerId;
use almond_types::meeting::AudioChannel;
use almond_types::stt::{Diarizer, SpeakerTurn, SttEngine, SttModelInfo, SttOptions, SttStream};
use almond_types::{Error, LangTag, Result};
use async_trait::async_trait;
use sherpa_onnx::{
    FastClusteringConfig, OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    OfflineTransducerModelConfig, SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig,
};
use std::sync::{Arc, Mutex};

fn s(p: std::path::PathBuf) -> Option<String> {
    Some(p.to_string_lossy().into_owned())
}

fn default_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get().min(4) as i32)
        .unwrap_or(2)
}

/// Runtime options for sherpa models.
#[derive(Debug, Clone)]
pub struct SherpaConfig {
    pub threads: i32,
    /// "cpu" | "cuda" | "directml" (needs a GPU build of sherpa-onnx; prebuilt libs are CPU).
    pub provider: String,
    /// Split a window's words into segments at pauses longer than this.
    pub segment_pause_ms: u64,
    pub stream: StreamConfig,
}

impl Default for SherpaConfig {
    fn default() -> Self {
        SherpaConfig {
            threads: default_threads(),
            provider: "cpu".into(),
            segment_pause_ms: 600,
            stream: StreamConfig {
                // Zipformer is cheap: decode more often for snappier captions.
                step_ms: 500,
                min_decode_ms: 500,
                ..Default::default()
            },
        }
    }
}

/// An offline Zipformer transducer.
pub struct ZipformerModel {
    rec: OfflineRecognizer,
    info: SttModelInfo,
    cfg: SherpaConfig,
}

impl ZipformerModel {
    pub fn from_manager(mgr: &ModelManager, id: &str, cfg: SherpaConfig) -> Result<Self> {
        if !mgr.is_installed(id) {
            return Err(Error::NotFound(format!("model {id} is not installed")));
        }
        let mut info = mgr.info(id)?;
        info.streaming = true; // pseudo-streaming
        let rc = OfflineRecognizerConfig {
            model_config: OfflineModelConfig {
                transducer: OfflineTransducerModelConfig {
                    encoder: s(mgr.file(id, "encoder")?),
                    decoder: s(mgr.file(id, "decoder")?),
                    joiner: s(mgr.file(id, "joiner")?),
                },
                tokens: s(mgr.file(id, "tokens")?),
                num_threads: cfg.threads,
                provider: Some(cfg.provider.clone()),
                ..Default::default()
            },
            decoding_method: Some("greedy_search".into()),
            ..Default::default()
        };
        let rec = OfflineRecognizer::create(&rc)
            .ok_or_else(|| Error::Other(format!("sherpa-onnx failed to load {id}")))?;
        Ok(ZipformerModel { rec, info, cfg })
    }

    pub fn info(&self) -> &SttModelInfo {
        &self.info
    }

    fn lang(&self) -> Option<LangTag> {
        self.info.languages.first().cloned()
    }

    /// Decode a window into timed words (times relative to `samples[0]`).
    pub fn decode_words(&self, samples: &[f32]) -> Result<Vec<HypWord>> {
        let stream = self.rec.create_stream();
        stream.accept_waveform(16_000, samples);
        self.rec.decode(&stream);
        let r = stream
            .get_result()
            .ok_or_else(|| Error::Other("sherpa-onnx returned no result".into()))?;
        let end_ms = samples_to_ms(samples.len());
        let ts = r.timestamps.unwrap_or_default();
        Ok(tokens_to_words(&r.tokens, &ts, end_ms))
    }

    pub fn transcribe_sync(
        &self,
        samples: &[f32],
        t_offset_ms: u64,
        channel: AudioChannel,
        opts: &SttOptions,
    ) -> Result<Vec<Segment>> {
        let mut segs = vec![];
        for (a, b) in pack_windows(samples) {
            let base = t_offset_ms + samples_to_ms(a);
            let words = self.decode_words(&samples[a..b])?;
            for group in split_on_pauses(&words, self.cfg.segment_pause_ms) {
                let text: String = group.iter().map(|w| w.text.as_str()).collect();
                if text.trim().is_empty() {
                    continue;
                }
                segs.push(Segment {
                    idx: segs.len() as u32,
                    t0_ms: (base + group[0].t0_ms) as u32,
                    t1_ms: (base + group[group.len() - 1].t1_ms) as u32,
                    channel,
                    speaker: None,
                    lang: self.lang(),
                    text: text.trim().to_string(),
                    confidence: None,
                    words: opts.word_timestamps.then(|| {
                        group
                            .iter()
                            .map(|w| Word {
                                t0_ms: (base + w.t0_ms) as u32,
                                t1_ms: (base + w.t1_ms) as u32,
                                text: w.text.trim().to_string(),
                                p: None,
                            })
                            .collect()
                    }),
                });
            }
        }
        Ok(segs)
    }
}

/// BPE tokens ("▁xin", "▁ch", "ào") + start times (s) -> words with leading spaces.
pub fn tokens_to_words(tokens: &[String], ts: &[f32], end_ms: u64) -> Vec<HypWord> {
    let mut words: Vec<HypWord> = vec![];
    for (i, tok) in tokens.iter().enumerate() {
        let t0 = ts.get(i).map(|t| (t * 1000.0) as u64).unwrap_or(end_ms);
        let t1 = ts
            .get(i + 1)
            .map(|t| (t * 1000.0) as u64)
            .unwrap_or(end_ms)
            .max(t0);
        let starts = tok.starts_with('▁') || tok.starts_with(' ');
        let piece = tok.replace('▁', " ");
        match words.last_mut() {
            Some(w) if !starts => {
                w.text.push_str(&piece);
                w.t1_ms = t1;
            }
            _ => words.push(HypWord {
                t0_ms: t0,
                t1_ms: t1,
                text: if starts { piece } else { format!(" {piece}") },
                p: None,
            }),
        }
    }
    // A word's end is the start of the next token, which over-extends across pauses; cap at
    // 1 s past its start so pause detection still works.
    for w in words.iter_mut() {
        w.t1_ms = w.t1_ms.min(w.t0_ms + 1000);
    }
    words
}

fn split_on_pauses(words: &[HypWord], pause_ms: u64) -> Vec<&[HypWord]> {
    let mut out = vec![];
    let mut start = 0;
    for i in 1..words.len() {
        if words[i].t0_ms.saturating_sub(words[i - 1].t1_ms) >= pause_ms {
            out.push(&words[start..i]);
            start = i;
        }
    }
    if start < words.len() {
        out.push(&words[start..]);
    }
    out
}

/// [`SttEngine`] over a shared [`ZipformerModel`].
#[derive(Clone)]
pub struct SherpaEngine {
    model: Arc<ZipformerModel>,
}

impl SherpaEngine {
    pub fn new(model: Arc<ZipformerModel>) -> Self {
        SherpaEngine { model }
    }
}

struct ZipformerChunkDecoder(Arc<ZipformerModel>);

impl ChunkDecoder for ZipformerChunkDecoder {
    fn decode(&mut self, samples: &[f32], _prompt: Option<&str>) -> Result<ChunkResult> {
        Ok(ChunkResult {
            words: self.0.decode_words(samples)?,
            lang: self.0.lang(),
        })
    }
}

#[async_trait]
impl SttEngine for SherpaEngine {
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

    fn start_stream(&self, _opts: SttOptions) -> Result<Box<dyn SttStream>> {
        Ok(Box::new(PseudoStream::new(
            ZipformerChunkDecoder(self.model.clone()),
            self.model.cfg.stream.clone(),
        )))
    }
}

/// Diarization tuning.
#[derive(Debug, Clone)]
pub struct DiarizerConfig {
    pub threads: i32,
    /// Cosine-distance threshold for clustering when the speaker count is unknown
    /// (sherpa default 0.5; larger = fewer speakers).
    pub threshold: f32,
    pub min_duration_on: f32,
    pub min_duration_off: f32,
    /// Fast mode ([`crate::diarize::FastDiarizer`]: VAD windows, one embedding per window,
    /// agglomerative clustering) for recordings without a speaker-count hint. `None` = the
    /// sherpa pyannote pipeline over the whole recording (much slower on long meetings).
    pub fast: Option<crate::diarize::FastConfig>,
}

impl Default for DiarizerConfig {
    fn default() -> Self {
        DiarizerConfig {
            // embedding extraction dominates; 10 threads: RTF 0.15 vs 0.25 with 4 (30-min file)
            threads: std::thread::available_parallelism()
                .map(|n| n.get().min(8) as i32)
                .unwrap_or(4),
            // Tuned on the synthetic meeting (eval/stt/RESULTS.md): 0.5 split 5 true speakers
            // into 48 labels; 1.0 gives ~1 label per speaker at 99.5% attribution accuracy.
            threshold: 1.0,
            min_duration_on: 0.3,
            min_duration_off: 0.5,
            fast: Some(crate::diarize::FastConfig::default()),
        }
    }
}

/// pyannote-seg + speaker-embedding diarizer.
pub struct SherpaDiarizer {
    seg_model: String,
    emb_model: String,
    cfg: DiarizerConfig,
}

impl SherpaDiarizer {
    pub const SEGMENTATION: &'static str = "sherpa:pyannote-seg-3.0";
    pub const EMBEDDING: &'static str = "sherpa:3dspeaker-eres2net-base";

    pub fn from_manager(mgr: &ModelManager, cfg: DiarizerConfig) -> Result<Self> {
        for id in [Self::SEGMENTATION, Self::EMBEDDING] {
            if !mgr.is_installed(id) {
                return Err(Error::NotFound(format!("model {id} is not installed")));
            }
        }
        Ok(SherpaDiarizer {
            seg_model: mgr
                .file(Self::SEGMENTATION, "model")?
                .to_string_lossy()
                .into_owned(),
            emb_model: mgr
                .file(Self::EMBEDDING, "model")?
                .to_string_lossy()
                .into_owned(),
            cfg,
        })
    }

    fn build(&self, num_speakers: Option<u32>) -> Result<OfflineSpeakerDiarization> {
        let threads = self.cfg.threads;
        let c = OfflineSpeakerDiarizationConfig {
            segmentation: OfflineSpeakerSegmentationModelConfig {
                pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: Some(self.seg_model.clone()),
                    ..Default::default()
                },
                num_threads: threads,
                ..Default::default()
            },
            embedding: SpeakerEmbeddingExtractorConfig {
                model: Some(self.emb_model.clone()),
                num_threads: threads,
                ..Default::default()
            },
            clustering: FastClusteringConfig {
                num_clusters: num_speakers.map(|n| n as i32).unwrap_or(-1),
                threshold: self.cfg.threshold,
                ..Default::default()
            },
            min_duration_on: self.cfg.min_duration_on,
            min_duration_off: self.cfg.min_duration_off,
        };
        OfflineSpeakerDiarization::create(&c)
            .ok_or_else(|| Error::Other("sherpa-onnx failed to create diarizer".into()))
    }

    /// Diarize a whole recording. Without a speaker-count hint and with `cfg.fast` set, uses the
    /// fast window-embedding path ([`crate::diarize::FastDiarizer`]); otherwise the sherpa
    /// pyannote pipeline.
    pub fn diarize_sync(
        &self,
        samples: &[f32],
        num_speakers: Option<u32>,
    ) -> Result<Vec<SpeakerTurn>> {
        if let (None, Some(fc)) = (num_speakers, &self.cfg.fast) {
            let workers = fc.workers.max(1);
            let threads = (self.cfg.threads / workers as i32).max(1);
            let pool = (0..workers)
                .map(|_| {
                    SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
                        model: Some(self.emb_model.clone()),
                        num_threads: threads,
                        ..Default::default()
                    })
                    .map(Mutex::new)
                    .ok_or_else(|| {
                        Error::Other("sherpa-onnx failed to create the embedding extractor".into())
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let d = crate::diarize::FastDiarizer {
                embedder: SherpaEmbedder { pool },
                cfg: fc.clone(),
            };
            return d.diarize_sync(samples);
        }
        let d = self.build(num_speakers)?;
        let r = d
            .process(samples)
            .ok_or_else(|| Error::Other("diarization failed".into()))?;
        Ok(r.sort_by_start_time()
            .into_iter()
            .map(|s| SpeakerTurn {
                t0_ms: (s.start * 1000.0) as u32,
                t1_ms: (s.end * 1000.0) as u32,
                speaker: SpeakerId(format!("S{}", s.speaker + 1)),
            })
            .collect())
    }
}

/// A pool of 3D-Speaker extractors, one per embedding worker.
struct SherpaEmbedder {
    pool: Vec<Mutex<SpeakerEmbeddingExtractor>>,
}

impl crate::diarize::Embedder for SherpaEmbedder {
    fn embed(&self, samples: &[f32]) -> Result<Vec<f32>> {
        // FastDiarizer runs as many workers as there are extractors, so one is normally free.
        let ex = loop {
            if let Some(g) = self.pool.iter().find_map(|m| m.try_lock().ok()) {
                break g;
            }
            std::thread::yield_now();
        };
        let st = ex
            .create_stream()
            .ok_or_else(|| Error::Other("sherpa-onnx: embedding stream".into()))?;
        st.accept_waveform(crate::audio::SR as i32, samples);
        st.input_finished();
        if !ex.is_ready(&st) {
            return Err(Error::Invalid(
                "too little audio for a speaker embedding".into(),
            ));
        }
        ex.compute(&st)
            .ok_or_else(|| Error::Other("sherpa-onnx: embedding failed".into()))
    }
}

#[async_trait]
impl Diarizer for SherpaDiarizer {
    async fn diarize(
        &self,
        samples: &[f32],
        num_speakers: Option<u32>,
    ) -> Result<Vec<SpeakerTurn>> {
        let this = SherpaDiarizer {
            seg_model: self.seg_model.clone(),
            emb_model: self.emb_model.clone(),
            cfg: self.cfg.clone(),
        };
        let samples = samples.to_vec();
        tokio::task::spawn_blocking(move || this.diarize_sync(&samples, num_speakers))
            .await
            .map_err(Error::other)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpe_tokens_to_words() {
        let toks: Vec<String> = ["▁xin", "▁ch", "ào", "▁các", "▁bạn"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ts = [0.1, 0.5, 0.7, 2.5, 2.9];
        let w = tokens_to_words(&toks, &ts, 3500);
        let text: String = w.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(text.trim(), "xin chào các bạn");
        assert_eq!(w.len(), 4);
        assert_eq!(w[1].t0_ms, 500);
        // "chào" would run to 2500 (next token); capped at +1 s
        assert_eq!(w[1].t1_ms, 1500);
        let groups = split_on_pauses(&w, 600);
        assert_eq!(groups.len(), 2);
    }

    /// Run: `cargo test -p almond-stt --features sherpa --release -- --ignored zipformer_test_wavs`
    /// (needs `sherpa:zipformer-vi-30m-int8` installed; uses the test wavs shipped in the archive).
    #[test]
    #[ignore = "needs model files"]
    fn zipformer_test_wavs() {
        let mgr = ModelManager::open_default().unwrap();
        let id = "sherpa:zipformer-vi-30m-int8";
        let m = ZipformerModel::from_manager(&mgr, id, SherpaConfig::default()).unwrap();
        let wav = mgr.model_dir(id).unwrap().join("test_wavs").join("0.wav");
        let x = crate::audio::load_wav_16k_mono(wav).unwrap();
        let segs = m
            .transcribe_sync(&x, 0, AudioChannel::Mixed, &SttOptions::default())
            .unwrap();
        println!("{segs:#?}");
        assert!(!segs.is_empty());
    }
}
