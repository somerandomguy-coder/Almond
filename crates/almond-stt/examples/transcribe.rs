//! Transcribe a meeting WAV into a `transcript` artifact body (JSON), with speakers.
//!
//! ```text
//! cargo run --release -p almond-stt --features whisper,sherpa --example transcribe -- \
//!     meeting.wav [--stereo-mic-system] [--vi MODEL] [--en MODEL] [--lid MODEL] \
//!     [--langs vi,en] [--speakers N] [--no-diarize] [--cpu] [--out transcript.json]
//! ```
//! * mono WAV -> mixed channel, diarized as a whole;
//! * `--stereo-mic-system`: left = mic ("me"), right = system loopback (diarized remote side).
//!
//! Defaults: vi -> whisper:phowhisper-large, en/other -> whisper:large-v3-turbo (also used for
//! language ID). Prints timing (RTF) to stderr.

use almond_stt::models::ModelManager;
use almond_stt::router::{LanguageId, MeetingAudio, RoutedEngine, transcribe_meeting};
use almond_stt::transcript::{ChannelSegments, build_transcript};
use almond_types::LangTag;
use almond_types::artifact::body::Transcript;
use almond_types::meeting::AudioChannel;
use almond_types::stt::{Diarizer, SttEngine, SttOptions};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let mut wav = None;
    let mut stereo = false;
    let mut vi = "whisper:phowhisper-large".to_string();
    let mut en = "whisper:large-v3-turbo".to_string();
    let mut lid: Option<String> = None;
    let mut langs = vec![LangTag::vi(), LangTag::en()];
    let mut speakers: Option<u32> = None;
    let mut diarize = true;
    let mut cpu = false;
    let mut out: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--stereo-mic-system" => stereo = true,
            "--vi" => vi = args.next().unwrap(),
            "--en" => en = args.next().unwrap(),
            "--lid" => lid = args.next(),
            "--langs" => {
                langs = args
                    .next()
                    .unwrap()
                    .split(',')
                    .map(|s| LangTag(s.into()))
                    .collect()
            }
            "--speakers" => speakers = Some(args.next().unwrap().parse().unwrap()),
            "--no-diarize" => diarize = false,
            "--cpu" => cpu = true,
            "--out" => out = args.next(),
            _ => wav = Some(a),
        }
    }
    let wav = wav.expect("usage: transcribe <file.wav> [options]");
    let lid = lid.unwrap_or_else(|| en.clone());
    let mgr = ModelManager::open_default().unwrap();

    let t0 = Instant::now();
    let (chans, sr) = almond_stt::audio::load_wav_channels(&wav).unwrap();
    let to16 = |x: &Vec<f32>| almond_stt::audio::resample_linear(x, sr, 16_000);
    let audio = if stereo && chans.len() >= 2 {
        MeetingAudio {
            mic: Some(to16(&chans[0])),
            system: Some(to16(&chans[1])),
            mixed: None,
        }
    } else {
        MeetingAudio {
            mixed: Some(to16(&almond_stt::audio::downmix(&chans))),
            ..Default::default()
        }
    };
    let dur_s = audio
        .mixed
        .as_ref()
        .or(audio.mic.as_ref())
        .map(|x| x.len() as f64 / 16_000.0)
        .unwrap_or(0.0);

    let load = |id: &str| -> (Arc<dyn SttEngine>, Option<Arc<dyn LanguageId>>) {
        let e = mgr.entry(id).unwrap();
        match e.engine.as_str() {
            #[cfg(feature = "whisper")]
            "whisper" => {
                let mut cfg = almond_stt::whisper::WhisperConfig::default();
                if cpu {
                    cfg.use_gpu = false;
                    cfg.flash_attn = false;
                }
                let m = Arc::new(
                    almond_stt::whisper::WhisperModel::from_manager(&mgr, id, cfg).unwrap(),
                );
                (
                    Arc::new(almond_stt::whisper::WhisperEngine::new(m.clone())),
                    Some(m),
                )
            }
            #[cfg(feature = "sherpa")]
            "sherpa-transducer" => {
                let m = Arc::new(
                    almond_stt::sherpa::ZipformerModel::from_manager(&mgr, id, Default::default())
                        .unwrap(),
                );
                (Arc::new(almond_stt::sherpa::SherpaEngine::new(m)), None)
            }
            other => panic!("engine {other} not compiled in"),
        }
    };
    let (vi_engine, _) = load(&vi);
    let (en_engine, en_lid) = load(&en);
    let lid_model: Arc<dyn LanguageId> = if lid == en {
        en_lid.expect("--lid must be a whisper model")
    } else {
        load(&lid).1.expect("--lid must be a whisper model")
    };
    let mut engines = BTreeMap::new();
    engines.insert("vi".to_string(), vi_engine);
    engines.insert("en".to_string(), en_engine.clone());
    let routed = RoutedEngine {
        lid: lid_model,
        engines,
        fallback: en_engine,
    };
    let t_loaded = Instant::now();

    #[cfg(feature = "sherpa")]
    let diarizer: Option<Box<dyn Diarizer>> = if diarize {
        Some(Box::new(
            almond_stt::sherpa::SherpaDiarizer::from_manager(&mgr, Default::default()).unwrap(),
        ))
    } else {
        None
    };
    #[cfg(not(feature = "sherpa"))]
    let diarizer: Option<Box<dyn Diarizer>> = {
        let _ = diarize;
        None
    };

    let opts = SttOptions {
        allowed_languages: langs,
        word_timestamps: true,
        ..Default::default()
    };
    // Same steps as `router::transcribe_meeting`, unrolled to log per-stage timing.
    let _ = transcribe_meeting; // (library entry point used by the app)
    let mut cs = ChannelSegments::default();
    let stage = |name: &str, t: Instant| eprintln!("  {name}: {:.1} s", t.elapsed().as_secs_f64());
    for (ch, x) in [
        (AudioChannel::Mic, &audio.mic),
        (AudioChannel::System, &audio.system),
        (AudioChannel::Mixed, &audio.mixed),
    ] {
        let Some(x) = x else { continue };
        let t = Instant::now();
        let segs = routed.transcribe(x, 0, ch, &opts).await.unwrap();
        stage(&format!("transcribe {ch:?} ({} segments)", segs.len()), t);
        let turns = match &diarizer {
            Some(d) if ch != AudioChannel::Mic && !segs.is_empty() => {
                let t = Instant::now();
                let turns = d.diarize(x, speakers).await.unwrap();
                stage(&format!("diarize {ch:?} ({} turns)", turns.len()), t);
                Some(turns)
            }
            _ => None,
        };
        match ch {
            AudioChannel::Mic => cs.mic = segs,
            AudioChannel::System => (cs.system, cs.system_turns) = (segs, turns),
            AudioChannel::Mixed => (cs.mixed, cs.mixed_turns) = (segs, turns),
        }
    }
    let t: Transcript = build_transcript(cs);
    let t_done = Instant::now();

    let json = serde_json::to_string_pretty(&t).unwrap();
    match out {
        Some(p) => std::fs::write(p, json).unwrap(),
        None => println!("{json}"),
    }
    let proc_s = (t_done - t_loaded).as_secs_f64();
    eprintln!(
        "audio {:.1} s | load {:.1} s | transcribe+diarize {:.1} s | RTF {:.3} | {} segments, {} speakers",
        dur_s,
        (t_loaded - t0).as_secs_f64(),
        proc_s,
        proc_s / dur_s.max(1e-9),
        t.segments.len(),
        t.speakers.len()
    );
}
