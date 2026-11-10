//! Language-routed live captions for meetings that switch between languages (vi / en).
//!
//! A [`LiveRouter`] stream is the usual LocalAgreement pseudo-stream ([`crate::streaming`]) with
//! two additions:
//!
//! * **partials** are decoded by the fast model of the language the buffer is currently in:
//!   [`RoutedChunkDecoder`] runs language ID on the buffer (when `lid_partials`) and keeps the
//!   current language unless the evidence is strong enough ([`crate::langid::OnlineLid`]);
//! * every **final** is re-decoded ([`RoutedRefiner`]) by the language's offline model on the
//!   whole segment (plus padding), after a language decision on the whole segment — so finals
//!   are in the right language even when the partials were not, and use full context.
//!
//! The `SttUpdate` protocol is unchanged (the refined final replaces the partials of its idx).

use crate::audio::{ms_to_samples, samples_to_ms};
use crate::langid::{LidObs, LidPolicy, OnlineLid};
use crate::router::{LanguageId, identify_regions};
use crate::streaming::{
    ChunkDecoder, ChunkResult, LocalAgreement, PseudoStream, SegmentRefiner, StreamConfig,
};
use almond_types::artifact::body::Segment;
use almond_types::meeting::AudioChannel;
use almond_types::stt::{SttEngine, SttModelInfo, SttOptions, SttStream};
use almond_types::{Error, LangTag, Result};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// A loaded model usable for live captions: a chunk decoder for partials and a synchronous
/// offline decode for finals.
pub trait LiveModel: Send + Sync {
    fn info(&self) -> SttModelInfo;
    /// A decoder for partials in `lang`.
    fn chunk_decoder(&self, lang: &LangTag, opts: &SttOptions) -> Result<Box<dyn ChunkDecoder>>;
    /// Offline decode of one segment (blocking).
    fn transcribe_blocking(
        &self,
        samples: &[f32],
        t_offset_ms: u64,
        channel: AudioChannel,
        opts: &SttOptions,
    ) -> Result<Vec<Segment>>;
    /// The model's preferred live-caption tuning.
    fn stream_config(&self) -> StreamConfig;
}

/// Live captions routed by language.
pub struct LiveRouter {
    /// None: no language ID; everything is in the first of `langs`.
    pub lid: Option<Arc<dyn LanguageId>>,
    /// Allowed languages, primary first.
    pub langs: Vec<LangTag>,
    /// Primary subtag -> model for partials.
    pub partial: BTreeMap<String, Arc<dyn LiveModel>>,
    /// Primary subtag -> model that re-decodes finals (missing: finals are not re-decoded).
    pub finals: BTreeMap<String, Arc<dyn LiveModel>>,
    pub policy: LidPolicy,
    /// Run language ID on every partial decode (needs a fast LID model: GPU). Off: partials use
    /// the language of the previous final; finals still get their own language decision.
    pub lid_partials: bool,
    pub stream: StreamConfig,
}

impl LiveRouter {
    fn primary(&self) -> Result<LangTag> {
        self.langs
            .first()
            .cloned()
            .ok_or_else(|| Error::Invalid("live captions need at least one language".into()))
    }

    fn model_for<'a>(
        map: &'a BTreeMap<String, Arc<dyn LiveModel>>,
        l: &LangTag,
    ) -> Option<&'a Arc<dyn LiveModel>> {
        map.get(&l.primary())
    }
}

#[async_trait]
impl SttEngine for LiveRouter {
    fn model(&self) -> SttModelInfo {
        let ids: Vec<String> = self
            .langs
            .iter()
            .filter_map(|l| {
                let p = Self::model_for(&self.partial, l)?.info().id;
                Some(match Self::model_for(&self.finals, l) {
                    Some(f) if f.info().id != p => format!("{}={p}>{}", l.0, f.info().id),
                    _ => format!("{}={p}", l.0),
                })
            })
            .collect();
        SttModelInfo {
            id: format!("live:{}", ids.join("+")),
            engine: "routed-live".into(),
            languages: self.langs.clone(),
            streaming: true,
            size_bytes: None,
            sha256: None,
            installed: true,
            role: almond_types::stt::ModelRole::Stt,
        }
    }

