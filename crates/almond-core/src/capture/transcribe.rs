//! `transcribe.local`: the meeting's audio tracks → a `transcript` artifact.
//!
//! - Engines from [`super::engines::Speech::offline`] (routing table for this device class;
//!   language ID per utterance between the allowed languages, default vi + en).
//! - Mic = "me" (channel prior); the system / mixed channel is diarized.
//! - Diarization runs **concurrently** with transcription
//!   (`almond_stt::router::transcribe_meeting_concurrent`) and uses the silence-compacted,
//!   chunk-parallel sherpa diarizer (`almond_stt::diarize`).
//! - Builds without the `whisper`/`sherpa` features fail the job with `Unsupported`.
//!
//! Job params (all optional): `languages: ["vi","en"]`, `num_speakers: n` (remote speakers).

use super::engines::Speech;
use crate::Core;
use almond_audio::recorder::{Codec, decode_track};
use almond_stt::models::ModelManager;
use almond_stt::router::{MeetingAudio, transcribe_meeting_concurrent};
use almond_types::artifact::body::Transcript;
use almond_types::artifact::{ArtifactDraft, ArtifactKind, Provenance, params_hash};
use almond_types::job::Capability;
use almond_types::meeting::{AudioChannel, AudioTrack};
use almond_types::pipeline::{JobCtx, Processor, ProcessorDescriptor};
use almond_types::store::BlobStore;
use almond_types::{Artifact, Error, LangTag, Result};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

pub const TRANSCRIBE_LOCAL: &str = "transcribe.local";

/// Capability a device must offer to run `transcribe.local` (`stt:<lang>` satisfies it).
pub const REQUIRES: &str = "stt";

pub struct TranscribeLocal {
    speech: Arc<Speech>,
}

impl TranscribeLocal {
    pub(crate) fn new(speech: Arc<Speech>) -> Self {
        TranscribeLocal { speech }
    }
}

/// What this build + models dir can do: `stt:<lang>` per language whose offline model is
/// compiled in and installed, `diarize` when the diarization models are usable. For the
/// processing module's capability list (the `transcribe.local` job requires `stt`).
pub fn speech_capabilities(core: &Core) -> Vec<Capability> {
    let Ok(mgr) = ModelManager::new(&core.cfg.models_dir) else {
        return vec![];
    };
    let usable = |id: &str| {
        mgr.entry(id)
            .map(|e| super::engines::engine_compiled(&e.engine) && mgr.is_installed(id))
            .unwrap_or(false)
    };
    let t = core.capture.speech.table();
    let mut caps = vec![];
    for l in [LangTag::vi(), LangTag::en()] {
        if usable(&t.route(Some(&l)).offline) {
            caps.push(Capability::new(format!("stt:{}", l.0)));
        }
    }
    if cfg!(feature = "sherpa")
        && usable("sherpa:pyannote-seg-3.0")
        && usable("sherpa:3dspeaker-eres2net-base")
    {
        caps.push(Capability::new("diarize"));
    }
    caps
}

fn codec_of(t: &AudioTrack) -> Result<Codec> {
    match t.codec.as_str() {
        "opus" => Ok(Codec::Opus),
        "wav" => Ok(Codec::Wav),
        other => Err(Error::Unsupported(format!("audio codec {other}"))),
    }
}

/// Decode one track (blocking).
fn decode(blobs: &dyn BlobStore, t: &AudioTrack) -> Result<Vec<f32>> {
    let codec = codec_of(t)?;
    if let Some(p) = blobs.path(&t.blob)? {
        return decode_track(&p, codec);
    }
    let bytes = blobs.get(&t.blob)?.ok_or_else(|| {
        Error::NotFound(format!(
            "audio {} is not on this device (sync it first)",
            t.blob.0
        ))
    })?;
    let tmp = std::env::temp_dir().join(format!(
        "almond-track-{}-{}.{}",
        std::process::id(),
        t.blob.0,
        t.codec
    ));
    std::fs::write(&tmp, bytes)?;
    let r = decode_track(&tmp, codec);
    let _ = std::fs::remove_file(&tmp);
    r
}

/// Decode a meeting's tracks into one 16 kHz timeline per channel, starting at the earliest
/// track's offset (returned, ms from `Meeting::started_at`).
pub fn meeting_audio(blobs: &dyn BlobStore, tracks: &[AudioTrack]) -> Result<(MeetingAudio, u32)> {
    let base = tracks.iter().map(|t| t.offset_ms).min().unwrap_or(0);
    let mut audio = MeetingAudio::default();
    let mut sorted: Vec<&AudioTrack> = tracks.iter().collect();
    sorted.sort_by_key(|t| t.offset_ms);
    for t in sorted {
        let x = decode(blobs, t)?;
        let slot = match t.channel {
            AudioChannel::Mic => &mut audio.mic,
            AudioChannel::System => &mut audio.system,
            AudioChannel::Mixed => &mut audio.mixed,
        };
        let buf = slot.get_or_insert_with(Vec::new);
        let at = (t.offset_ms - base) as usize * 16;
        if buf.len() < at {
            buf.resize(at, 0.0);
        }
        // Overlapping tracks of one channel (shouldn't happen): the later one wins.
        let end = at + x.len();
        if buf.len() < end {
            buf.resize(end, 0.0);
        }
        buf[at..end].copy_from_slice(&x);
    }
    Ok((audio, base))
}

