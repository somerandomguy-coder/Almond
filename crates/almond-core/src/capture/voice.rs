//! Voice prints: one speaker embedding per transcript speaker, for recognising people across
//! meetings (feature `people`, docs/FEATURES-2.md).
//!
//! - The embedding model is the diarization one already installed,
//!   `sherpa:3dspeaker-eres2net-base` (3D-Speaker ERes2Net, 16 kHz), through sherpa-onnx.
//! - A speaker's print = the duration-weighted mean of the L2-normalised embeddings of their
//!   segments (segments under [`MIN_SEGMENT_MS`] are skipped, long ones are cut into
//!   [`CHUNK_MS`] pieces, at most [`MAX_SPEECH_S`] of the longest segments are used), normalised
//!   again. Speakers with less than [`MIN_SPEECH_S`] of usable speech get no print.
//! - Audio comes from the meeting's blobs, decoded like `transcribe.local` does
//!   ([`super::transcribe::meeting_audio`]); segment times are relative to `Meeting::started_at`.
//! - Without the `sherpa` feature [`native_embedder`] returns `Unsupported`: speaker labels still
//!   work, just without voice matching.

use almond_stt::router::MeetingAudio;
use almond_types::artifact::body::Transcript;
use almond_types::ids::SpeakerId;
use almond_types::meeting::{AudioChannel, Meeting};
use almond_types::store::BlobStore;
use almond_types::{Error, Result};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// Model id stored in `VoicePrint::model`; only prints of the same model are compared.
pub const VOICE_MODEL: &str = "sherpa:3dspeaker-eres2net-base";
/// Segments shorter than this are not embedded (too little audio for a stable embedding).
pub const MIN_SEGMENT_MS: u32 = 1000;
/// Less speech than this in total: no print for that speaker.
pub const MIN_SPEECH_S: f32 = 5.0;
/// At most this much speech (longest segments first) goes into one print.
pub const MAX_SPEECH_S: f32 = 90.0;
/// Long segments are embedded in pieces of about this length.
pub const CHUNK_MS: u32 = 10_000;

const SR: usize = 16_000;

/// Speaker embeddings of 16 kHz mono audio. Blocking.
pub trait VoiceEmbedder: Send + Sync {
    /// Model id (see [`VOICE_MODEL`]).
    fn model(&self) -> String;
    /// Embedding of `samples` (not necessarily normalised). May fail for very short input.
    fn embed(&self, samples: &[f32]) -> Result<Vec<f32>>;
}

/// One speaker's voice in one meeting.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerVoice {
    pub model: String,
    /// L2-normalised.
    pub vector: Vec<f32>,
    /// Seconds of speech averaged into `vector`.
    pub speech_s: f32,
}

/// L2-normalise in place; false (and unchanged) for a zero or non-finite vector.
pub fn normalize(v: &mut [f32]) -> bool {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n == 0.0 || !n.is_finite() {
        return false;
    }
    v.iter_mut().for_each(|x| *x /= n);
    true
}

/// Cosine similarity (0 for mismatched or zero vectors).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut ab, mut aa, mut bb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa == 0.0 || bb == 0.0 {
        return 0.0;
    }
    ab / (aa.sqrt() * bb.sqrt())
}

/// Embed `pieces` (sample slices) and average them, weighted by length. `None` when nothing
/// could be embedded or the total is under `min_speech_s`.
pub fn mean_embedding(
    embedder: &dyn VoiceEmbedder,
    pieces: &[&[f32]],
    min_speech_s: f32,
) -> Result<Option<SpeakerVoice>> {
    let mut sum: Vec<f32> = vec![];
    let mut total = 0.0f32;
    for p in pieces {
        let mut e = match embedder.embed(p) {
            Ok(e) => e,
            Err(Error::Invalid(_)) => continue, // too short for the model
            Err(e) => return Err(e),
        };
        if !normalize(&mut e) {
            continue;
        }
        if sum.is_empty() {
            sum = vec![0.0; e.len()];
        } else if sum.len() != e.len() {
            return Err(Error::Other(format!(
                "embedding size changed ({} vs {})",
                e.len(),
                sum.len()
            )));
        }
        let w = p.len() as f32 / SR as f32;
        sum.iter_mut().zip(&e).for_each(|(s, x)| *s += w * x);
        total += w;
    }
    if total < min_speech_s || !normalize(&mut sum) {
        return Ok(None);
    }
    Ok(Some(SpeakerVoice {
        model: embedder.model(),
        vector: sum,
        speech_s: total,
    }))
}