    async fn transcribe(
        &self,
        samples: &[f32],
        t_offset_ms: u64,
        channel: AudioChannel,
        opts: &SttOptions,
    ) -> Result<Vec<Segment>> {
        // Not the meeting path (that is `RoutedEngine`); a simple routed decode for completeness.
        let primary = self.primary()?;
        let regions = match (&self.lid, &opts.language) {
            (_, Some(l)) => vec![(0, samples.len(), l.clone())],
            (Some(lid), None) => {
                identify_regions(lid.as_ref(), samples, &self.langs, &self.policy)?
            }
            (None, None) => vec![(0, samples.len(), primary)],
        };
        let mut out = vec![];
        for (a, b, l) in regions {
            let m = Self::model_for(&self.finals, &l)
                .or_else(|| Self::model_for(&self.partial, &l))
                .ok_or_else(|| Error::Unsupported(format!("no live model for {}", l.0)))?;
            let mut o = opts.clone();
            o.language = Some(l);
            out.extend(m.transcribe_blocking(
                &samples[a..b],
                t_offset_ms + crate::audio::samples_to_ms(a),
                channel,
                &o,
            )?);
        }
        for (i, s) in out.iter_mut().enumerate() {
            s.idx = i as u32;
        }
        Ok(out)
    }

    fn start_stream(&self, opts: SttOptions) -> Result<Box<dyn SttStream>> {
        let primary = self.primary()?;
        // A fixed language (the user picked one): no routing.
        let langs: Vec<LangTag> = match &opts.language {
            Some(l) => vec![LangTag(l.primary())],
            None => self.langs.clone(),
        };
        let state = Arc::new(Mutex::new(OnlineLid::new(
            langs.clone(),
            self.policy.clone(),
        )));
        let mut decoders = BTreeMap::new();
        for l in &langs {
            if let Some(m) = Self::model_for(&self.partial, l) {
                decoders.insert(l.primary(), m.chunk_decoder(l, &opts)?);
            }
        }
        if decoders.is_empty() {
            return Err(Error::Unsupported(format!(
                "no live caption model for {}",
                primary.0
            )));
        }
        let first = langs.first().cloned().unwrap_or(primary);
        let cfg = Self::model_for(&self.partial, &first)
            .map(|m| m.stream_config())
            .unwrap_or_else(|| self.stream.clone());
        let dec = RoutedChunkDecoder {
            lid: if self.lid_partials && langs.len() > 1 {
                self.lid.clone()
            } else {
                None
            },
            decoders,
            langs: langs.clone(),
            state: state.clone(),
        };
        let finals: BTreeMap<String, Arc<dyn LiveModel>> = langs
            .iter()
            .filter_map(|l| Some((l.primary(), Self::model_for(&self.finals, l)?.clone())))
            .collect();
        let mut la = LocalAgreement::new(dec, cfg);
        if !finals.is_empty() || (self.lid.is_some() && langs.len() > 1) {
            la = la.with_refiner(Box::new(RoutedRefiner {
                lid: if langs.len() > 1 {
                    self.lid.clone()
                } else {
                    None
                },
                finals,
                langs,
                state,
                prev_end: None,
                prev_tail: vec![],
                opts,
            }));
        }
        Ok(Box::new(PseudoStream::from_local_agreement(la)))
    }
}

/// Partials: language ID on the buffer, then the fast decoder of that language.
pub struct RoutedChunkDecoder {
    lid: Option<Arc<dyn LanguageId>>,
    decoders: BTreeMap<String, Box<dyn ChunkDecoder>>,
    langs: Vec<LangTag>,
    state: Arc<Mutex<OnlineLid>>,
}

