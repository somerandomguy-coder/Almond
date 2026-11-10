//! Per-segment language routing for mixed-language meetings (ARCHITECTURE §10).
//!
//! [`RoutedEngine`] splits audio into utterances (energy VAD), identifies each utterance's
//! language (restricted to `allowed_languages`, e.g. vi/en) with a [`LanguageId`] model, packs
//! consecutive same-language utterances into <= 28 s windows, and sends each window to the engine
//! routed for that language (vi -> PhoWhisper / Zipformer, en -> whisper-turbo, ...).

use crate::audio::{VadParams, ms_to_samples, pack_windows, samples_to_ms, split_on_silence};
use crate::langid::{LidObs, LidPolicy, decide};
use crate::transcript::{ChannelSegments, build_transcript};
use almond_types::artifact::body::{Segment, Transcript};
use almond_types::meeting::AudioChannel;
use almond_types::stt::{Diarizer, SttEngine, SttModelInfo, SttOptions, SttStream};
use almond_types::{Error, LangTag, Result};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Spoken-language identification on a short (<= 30 s) window.
pub trait LanguageId: Send + Sync {
    /// Most likely language among `allowed` (any language if empty) and its probability.
    fn identify(&self, samples: &[f32], allowed: &[LangTag]) -> Result<(LangTag, f32)>;

    /// Probabilities of the `allowed` languages (renormalized), most likely first. The default
    /// spreads `1 - p(best)` evenly over the other allowed languages.
    fn probabilities(&self, samples: &[f32], allowed: &[LangTag]) -> Result<Vec<(LangTag, f32)>> {
        let (best, p) = self.identify(samples, allowed)?;
        let others: Vec<&LangTag> = allowed
            .iter()
            .filter(|l| l.primary() != best.primary())
            .collect();
        let rest = if others.is_empty() {
            0.0
        } else {
            (1.0 - p).max(0.0) / others.len() as f32
        };
        let mut v = vec![(best, p)];
        v.extend(others.into_iter().map(|l| (LangTag(l.primary()), rest)));
        Ok(v)
    }
}

#[cfg(feature = "whisper")]
impl LanguageId for crate::whisper::WhisperModel {
    fn identify(&self, samples: &[f32], allowed: &[LangTag]) -> Result<(LangTag, f32)> {
        let mut st = self.create_state()?;
        self.identify_language(&mut st, samples, allowed)
    }

    fn probabilities(&self, samples: &[f32], allowed: &[LangTag]) -> Result<Vec<(LangTag, f32)>> {
        let mut st = self.create_state()?;
        self.language_probs(&mut st, samples, allowed)
    }
}

/// Routes windows to per-language engines.
pub struct RoutedEngine {
    pub lid: Arc<dyn LanguageId>,
    /// Primary language subtag -> engine.
    pub engines: BTreeMap<String, Arc<dyn SttEngine>>,
    pub fallback: Arc<dyn SttEngine>,
    /// How LID probabilities become a language per utterance.
    pub policy: LidPolicy,
    /// A segment decoded in a secondary language with a lower mean word probability than this
    /// (or a secondary-language window that decodes to nothing) is re-decoded in the primary
    /// language (the first allowed one). `None` = trust the language decision.
    pub verify_min_confidence: Option<f32>,
}

/// Default for [`RoutedEngine::verify_min_confidence`] (eval/stt/RESULTS.md).
pub const VERIFY_MIN_CONFIDENCE: f32 = 0.4;

/// Fewer words than this (per 10 s of speech) in a secondary-language window means the
/// decoder did not understand it (conversational English is ~25 words / 10 s).
const MIN_WORDS_PER_10S: u64 = 7;

impl RoutedEngine {
    pub fn new(
        lid: Arc<dyn LanguageId>,
        engines: BTreeMap<String, Arc<dyn SttEngine>>,
        fallback: Arc<dyn SttEngine>,
    ) -> Self {
        RoutedEngine {
            lid,
            engines,
            fallback,
            policy: LidPolicy::default(),
            verify_min_confidence: Some(VERIFY_MIN_CONFIDENCE),
        }
    }

    fn engine_for(&self, lang: &LangTag) -> &Arc<dyn SttEngine> {
        self.engines.get(&lang.primary()).unwrap_or(&self.fallback)
    }
}

