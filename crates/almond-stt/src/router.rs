//! Per-segment language routing for mixed-language meetings (ARCHITECTURE §10).
//!
//! [`RoutedEngine`] splits audio into <= 28 s VAD windows, identifies each window's language
//! (restricted to `allowed_languages`, e.g. vi/en) with a [`LanguageId`] model, and sends the
//! window to the engine routed for that language (vi -> PhoWhisper, en -> whisper-turbo, ...).

use crate::audio::{pack_windows, samples_to_ms};
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
}

#[cfg(feature = "whisper")]
impl LanguageId for crate::whisper::WhisperModel {
    fn identify(&self, samples: &[f32], allowed: &[LangTag]) -> Result<(LangTag, f32)> {
        let mut st = self.create_state()?;
        self.identify_language(&mut st, samples, allowed)
    }
}

/// Routes windows to per-language engines.
pub struct RoutedEngine {
    pub lid: Arc<dyn LanguageId>,
    /// Primary language subtag -> engine.
    pub engines: BTreeMap<String, Arc<dyn SttEngine>>,
    pub fallback: Arc<dyn SttEngine>,
}

impl RoutedEngine {
    fn engine_for(&self, lang: &LangTag) -> &Arc<dyn SttEngine> {
        self.engines.get(&lang.primary()).unwrap_or(&self.fallback)
    }
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
        for (a, b) in pack_windows(samples) {
            let win = &samples[a..b];
            let lang = match &opts.language {
                Some(l) => l.clone(),
                None => {
                    let lid = self.lid.clone();
                    let w = win.to_vec();
                    let allowed = opts.allowed_languages.clone();
                    tokio::task::spawn_blocking(move || lid.identify(&w, &allowed))
                        .await
                        .map_err(Error::other)??
                        .0
                }
            };
            let mut o = opts.clone();
            o.language = Some(lang.clone());
            let segs = self
                .engine_for(&lang)
                .transcribe(win, t_offset_ms + samples_to_ms(a), channel, &o)
                .await?;
            out.extend(segs);
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
            .map(|i| 0.2 * (i as f32 * 0.05).sin())
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
        let r = RoutedEngine {
            lid: Arc::new(FixedLid("en")),
            engines,
            fallback: en,
        };
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
}