impl ChunkDecoder for RoutedChunkDecoder {
    fn decode(&mut self, samples: &[f32], prompt: Option<&str>) -> Result<ChunkResult> {
        let st = self.state.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let mut lang = st.current().unwrap_or_else(|| self.langs[0].clone());
        if let Some(lid) = &self.lid {
            let dur_ms = crate::audio::samples_to_ms(samples.len());
            if dur_ms >= st.policy.min_evidence_ms {
                let probs = lid.probabilities(samples, &self.langs)?;
                if let Some(l) = st.peek(&LidObs {
                    dur_ms,
                    gap_ms: 0,
                    probs,
                }) {
                    lang = l;
                }
            }
        }
        let dec = match self.decoders.get_mut(&lang.primary()) {
            Some(d) => d,
            None => self
                .decoders
                .values_mut()
                .next()
                .ok_or_else(|| Error::Other("no partial decoder".into()))?,
        };
        let mut r = dec.decode(samples, prompt)?;
        r.lang.get_or_insert(lang);
        Ok(r)
    }
}

/// Finals: the segment's audio is split at short pauses, each part gets a language (online
/// policy, continuing from the previous finals), consecutive same-language parts are decoded by
/// that language's offline model, and the pieces are joined into one final. A live segment can
/// span a language change (turns 300 ms apart), so one decision per segment is not enough.
pub struct RoutedRefiner {
    lid: Option<Arc<dyn LanguageId>>,
    finals: BTreeMap<String, Arc<dyn LiveModel>>,
    langs: Vec<LangTag>,
    state: Arc<Mutex<OnlineLid>>,
    /// End (absolute ms) of the audio already covered by earlier finals.
    prev_end: Option<u64>,
    /// Normalized words at the end of the previous final (to drop a repeated overlap).
    prev_tail: Vec<String>,
    opts: SttOptions,
}

impl RoutedRefiner {
    /// Language of each VAD part of `audio` (times in samples), committed to the online state.
    fn languages(
        &mut self,
        audio: &[f32],
        t0_ms: u64,
        fallback: &LangTag,
    ) -> Result<Vec<(usize, usize, LangTag)>> {
        let vad = crate::audio::VadParams {
            min_silence_ms: 200,
            ..Default::default()
        };
        let mut regions = crate::audio::split_on_silence(audio, &vad);
        if regions.is_empty() {
            regions.push((0, audio.len()));
        }
        let Some(lid) = &self.lid else {
            return Ok(regions
                .into_iter()
                .map(|(a, b)| (a, b, fallback.clone()))
                .collect());
        };
        let mut out = vec![];
        let mut last_end = self.prev_end;
        for (a, b) in regions {
            let start = t0_ms + samples_to_ms(a);
            let obs = LidObs {
                dur_ms: samples_to_ms(b - a),
                gap_ms: last_end.map(|e| start.saturating_sub(e)).unwrap_or(0),
                probs: lid.probabilities(&audio[a..b], &self.langs)?,
            };
            last_end = Some(t0_ms + samples_to_ms(b));
            let l = self
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .commit(&obs)
                .unwrap_or_else(|| fallback.clone());
            out.push((a, b, l));
        }
        Ok(out)
    }
}

