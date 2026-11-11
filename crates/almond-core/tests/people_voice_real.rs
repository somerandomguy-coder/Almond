//! Real 3D-Speaker voice prints (ignored; needs `--features sherpa` and the installed model):
//! cosine similarity of voice prints of the same speaker vs different speakers, as used to pick
//! `people::DEFAULT_VOICE_THRESHOLD` (results in docs/people-voice.md).
//!
//! Clips come from `ALMOND_VOICE_CLIPS`: `;`-separated `speaker=path` entries (WAV 16 kHz mono
//! or Ogg Opus, e.g. tracks from the app's blob store). Each clip's speech is cut into ~3 s
//! windows (energy VAD) and split into two halves; each half becomes one print the way the app
//! makes them (mean of L2-normalised window embeddings, normalised). Prints of the same speaker
//! (halves of one clip, and across clips) are compared with prints of different speakers.
//!
//! ```text
//! set ALMOND_VOICE_CLIPS=nam=C:\...\mic1.opus;nam=C:\...\mic2.opus;tts-david=C:\...\david.wav
//! crates\almond-stt\scripts\cargo-cuda.cmd test -p almond-core --features almond-core/cuda,almond-core/sherpa --test people_voice_real -- --ignored --nocapture
//! ```
#![cfg(feature = "sherpa")]

use almond_audio::recorder::{Codec, decode_track};
use almond_core::capture::voice::{cosine, mean_embedding, native_embedder};
use almond_stt::diarize::{FastConfig, windows};
use std::path::Path;

struct Print {
    speaker: String,
    name: String,
    vector: Vec<f32>,
    speech_s: f32,
}

#[test]
#[ignore = "needs the 3D-Speaker model and ALMOND_VOICE_CLIPS"]
fn same_vs_different_speaker_similarity() {
    let clips = std::env::var("ALMOND_VOICE_CLIPS").expect("set ALMOND_VOICE_CLIPS");
    let emb = native_embedder(&almond_stt::models::default_models_dir()).expect("embedder");
    let cfg = FastConfig::default();
    let mut prints: Vec<Print> = vec![];
    for (i, entry) in clips
        .split(';')
        .filter(|s| !s.trim().is_empty())
        .enumerate()
    {
        let (speaker, path) = entry.split_once('=').expect("speaker=path");
        let codec = if path.to_lowercase().ends_with(".wav") {
            Codec::Wav
        } else {
            Codec::Opus
        };
        let x = decode_track(Path::new(path.trim()), codec).expect("decode");
        let w: Vec<&[f32]> = windows(&x, &cfg)
            .into_iter()
            .filter(|(a, b)| b - a >= 16_000)
            .map(|(a, b)| &x[a..b])
            .collect();
        let speech: f32 = w.iter().map(|p| p.len() as f32 / 16_000.0).sum();
        println!(
            "clip {i} {speaker}: {:.1} s audio, {:.1} s speech in {} windows",
            x.len() as f32 / 16_000.0,
            speech,
            w.len()
        );
        let half = w.len() / 2;
        for (k, part) in [&w[..half], &w[half..]].into_iter().enumerate() {
            match mean_embedding(emb.as_ref(), part, 3.0).unwrap() {
                Some(v) => prints.push(Print {
                    speaker: speaker.trim().into(),
                    name: format!("{}#{i}{}", speaker.trim(), ["a", "b"][k]),
                    vector: v.vector,
                    speech_s: v.speech_s,
                }),
                None => println!("  half {k}: too little speech"),
            }
        }
    }
    println!();
    print!("{:>16}", "");
    for p in &prints {
        print!("{:>14}", p.name);
    }
    println!();
    let (mut same, mut diff) = (vec![], vec![]);
    for (i, a) in prints.iter().enumerate() {
        print!("{:>16}", format!("{} ({:.0}s)", a.name, a.speech_s));
        for (j, b) in prints.iter().enumerate() {
            let c = cosine(&a.vector, &b.vector);
            print!("{c:>14.3}");
            if j > i {
                if a.speaker == b.speaker {
                    same.push(c);
                } else {
                    diff.push(c);
                }
            }
        }
        println!();
    }
    let stats = |v: &[f32]| {
        if v.is_empty() {
            return "none".to_string();
        }
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        let min = v.iter().cloned().fold(f32::INFINITY, f32::min);
        let max = v.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        format!("n={} min={min:.3} mean={mean:.3} max={max:.3}", v.len())
    };
    println!("\nsame speaker:      {}", stats(&same));
    println!("different speaker: {}", stats(&diff));
}