/// Utterances (VAD regions) of `samples` with a language each, decided by `policy` from the
/// LID probabilities of every region (see [`crate::langid`]). `allowed` = candidate languages,
/// primary first (empty: Whisper's top guess per region, no smoothing).
pub fn identify_regions(
    lid: &dyn LanguageId,
    samples: &[f32],
    allowed: &[LangTag],
    policy: &LidPolicy,
) -> Result<Vec<(usize, usize, LangTag)>> {
    // Short min-silence: speaker/language changes in conversation often have gaps of only
    // 250-500 ms; merging them made English turns get routed (and lost) as Vietnamese on the
    // synthetic meeting. Engines re-split with their own VAD.
    let vad = VadParams {
        min_silence_ms: 200,
        ..VadParams::default()
    };
    let regions = split_on_silence(samples, &vad);
    if allowed.is_empty() {
        return regions
            .into_iter()
            .map(|(a, b)| Ok((a, b, lid.identify(&samples[a..b], allowed)?.0)))
            .collect();
    }
    let mut langs: Vec<LangTag> = vec![];
    for l in allowed {
        if !langs.iter().any(|x| x.primary() == l.primary()) {
            langs.push(LangTag(l.primary()));
        }
    }
    let mut obs = Vec::with_capacity(regions.len());
    let mut prev_end = 0usize;
    for &(a, b) in &regions {
        // LID needs no padding-sized decision for sub-evidence regions, but the call is cheap
        // next to decoding and keeps the debug output complete.
        let probs = lid.probabilities(&samples[a..b], &langs)?;
        obs.push(LidObs {
            dur_ms: samples_to_ms(b - a),
            gap_ms: samples_to_ms(a.saturating_sub(prev_end)),
            probs,
        });
        prev_end = b;
    }
    let decided = decide(&obs, &langs, policy);
    if std::env::var_os("ALMOND_STT_DEBUG").is_some() {
        for ((a, b), (o, l)) in regions.iter().zip(obs.iter().zip(&decided)) {
            eprintln!(
                "[lid] {}..{} ms {:?} -> {}",
                samples_to_ms(*a),
                samples_to_ms(*b),
                o.probs,
                l.0
            );
        }
    }
    Ok(regions
        .into_iter()
        .zip(decided)
        .map(|((a, b), l)| (a, b, l))
        .collect())
}