impl SegmentRefiner for RoutedRefiner {
    fn refine(
        &mut self,
        audio: &[f32],
        audio_t0_ms: u64,
        seg: &Segment,
    ) -> Result<Option<Segment>> {
        // Audio before the end of the previous final was decoded already (the padding and
        // LocalAgreement's word ends overlap): decoding it again repeats its words.
        let skip_ms = self
            .prev_end
            .map(|e| e.saturating_sub(audio_t0_ms))
            .unwrap_or(0);
        let skip = ms_to_samples(skip_ms).min(audio.len());
        let (audio, t0) = (&audio[skip..], audio_t0_ms + samples_to_ms(skip));
        let fallback = seg.lang.clone().unwrap_or_else(|| self.langs[0].clone());
        if audio.len() < ms_to_samples(200) {
            return Ok(None);
        }
        let parts = self.languages(audio, t0, &fallback)?;
        // consecutive same-language parts -> one decode
        let mut groups: Vec<(usize, usize, LangTag)> = vec![];
        for (a, b, l) in parts {
            match groups.last_mut() {
                Some(g) if g.2 == l => g.1 = b,
                _ => groups.push((a, b, l)),
            }
        }
        let mut pieces: Vec<Segment> = vec![];
        for (i, (a, b, l)) in groups.iter().enumerate() {
            // extend each group to its neighbours' boundaries so no audio is lost between parts
            let a = if i == 0 { 0 } else { *a };
            let b = if i + 1 == groups.len() {
                audio.len()
            } else {
                *b
            };
            let off = t0 + samples_to_ms(a);
            match self.finals.get(&l.primary()) {
                Some(m) => {
                    let mut o = self.opts.clone();
                    o.language = Some(l.clone());
                    o.word_timestamps = true;
                    pieces.extend(m.transcribe_blocking(&audio[a..b], off, seg.channel, &o)?);
                }
                None => {
                    // no offline model: the committed words of this span
                    let end = t0 + samples_to_ms(b);
                    let words: Vec<_> = seg
                        .words
                        .iter()
                        .flatten()
                        .filter(|w| {
                            let mid = (w.t0_ms as u64 + w.t1_ms as u64) / 2;
                            mid >= off && mid < end
                        })
                        .cloned()
                        .collect();
                    if !words.is_empty() {
                        pieces.push(Segment {
                            text: words
                                .iter()
                                .map(|w| w.text.as_str())
                                .collect::<Vec<_>>()
                                .join(" "),
                            t0_ms: words[0].t0_ms,
                            t1_ms: words[words.len() - 1].t1_ms,
                            lang: Some(l.clone()),
                            words: Some(words),
                            ..seg.clone()
                        });
                    }
                }
            }
        }
        self.prev_end = Some(
            (seg.t1_ms as u64)
                .max(pieces.last().map(|s| s.t1_ms as u64).unwrap_or(0))
                .max(t0),
        );
        if pieces.is_empty() && groups.iter().all(|g| g.2 == fallback) {
            // The offline model heard nothing where the partials were in the same language:
            // keep the committed text rather than emit an empty final.
            return Ok(None);
        }
        let mut out = merge(pieces, seg, fallback);
        drop_repeated_overlap(&mut out, &self.prev_tail);
        let words: Vec<String> = crate::metrics::normalize(&out.text)
            .split_whitespace()
            .map(String::from)
            .collect();
        if !words.is_empty() {
            self.prev_tail = words[words.len().saturating_sub(12)..].to_vec();
        }
        Ok(Some(out))
    }
}

/// Remove leading words of `s` that repeat the end of the previous final (>= 2 words).
fn drop_repeated_overlap(s: &mut Segment, prev_tail: &[String]) {
    let words: Vec<String> = crate::metrics::normalize(&s.text)
        .split_whitespace()
        .map(String::from)
        .collect();
    let max = words.len().min(prev_tail.len());
    let k = (2..=max)
        .rev()
        .find(|&k| words[..k] == prev_tail[prev_tail.len() - k..])
        .unwrap_or(0);
    if k == 0 {
        return;
    }
    // drop the first k words of the text (keeping the rest's punctuation and case)
    let mut seen = 0;
    let mut cut = s.text.len();
    for (i, w) in s
        .text
        .split_inclusive(char::is_whitespace)
        .scan(0, |pos, w| {
            let start = *pos;
            *pos += w.len();
            Some((start, w))
        })
    {
        if seen == k {
            cut = i;
            break;
        }
        if !crate::metrics::normalize(w).is_empty() {
            seen += 1;
        }
    }
    s.text = s.text[cut.min(s.text.len())..].trim().to_string();
    if let Some(ws) = s.words.as_mut() {
        let n = k.min(ws.len());
        ws.drain(..n);
        if let Some(w) = ws.first() {
            s.t0_ms = w.t0_ms;
        }
    }
}