fn shift(t: &mut Transcript, ms: u32) {
    if ms == 0 {
        return;
    }
    for s in &mut t.segments {
        s.t0_ms = s.t0_ms.saturating_add(ms);
        s.t1_ms = s.t1_ms.saturating_add(ms);
        if let Some(ws) = s.words.as_mut() {
            for w in ws {
                w.t0_ms = w.t0_ms.saturating_add(ms);
                w.t1_ms = w.t1_ms.saturating_add(ms);
            }
        }
    }
}

fn languages(ctx: &JobCtx) -> Vec<LangTag> {
    let from_params: Option<Vec<LangTag>> = ctx
        .params
        .get("languages")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    match from_params {
        Some(v) if !v.is_empty() => v,
        _ => {
            // Always allow vi + en (meetings switch languages); meeting languages first.
            let mut v = ctx.meeting.languages.clone();
            for l in [LangTag::vi(), LangTag::en()] {
                if !v.iter().any(|x| x.primary() == l.primary()) {
                    v.push(l);
                }
            }
            v
        }
    }
}

#[async_trait]
impl Processor for TranscribeLocal {
    fn descriptor(&self) -> ProcessorDescriptor {
        ProcessorDescriptor {
            name: TRANSCRIBE_LOCAL.into(),
            version: almond_stt::VERSION.into(),
            consumes: vec![],
            produces: vec![ArtifactKind::TRANSCRIPT.into()],
            needs_audio: true,
            requires: vec![Capability::new(REQUIRES)],
            description:
                "Transcribe the meeting's audio on this device (whisper.cpp / sherpa-onnx), \
                          mic = me, diarized remote speakers"
                    .into(),
        }
    }

    async fn run(&self, ctx: &JobCtx, _inputs: Vec<Artifact>) -> Result<Vec<ArtifactDraft>> {
        let tracks = ctx.meeting.audio.clone();
        if tracks.is_empty() {
            return Err(Error::Invalid("the meeting has no audio".into()));
        }
        (ctx.progress)(0.02, "decoding audio");
        let blobs = ctx.blobs.clone();
        let (audio, base_ms) =
            tokio::task::spawn_blocking(move || meeting_audio(blobs.as_ref(), &tracks))
                .await
                .map_err(Error::other)??;
        ctx.check_cancel()?;

        (ctx.progress)(0.08, "loading speech models");
        let langs = languages(ctx);
        let speech = self.speech.clone();
        let (setup, diarizer) =
            tokio::task::spawn_blocking(move || (speech.offline(&langs), speech.diarizer()))
                .await
                .map_err(Error::other)?;
        let setup = setup?;
        ctx.check_cancel()?;

        let num_speakers = ctx
            .params
            .get("num_speakers")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        (ctx.progress)(
            0.15,
            if diarizer.is_some() {
                "transcribing and identifying speakers"
            } else {
                "transcribing"
            },
        );
        let run = transcribe_meeting_concurrent(
            setup.engine.clone(),
            diarizer.clone(),
            Arc::new(audio),
            &setup.opts,
            num_speakers,
        );
        let cancel = ctx.cancel.clone();
        let cancelled = async move {
            while !cancel.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        };
        let (mut transcript, timings) = tokio::select! {
            r = run => r?,
            _ = cancelled => return Err(Error::Cancelled),
        };
        shift(&mut transcript, base_ms);
        tracing::info!(
            meeting = %ctx.meeting.id,
            segments = transcript.segments.len(),
            speakers = transcript.speakers.len(),
            ?timings,
            "transcribed"
        );
        (ctx.progress)(
            0.98,
            &format!(
                "saving transcript (transcribed mic {:.0} s, system {:.0} s, mixed {:.0} s; \
                 diarized in parallel {:.0} s; total {:.0} s)",
                timings.transcribe_mic_ms as f64 / 1000.0,
                timings.transcribe_system_ms as f64 / 1000.0,
                timings.transcribe_mixed_ms as f64 / 1000.0,
                timings.diarize_ms as f64 / 1000.0,
                timings.total_ms as f64 / 1000.0
            ),
        );
        let model = format!(
            "{}{}",
            setup.models,
            if diarizer.is_some() {
                ",diarize=sherpa:3dspeaker-eres2net-base(fast)"
            } else {
                ""
            }
        );
        let draft = ArtifactDraft::new(
            ctx.meeting.id.clone(),
            &transcript,
            Provenance {
                processor: TRANSCRIBE_LOCAL.into(),
                processor_version: almond_stt::VERSION.into(),
                model: Some(model),
                params_hash: Some(params_hash(TRANSCRIBE_LOCAL, &ctx.params)),
                inputs: vec![],
            },
        )?;
        Ok(vec![draft])
    }
}