#[async_trait]
impl SttEngine for RoutedEngine {
    fn model(&self) -> SttModelInfo {
        let mut langs: Vec<LangTag> = self.engines.keys().map(|k| LangTag(k.clone())).collect();
        langs.extend(self.fallback.model().languages);
        langs.sort();
        langs.dedup();
        let ids: Vec<String> = self
            .engines
            .values()
            .chain(std::iter::once(&self.fallback))
            .map(|e| e.model().id)
            .collect();
        SttModelInfo {
            id: format!("routed:{}", ids.join("+")),
            engine: "routed".into(),
            languages: langs,
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
        let mut out: Vec<Segment> = vec![];
        let windows: Vec<(usize, usize, LangTag)> = match &opts.language {
            Some(l) => pack_windows(samples)
                .into_iter()
                .map(|(a, b)| (a, b, l.clone()))
                .collect(),
            None => {
                // Language per utterance (VAD region), then pack same-language neighbours.
                let lid = self.lid.clone();
                let x = samples.to_vec();
                let allowed = opts.allowed_languages.clone();
                let policy = self.policy.clone();
                let regions = tokio::task::spawn_blocking(move || {
                    identify_regions(lid.as_ref(), &x, &allowed, &policy)
                })
                .await
                .map_err(Error::other)??;
                pack_same_language(regions)
            }
        };
        // Language-ID'd windows in a secondary language are checked against the decoder's
        // confidence; doubtful ones go to the primary language's engine instead.
        let primary = opts.allowed_languages.first().map(|l| LangTag(l.primary()));
        let verify = match (&opts.language, &primary, self.verify_min_confidence) {
            (None, Some(p), Some(min)) if self.engines.contains_key(&p.primary()) => {
                Some((p.clone(), min))
            }
            _ => None,
        };
        for (a, b, lang) in windows {
            let win = &samples[a..b];
            let base = t_offset_ms + samples_to_ms(a);
            let mut o = opts.clone();
            o.language = Some(lang.clone());
            let segs = self
                .engine_for(&lang)
                .transcribe(win, base, channel, &o)
                .await?;
            match &verify {
                Some((p, min)) if p.primary() != lang.primary() => {
                    let mut o = opts.clone();
                    o.language = Some(p.clone());
                    let primary_engine = self.engine_for(p).clone();
                    out.extend(
                        verify_segments(segs, win, base, *min, |x, off| {
                            let (e, o) = (primary_engine.clone(), o.clone());
                            async move { e.transcribe(&x, off, channel, &o).await }
                        })
                        .await?,
                    );
                }
                _ => out.extend(segs),
            }
        }
        for (i, s) in out.iter_mut().enumerate() {
            s.idx = i as u32;
        }
        Ok(out)
    }

    fn start_stream(&self, opts: SttOptions) -> Result<Box<dyn SttStream>> {
        let lang = opts
            .language
            .clone()
            .or_else(|| opts.allowed_languages.first().cloned());
        match lang {
            Some(l) => self.engine_for(&l).start_stream(opts),
            None => self.fallback.start_stream(opts),
        }
    }
}

/// Re-decode doubtful secondary-language segments with the primary engine (`redecode(audio,
/// t_offset_ms)`): segments whose confidence is below `min_conf`, or the whole window when the
/// secondary engine kept nothing (it dropped what it heard as a hallucination). `win` starts at
/// absolute `base_ms`.
async fn verify_segments<F, Fut>(
    segs: Vec<Segment>,
    win: &[f32],
    base_ms: u64,
    min_conf: f32,
    redecode: F,
) -> Result<Vec<Segment>>
where
    F: Fn(Vec<f32>, u64) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<Segment>>>,
{
    if segs.is_empty() {
        if has_speech(win) {
            return redecode(win.to_vec(), base_ms).await;
        }
        return Ok(segs);
    }
    // Far too few words for the speech in the window (Whisper told to expect English answered
    // 7 s of Vietnamese with "oh"): the whole window goes to the primary engine.
    let speech_ms: u64 = split_on_silence(win, &VadParams::default())
        .iter()
        .map(|(a, b)| samples_to_ms(b - a))
        .sum();
    let n_words: usize = segs.iter().map(|s| s.text.split_whitespace().count()).sum();
    if speech_ms >= 1500 && (n_words as u64) * 1000 < speech_ms * MIN_WORDS_PER_10S / 10 {
        if std::env::var_os("ALMOND_STT_DEBUG").is_some() {
            eprintln!("[route] {n_words} words for {speech_ms} ms of speech -> primary engine");
        }
        return redecode(win.to_vec(), base_ms).await;
    }
    let pad = 150u64;
    let win_end = base_ms + samples_to_ms(win.len());
    let mut out = vec![];
    let mut last_end = base_ms;
    for s in segs {
        if s.confidence.is_some_and(|c| c >= min_conf) || s.confidence.is_none() {
            last_end = last_end.max(s.t1_ms as u64);
            out.push(s);
            continue;
        }
        let a = (s.t0_ms as u64).saturating_sub(pad).max(base_ms).max(last_end);
        let b = (s.t1_ms as u64 + pad).min(win_end);
        if b <= a {
            continue;
        }
        let x = &win[ms_to_samples(a - base_ms)..ms_to_samples(b - base_ms).min(win.len())];
        if std::env::var_os("ALMOND_STT_DEBUG").is_some() {
            eprintln!(
                "[route] {}..{} ms: {:?} (conf {:?}) -> primary engine",
                a, b, s.text, s.confidence
            );
        }
        let r = redecode(x.to_vec(), a).await?;
        last_end = b;
        out.extend(r);
    }
    Ok(out)
}

/// Merge consecutive same-language regions into decode windows (<= 28 s, gaps <= 5 s).
pub fn pack_same_language(regions: Vec<(usize, usize, LangTag)>) -> Vec<(usize, usize, LangTag)> {
    let max = ms_to_samples(28_000);
    let max_gap = ms_to_samples(5_000);
    let mut out: Vec<(usize, usize, LangTag)> = vec![];
    for (a, b, l) in regions {
        match out.last_mut() {
            Some((s, e, pl)) if *pl == l && b - *s <= max && a.saturating_sub(*e) <= max_gap => {
                *e = b
            }
            _ => out.push((a, b, l)),
        }
    }
    out
}

/// Audio of one meeting, per channel (16 kHz mono f32, all starting at meeting t=0).
#[derive(Debug, Default, Clone)]
pub struct MeetingAudio {
    pub mic: Option<Vec<f32>>,
    pub system: Option<Vec<f32>>,
    pub mixed: Option<Vec<f32>>,
}

/// Full offline pipeline: transcribe each channel, diarize system/mixed, assemble transcript.
/// `num_speakers` is the expected number of **remote** speakers on the system channel (e.g.
/// calendar attendees - 1), or of all speakers for mixed audio.
pub async fn transcribe_meeting(
    engine: &dyn SttEngine,
    diarizer: Option<&dyn Diarizer>,
    audio: &MeetingAudio,
    opts: &SttOptions,
    num_speakers: Option<u32>,
) -> Result<Transcript> {
    let mut cs = ChannelSegments::default();
    if let Some(x) = &audio.mic {
        cs.mic = engine.transcribe(x, 0, AudioChannel::Mic, opts).await?;
    }
    if let Some(x) = &audio.system {
        cs.system = engine.transcribe(x, 0, AudioChannel::System, opts).await?;
        if let Some(d) = diarizer
            && !cs.system.is_empty()
        {
            cs.system_turns = Some(d.diarize(x, num_speakers).await?);
        }
    }
    if let Some(x) = &audio.mixed {
        cs.mixed = engine.transcribe(x, 0, AudioChannel::Mixed, opts).await?;
        if let Some(d) = diarizer
            && !cs.mixed.is_empty()
        {
            cs.mixed_turns = Some(d.diarize(x, num_speakers).await?);
        }
    }
    Ok(build_transcript(cs))
}

/// Wall time of each stage of [`transcribe_meeting_concurrent`] (ms).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct MeetingTimings {
    pub transcribe_mic_ms: u64,
    pub transcribe_system_ms: u64,
    pub transcribe_mixed_ms: u64,
    pub diarize_ms: u64,
    pub total_ms: u64,
}