/// Several decoded segments -> one caption segment, in the language with the most words.
fn merge(segs: Vec<Segment>, like: &Segment, fallback: LangTag) -> Segment {
    let text = segs
        .iter()
        .map(|s| s.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for s in &segs {
        if let Some(l) = &s.lang {
            *count.entry(l.primary()).or_default() += s.text.split_whitespace().count();
        }
    }
    let lang = count
        .into_iter()
        .max_by_key(|x| x.1)
        .map(|x| LangTag(x.0))
        .unwrap_or(fallback);
    let words: Vec<_> = segs
        .iter()
        .flat_map(|s| s.words.clone().unwrap_or_default())
        .collect();
    let confs: Vec<f32> = segs.iter().filter_map(|s| s.confidence).collect();
    Segment {
        idx: like.idx,
        t0_ms: segs.first().map(|s| s.t0_ms).unwrap_or(like.t0_ms),
        t1_ms: segs.last().map(|s| s.t1_ms).unwrap_or(like.t1_ms),
        channel: like.channel,
        speaker: like.speaker.clone(),
        lang: Some(lang),
        text,
        confidence: (!confs.is_empty()).then(|| confs.iter().sum::<f32>() / confs.len() as f32),
        words: Some(words),
    }
}

#[cfg(feature = "whisper")]
impl LiveModel for crate::whisper::WhisperEngine {
    fn info(&self) -> SttModelInfo {
        self.model_arc().info().clone()
    }
    fn chunk_decoder(&self, lang: &LangTag, opts: &SttOptions) -> Result<Box<dyn ChunkDecoder>> {
        let mut o = opts.clone();
        o.language = Some(lang.clone());
        Ok(Box::new(crate::whisper::WhisperChunkDecoder::new(
            self.model_arc().clone(),
            o,
        )?))
    }
    fn transcribe_blocking(
        &self,
        samples: &[f32],
        t_offset_ms: u64,
        channel: AudioChannel,
        opts: &SttOptions,
    ) -> Result<Vec<Segment>> {
        self.model_arc()
            .transcribe_sync(samples, t_offset_ms, channel, opts)
    }
    fn stream_config(&self) -> StreamConfig {
        self.model_arc().config().stream.clone()
    }
}

#[cfg(feature = "sherpa")]
impl LiveModel for crate::sherpa::SherpaEngine {
    fn info(&self) -> SttModelInfo {
        self.model_arc().info().clone()
    }
    fn chunk_decoder(&self, _: &LangTag, _: &SttOptions) -> Result<Box<dyn ChunkDecoder>> {
        Ok(Box::new(self.chunk_decoder()))
    }
    fn transcribe_blocking(
        &self,
        samples: &[f32],
        t_offset_ms: u64,
        channel: AudioChannel,
        opts: &SttOptions,
    ) -> Result<Vec<Segment>> {
        self.model_arc()
            .transcribe_sync(samples, t_offset_ms, channel, opts)
    }
    fn stream_config(&self) -> StreamConfig {
        self.model_arc().config().stream.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::{HypWord, frames};

    /// Decodes everything as a fixed word list; the final model returns `final_text`.
    struct Fake {
        id: &'static str,
        word: &'static str,
        final_text: &'static str,
    }

    struct FakeDec(&'static str);
    impl ChunkDecoder for FakeDec {
        fn decode(&mut self, samples: &[f32], _: Option<&str>) -> Result<ChunkResult> {
            let ms = crate::audio::samples_to_ms(samples.len());
            Ok(ChunkResult {
                words: (0..ms / 400)
                    .map(|i| HypWord {
                        t0_ms: i * 400,
                        t1_ms: i * 400 + 300,
                        text: format!(" {}", self.0),
                        p: None,
                    })
                    .collect(),
                lang: None,
            })
        }
    }

    impl LiveModel for Fake {
        fn info(&self) -> SttModelInfo {
            SttModelInfo {
                id: self.id.into(),
                engine: "fake".into(),
                languages: vec![],
                streaming: true,
                size_bytes: None,
                sha256: None,
                installed: true,
                role: almond_types::stt::ModelRole::Stt,
            }
        }
        fn chunk_decoder(&self, _: &LangTag, _: &SttOptions) -> Result<Box<dyn ChunkDecoder>> {
            Ok(Box::new(FakeDec(self.word)))
        }
        fn transcribe_blocking(
            &self,
            samples: &[f32],
            t_offset_ms: u64,
            channel: AudioChannel,
            opts: &SttOptions,
        ) -> Result<Vec<Segment>> {
            Ok(vec![Segment {
                idx: 0,
                t0_ms: t_offset_ms as u32,
                t1_ms: (t_offset_ms + crate::audio::samples_to_ms(samples.len())) as u32,
                channel,
                speaker: None,
                lang: opts.language.clone(),
                text: self.final_text.into(),
                confidence: None,
                words: None,
            }])
        }
        fn stream_config(&self) -> StreamConfig {
            StreamConfig {
                step_ms: 500,
                min_decode_ms: 500,
                ..Default::default()
            }
        }
    }

    struct AlwaysEnglish;
    impl LanguageId for AlwaysEnglish {
        fn identify(&self, _: &[f32], _: &[LangTag]) -> Result<(LangTag, f32)> {
            Ok((LangTag::en(), 0.99))
        }
    }

    fn router(lid: Option<Arc<dyn LanguageId>>) -> LiveRouter {
        let vi: Arc<dyn LiveModel> = Arc::new(Fake {
            id: "vi-fast",
            word: "xin",
            final_text: "xin chào",
        });
        let en: Arc<dyn LiveModel> = Arc::new(Fake {
            id: "en-big",
            word: "hello",
            final_text: "hello there",
        });
        LiveRouter {
            lid,
            langs: vec![LangTag::vi(), LangTag::en()],
            partial: [
                ("vi".to_string(), vi.clone()),
                ("en".to_string(), en.clone()),
            ]
            .into(),
            finals: [("vi".to_string(), vi), ("en".to_string(), en)].into(),
            policy: LidPolicy::default(),
            lid_partials: true,
            stream: StreamConfig::default(),
        }
    }

    async fn run(r: &LiveRouter) -> Vec<almond_types::stt::SttUpdate> {
        let audio: Vec<f32> = (0..crate::audio::ms_to_samples(4000))
            .map(|i| 0.2 * ((i as f32) * 0.07).sin())
            .collect();
        let mut s = r.start_stream(SttOptions::default()).unwrap();
        let mut ups = vec![];
        for f in frames(&audio, 100, AudioChannel::Mic) {
            ups.extend(s.push(f).await.unwrap());
        }
        ups.extend(s.finish().await.unwrap());
        ups
    }

    #[tokio::test]
    async fn english_speech_gets_english_partials_and_finals() {
        let ups = run(&router(Some(Arc::new(AlwaysEnglish)))).await;
        let finals: Vec<_> = ups.iter().filter(|u| u.is_final).collect();
        assert!(!finals.is_empty());
        for f in &finals {
            assert_eq!(f.segment.text, "hello there");
            assert_eq!(f.segment.lang, Some(LangTag::en()));
        }
        // partials switched to the English decoder once there was enough audio
        assert!(
            ups.iter()
                .any(|u| !u.is_final && u.segment.text.contains("hello"))
        );
        // one final per idx, no updates after it
        for (i, u) in ups.iter().enumerate() {
            if u.is_final {
                assert!(ups[i + 1..].iter().all(|v| v.segment.idx != u.segment.idx));
            }
        }
    }

    #[test]
    fn repeated_overlap_with_the_previous_final_is_dropped() {
        let w = |t: &str, i: u32| almond_types::artifact::body::Word {
            t0_ms: i * 100,
            t1_ms: i * 100 + 90,
            text: t.into(),
            p: None,
        };
        let mut s = Segment {
            idx: 0,
            t0_ms: 0,
            t1_ms: 500,
            channel: AudioChannel::Mic,
            speaker: None,
            lang: None,
            text: "for kids. And then we left.".into(),
            confidence: None,
            words: Some(vec![
                w("for", 0),
                w("kids.", 1),
                w("And", 2),
                w("then", 3),
                w("we", 4),
                w("left.", 5),
            ]),
        };
        let prev: Vec<String> = ["interesting", "things", "for", "kids"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        drop_repeated_overlap(&mut s, &prev);
        assert_eq!(s.text, "And then we left.");
        assert_eq!(s.words.as_ref().unwrap().len(), 4);
        assert_eq!(s.t0_ms, 200);
    }

    #[tokio::test]
    async fn without_lid_everything_is_primary() {
        let ups = run(&router(None)).await;
        assert!(
            ups.iter()
                .filter(|u| u.is_final)
                .all(|u| u.segment.text == "xin chào" && u.segment.lang == Some(LangTag::vi()))
        );
    }
}