fn channel_audio(audio: &MeetingAudio, ch: AudioChannel) -> Option<&[f32]> {
    match ch {
        AudioChannel::Mic => audio.mic.as_deref(),
        AudioChannel::System => audio.system.as_deref(),
        AudioChannel::Mixed => audio.mixed.as_deref(),
    }
}

/// The audio pieces (≤ [`CHUNK_MS`] each, longest segments first, ≤ [`MAX_SPEECH_S`] in total)
/// of one speaker. `base_ms` = offset of the audio timeline from `Meeting::started_at`.
pub fn speaker_pieces<'a>(
    audio: &'a MeetingAudio,
    base_ms: u32,
    transcript: &Transcript,
    speaker: &SpeakerId,
) -> Vec<&'a [f32]> {
    let mut segs: Vec<_> = transcript
        .segments
        .iter()
        .filter(|s| s.speaker.as_ref() == Some(speaker))
        .filter(|s| s.t1_ms.saturating_sub(s.t0_ms) >= MIN_SEGMENT_MS)
        .collect();
    segs.sort_by_key(|s| std::cmp::Reverse(s.t1_ms - s.t0_ms));
    let mut out = vec![];
    let mut total = 0usize;
    let max = (MAX_SPEECH_S * SR as f32) as usize;
    let chunk = CHUNK_MS as usize * SR / 1000;
    for s in segs {
        let Some(x) = channel_audio(audio, s.channel) else {
            continue;
        };
        let a = (s.t0_ms.saturating_sub(base_ms) as usize * SR / 1000).min(x.len());
        let b = (s.t1_ms.saturating_sub(base_ms) as usize * SR / 1000).min(x.len());
        if b.saturating_sub(a) < MIN_SEGMENT_MS as usize * SR / 1000 {
            continue;
        }
        // Equal pieces of at most `chunk` samples.
        let n = (b - a).div_ceil(chunk);
        let step = (b - a) / n;
        for i in 0..n {
            if total >= max {
                return out;
            }
            let s0 = a + i * step;
            let s1 = if i + 1 == n { b } else { s0 + step };
            out.push(&x[s0..s1]);
            total += s1 - s0;
        }
    }
    out
}

/// Voice prints of the given speakers (all of the transcript's when `only` is empty) from
/// decoded meeting audio. Speakers with too little speech are missing from the result.
pub fn speaker_voices(
    embedder: &dyn VoiceEmbedder,
    audio: &MeetingAudio,
    base_ms: u32,
    transcript: &Transcript,
    only: &[SpeakerId],
) -> Result<BTreeMap<SpeakerId, SpeakerVoice>> {
    let mut ids: Vec<SpeakerId> = vec![];
    for s in &transcript.segments {
        if let Some(id) = &s.speaker
            && !ids.contains(id)
            && (only.is_empty() || only.contains(id))
        {
            ids.push(id.clone());
        }
    }
    let mut out = BTreeMap::new();
    for id in ids {
        let pieces = speaker_pieces(audio, base_ms, transcript, &id);
        if let Some(v) = mean_embedding(embedder, &pieces, MIN_SPEECH_S)? {
            out.insert(id, v);
        }
    }
    Ok(out)
}

/// Decode a meeting's audio (blocking). `NotFound` when the audio is not on this device.
pub fn meeting_audio(blobs: &dyn BlobStore, meeting: &Meeting) -> Result<(MeetingAudio, u32)> {
    if meeting.audio.is_empty() {
        return Err(Error::NotFound(format!(
            "meeting {} has no audio",
            meeting.id
        )));
    }
    super::transcribe::meeting_audio(blobs, &meeting.audio)
}

/// The 3D-Speaker embedder from the models dir. `Unsupported` without the `sherpa` feature,
/// `NotFound` when the model is not installed.
pub fn native_embedder(models_dir: &Path) -> Result<Arc<dyn VoiceEmbedder>> {
    native::load(models_dir)
}

#[cfg(feature = "sherpa")]
mod native {
    use super::*;
    use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
    use std::sync::Mutex;

    pub struct SherpaVoiceEmbedder {
        ex: Mutex<SpeakerEmbeddingExtractor>,
    }