fn has_speech(x: &[f32]) -> bool {
    !split_on_silence(x, &VadParams::default()).is_empty()
}

/// [`transcribe_meeting`] with diarization of the system / mixed channel running **concurrently**
/// with transcription (on the 30-min test meeting diarization was ~80 % of the sequential time;
/// it is CPU-bound while Whisper runs on the GPU). Channels without speech are not diarized.
pub async fn transcribe_meeting_concurrent(
    engine: Arc<dyn SttEngine>,
    diarizer: Option<Arc<dyn Diarizer>>,
    audio: Arc<MeetingAudio>,
    opts: &SttOptions,
    num_speakers: Option<u32>,
) -> Result<(Transcript, MeetingTimings)> {
    let t_all = std::time::Instant::now();
    let diarize = {
        let audio = audio.clone();
        async move {
            let t = std::time::Instant::now();
            let mut out: (Option<Vec<_>>, Option<Vec<_>>) = (None, None);
            if let Some(d) = &diarizer {
                if let Some(x) = audio.system.as_deref()
                    && has_speech(x)
                {
                    out.0 = Some(d.diarize(x, num_speakers).await?);
                }
                if let Some(x) = audio.mixed.as_deref()
                    && has_speech(x)
                {
                    out.1 = Some(d.diarize(x, num_speakers).await?);
                }
            }
            Ok::<_, Error>((out, t.elapsed().as_millis() as u64))
        }
    };
    let transcribe = async {
        let mut cs = ChannelSegments::default();
        let mut ms = [0u64; 3];
        for (i, ch, x) in [
            (0, AudioChannel::Mic, &audio.mic),
            (1, AudioChannel::System, &audio.system),
            (2, AudioChannel::Mixed, &audio.mixed),
        ] {
            let Some(x) = x else { continue };
            let t = std::time::Instant::now();
            let segs = engine.transcribe(x, 0, ch, opts).await?;
            ms[i] = t.elapsed().as_millis() as u64;
            match ch {
                AudioChannel::Mic => cs.mic = segs,
                AudioChannel::System => cs.system = segs,
                AudioChannel::Mixed => cs.mixed = segs,
            }
        }
        Ok::<_, Error>((cs, ms))
    };
    let (((sys_turns, mixed_turns), diarize_ms), (mut cs, ms)) =
        tokio::try_join!(diarize, transcribe)?;
    if !cs.system.is_empty() {
        cs.system_turns = sys_turns;
    }
    if !cs.mixed.is_empty() {
        cs.mixed_turns = mixed_turns;
    }
    let t = build_transcript(cs);
    Ok((
        t,
        MeetingTimings {
            transcribe_mic_ms: ms[0],
            transcribe_system_ms: ms[1],
            transcribe_mixed_ms: ms[2],
            diarize_ms,
            total_ms: t_all.elapsed().as_millis() as u64,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::mock::MockStt;
    use almond_types::stt::SpeakerTurn;

    struct FixedLid(&'static str);
    impl LanguageId for FixedLid {
        fn identify(&self, _: &[f32], _: &[LangTag]) -> Result<(LangTag, f32)> {
            Ok((LangTag(self.0.into()), 1.0))
        }
    }

    struct TwoSpeakers;
    #[async_trait]
    impl Diarizer for TwoSpeakers {
        async fn diarize(&self, s: &[f32], _: Option<u32>) -> Result<Vec<SpeakerTurn>> {
            let end = samples_to_ms(s.len()) as u32;
            Ok(vec![SpeakerTurn {
                t0_ms: 0,
                t1_ms: end,
                speaker: almond_types::ids::SpeakerId("S1".into()),
            }])
        }
    }

    fn voice(ms: u64) -> Vec<f32> {
        (0..crate::audio::ms_to_samples(ms))
            .map(|i| {
                let t = i as f32 / 16_000.0;
                0.2 * (0.55 + 0.45 * (25.0 * t).sin()) * (i as f32 * 0.05).sin()
            })
            .collect()
    }

    #[tokio::test]
    async fn routes_by_identified_language() {
        let vi: Arc<dyn SttEngine> = Arc::new(MockStt::default());
        let en: Arc<dyn SttEngine> = Arc::new(MockStt {
            text: "hello".into(),
            lang: LangTag::en(),
        });
        let mut engines = BTreeMap::new();
        engines.insert("vi".to_string(), vi);
        engines.insert("en".to_string(), en.clone());
        let r = RoutedEngine::new(Arc::new(FixedLid("en")), engines, en);
        let segs = r
            .transcribe(
                &voice(3000),
                1000,
                AudioChannel::Mic,
                &SttOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "hello");
        assert!(segs[0].t0_ms >= 1000);
    }

    /// Whisper-like LID that calls everything English with probability `p_en`.
    struct EnLid(f32);
    impl LanguageId for EnLid {
        fn identify(&self, _: &[f32], _: &[LangTag]) -> Result<(LangTag, f32)> {
            Ok((LangTag::en(), self.0))
        }
    }

    /// One segment per call with a fixed text and confidence.
    struct Fixed(&'static str, f32);
    #[async_trait]
    impl SttEngine for Fixed {
        fn model(&self) -> SttModelInfo {
            MockStt::default().model()
        }
        async fn transcribe(
            &self,
            s: &[f32],
            t: u64,
            channel: AudioChannel,
            o: &SttOptions,
        ) -> Result<Vec<Segment>> {
            Ok(vec![Segment {
                idx: 0,
                t0_ms: t as u32,
                t1_ms: (t + samples_to_ms(s.len())) as u32,
                channel,
                speaker: None,
                lang: o.language.clone(),
                text: self.0.into(),
                confidence: Some(self.1),
                words: None,
            }])
        }
        fn start_stream(&self, o: SttOptions) -> Result<Box<dyn SttStream>> {
            MockStt::default().start_stream(o)
        }
    }

    fn vi_en(lid: f32, en_conf: f32) -> RoutedEngine {
        let mut engines: BTreeMap<String, Arc<dyn SttEngine>> = BTreeMap::new();
        engines.insert("vi".into(), Arc::new(Fixed("xin chào", 0.9)));
        let en: Arc<dyn SttEngine> = Arc::new(Fixed("hello there how are you doing today", en_conf));
        engines.insert("en".into(), en.clone());
        RoutedEngine::new(Arc::new(EnLid(lid)), engines, en)
    }

    fn vi_en_opts() -> SttOptions {
        SttOptions {
            allowed_languages: vec![LangTag::vi(), LangTag::en()],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn short_utterance_with_doubtful_lid_stays_primary() {
        // 0.8 s, Whisper says 90 % English: the prior wins
        let segs = vi_en(0.9, 0.9)
            .transcribe(&voice(800), 0, AudioChannel::Mic, &vi_en_opts())
            .await
            .unwrap();
        assert_eq!(segs[0].text, "xin chào");
        // the old behaviour routed it to English
        let mut raw = vi_en(0.9, 0.9);
        raw.policy = LidPolicy::raw();
        let segs = raw
            .transcribe(&voice(800), 0, AudioChannel::Mic, &vi_en_opts())
            .await
            .unwrap();
        assert_eq!(segs[0].text, "hello there how are you doing today");
    }

    #[tokio::test]
    async fn long_confident_english_is_english_unless_decoding_disagrees() {
        let segs = vi_en(0.99, 0.9)
            .transcribe(&voice(4000), 0, AudioChannel::Mic, &vi_en_opts())
            .await
            .unwrap();
        assert_eq!(segs[0].text, "hello there how are you doing today");
        assert_eq!(segs[0].lang, Some(LangTag::en()));
        // English decode with low confidence: re-decoded as Vietnamese
        let segs = vi_en(0.99, 0.3)
            .transcribe(&voice(4000), 0, AudioChannel::Mic, &vi_en_opts())
            .await
            .unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "xin chào");
        assert_eq!(segs[0].lang, Some(LangTag::vi()));
    }

    #[tokio::test]
    async fn one_word_for_seconds_of_speech_goes_to_the_primary_engine() {
        let mut engines: BTreeMap<String, Arc<dyn SttEngine>> = BTreeMap::new();
        engines.insert("vi".into(), Arc::new(Fixed("xin chào các bạn", 0.9)));
        let en: Arc<dyn SttEngine> = Arc::new(Fixed("oh", 0.97));
        engines.insert("en".into(), en.clone());
        let r = RoutedEngine::new(Arc::new(EnLid(0.99)), engines, en);
        let segs = r
            .transcribe(&voice(5000), 0, AudioChannel::Mic, &vi_en_opts())
            .await
            .unwrap();
        assert_eq!(segs[0].text, "xin chào các bạn");
    }

    #[test]
    fn packs_only_same_language_neighbours() {
        let s = |sec: usize| sec * 16_000;
        let w = pack_same_language(vec![
            (s(0), s(3), LangTag::vi()),
            (s(4), s(6), LangTag::vi()),
            (s(7), s(9), LangTag::en()),
            (s(10), s(12), LangTag::vi()),
        ]);
        let langs: Vec<_> = w.iter().map(|x| x.2.0.as_str()).collect();
        assert_eq!(langs, vec!["vi", "en", "vi"]);
        assert_eq!((w[0].0, w[0].1), (0, s(6)));
    }

    #[tokio::test]
    async fn meeting_pipeline_labels_channels() {
        let e = MockStt::default();
        let t = transcribe_meeting(
            &e,
            Some(&TwoSpeakers),
            &MeetingAudio {
                mic: Some(voice(2000)),
                system: Some(voice(2000)),
                mixed: None,
            },
            &SttOptions::default(),
            None,
        )
        .await
        .unwrap();
        // identical mock text on both channels at the same time => mic one is treated as echo
        assert_eq!(t.segments.len(), 1);
        assert_eq!(t.segments[0].speaker.as_ref().unwrap().0, "remote:S1");
    }

    #[tokio::test]
    async fn concurrent_pipeline_matches_sequential() {
        let audio = MeetingAudio {
            mic: Some(voice(2000)),
            system: Some(voice(3000)),
            mixed: None,
        };
        let seq = transcribe_meeting(
            &MockStt::default(),
            Some(&TwoSpeakers),
            &audio,
            &SttOptions::default(),
            None,
        )
        .await
        .unwrap();
        let (con, timings) = transcribe_meeting_concurrent(
            Arc::new(MockStt::default()),
            Some(Arc::new(TwoSpeakers)),
            Arc::new(audio),
            &SttOptions::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(seq, con);
        assert!(timings.total_ms >= timings.diarize_ms);
    }
}