    pub fn load(models_dir: &Path) -> Result<Arc<dyn VoiceEmbedder>> {
        let mgr = almond_stt::models::ModelManager::new(models_dir)?;
        if !mgr.is_installed(VOICE_MODEL) {
            return Err(Error::NotFound(format!(
                "speech model {VOICE_MODEL} is not installed (download it under Settings → Models)"
            )));
        }
        let model = mgr.file(VOICE_MODEL, "model")?;
        let ex = SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
            model: Some(model.to_string_lossy().into_owned()),
            num_threads: std::thread::available_parallelism()
                .map(|n| n.get().min(4) as i32)
                .unwrap_or(2),
            ..Default::default()
        })
        .ok_or_else(|| {
            Error::Other("sherpa-onnx failed to create the embedding extractor".into())
        })?;
        Ok(Arc::new(SherpaVoiceEmbedder { ex: Mutex::new(ex) }))
    }

    impl VoiceEmbedder for SherpaVoiceEmbedder {
        fn model(&self) -> String {
            VOICE_MODEL.into()
        }

        fn embed(&self, samples: &[f32]) -> Result<Vec<f32>> {
            let ex = self.ex.lock().unwrap_or_else(|p| p.into_inner());
            let st = ex
                .create_stream()
                .ok_or_else(|| Error::Other("sherpa-onnx: embedding stream".into()))?;
            st.accept_waveform(SR as i32, samples);
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
}

#[cfg(not(feature = "sherpa"))]
mod native {
    use super::*;

    pub fn load(_models_dir: &Path) -> Result<Arc<dyn VoiceEmbedder>> {
        Err(Error::Unsupported(
            "voice prints need the sherpa speech engine; this build has none \
             (build with --features sherpa)"
                .into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::artifact::body::Segment;

    /// "Embedding" = the samples' mean and RMS: enough to tell constant signals apart.
    struct Stats;
    impl VoiceEmbedder for Stats {
        fn model(&self) -> String {
            "test:stats".into()
        }
        fn embed(&self, x: &[f32]) -> Result<Vec<f32>> {
            if x.len() < SR / 2 {
                return Err(Error::Invalid("short".into()));
            }
            let mean = x.iter().sum::<f32>() / x.len() as f32;
            Ok(vec![mean, 1.0])
        }
    }

    fn seg(idx: u32, t0: u32, t1: u32, ch: AudioChannel, spk: &str) -> Segment {
        Segment {
            idx,
            t0_ms: t0,
            t1_ms: t1,
            channel: ch,
            speaker: Some(SpeakerId(spk.into())),
            lang: None,
            text: String::new(),
            confidence: None,
            words: None,
        }
    }

    #[test]
    fn cosine_and_normalize() {
        assert!((cosine(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 3.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        let mut v = vec![3.0, 4.0];
        assert!(normalize(&mut v));
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!(!normalize(&mut [0.0, 0.0]));
    }

    #[test]
    fn pieces_skip_short_segments_cap_and_chunk() {
        // 40 s of system audio starting 1 s after the meeting start (base 1000 ms)
        let audio = MeetingAudio {
            system: Some(vec![0.5; 40 * SR]),
            mic: Some(vec![-0.5; 40 * SR]),
            ..Default::default()
        };
        let t = Transcript {
            segments: vec![
                seg(0, 1_000, 1_500, AudioChannel::System, "S1"), // 0.5 s: skipped
                seg(1, 2_000, 27_000, AudioChannel::System, "S1"), // 25 s: 3 chunks
                seg(2, 30_000, 33_000, AudioChannel::Mic, "me"),
            ],
            speakers: vec![],
        };
        let p = speaker_pieces(&audio, 1_000, &t, &SpeakerId("S1".into()));
        assert_eq!(p.len(), 3);
        assert_eq!(p.iter().map(|x| x.len()).sum::<usize>(), 25 * SR);
        assert!(p.iter().all(|x| x.len() <= CHUNK_MS as usize * SR / 1000));

        let v = speaker_voices(&Stats, &audio, 1_000, &t, &[]).unwrap();
        // "me" has 3 s < MIN_SPEECH_S: no print
        assert_eq!(v.len(), 1);
        let s1 = &v[&SpeakerId("S1".into())];
        assert!((s1.speech_s - 25.0).abs() < 0.01);
        assert!((s1.vector[0] / s1.vector[1] - 0.5).abs() < 1e-4); // (0.5, 1), normalised
        let only_me = speaker_voices(&Stats, &audio, 1_000, &t, &[SpeakerId("me".into())]);
        assert!(only_me.unwrap().is_empty());
    }
}
