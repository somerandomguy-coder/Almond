//! Diarize one channel of a WAV with sherpa-onnx models (3D-Speaker embeddings; pyannote-seg-3.0
//! for the `--pyannote` path).
//!
//! ```text
//! cargo run --release -p almond-stt --features sherpa --example diarize -- file.wav \
//!     [--channel 1] [--speakers N] [--out turns.json]
//!     [--pyannote [--threshold 1.0]] [--fast-threshold 0.6] [--window-ms 3000] [--workers 4]
//! ```
//! Default: the fast window-embedding diarizer (`almond_stt::diarize::FastDiarizer`).
//! `--pyannote` (or `--speakers N`) runs sherpa's pyannote pipeline over the whole recording.

use almond_stt::models::ModelManager;
use almond_stt::sherpa::{DiarizerConfig, SherpaDiarizer};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut wav, mut ch, mut n, mut thr, mut out) = (
        String::new(),
        None,
        None,
        DiarizerConfig::default().threshold,
        None,
    );
    let mut threads = None;
    let mut fast = Some(almond_stt::diarize::FastConfig::default());
    while let Some(a) = args.next() {
        let num = |v: Option<String>| -> f64 { v.unwrap().parse().unwrap() };
        match a.as_str() {
            "--fast-threshold" => {
                let v = num(args.next());
                if let Some(c) = fast.as_mut() {
                    c.threshold = v as f32
                }
            }
            "--window-ms" => {
                let v = num(args.next());
                if let Some(c) = fast.as_mut() {
                    c.window_ms = v as u64
                }
            }
            "--workers" => {
                let v = num(args.next());
                if let Some(c) = fast.as_mut() {
                    c.workers = v as usize
                }
            }
            "--min-speaker-ms" => {
                let v = num(args.next());
                if let Some(c) = fast.as_mut() {
                    c.min_speaker_ms = v as u64
                }
            }
            "--pyannote" => fast = None,
            "--channel" => ch = Some(args.next().unwrap().parse::<usize>().unwrap()),
            "--speakers" => n = Some(args.next().unwrap().parse::<u32>().unwrap()),
            "--threshold" => thr = args.next().unwrap().parse().unwrap(),
            "--threads" => threads = Some(args.next().unwrap().parse::<i32>().unwrap()),
            "--out" => out = args.next(),
            _ => wav = a,
        }
    }
    let (chans, sr) = almond_stt::audio::load_wav_channels(&wav).unwrap();
    let x = match ch {
        Some(c) => chans[c].clone(),
        None => almond_stt::audio::downmix(&chans),
    };
    let x = almond_stt::audio::resample_linear(&x, sr, 16_000);
    let mgr = ModelManager::open_default().unwrap();
    let d = SherpaDiarizer::from_manager(
        &mgr,
        DiarizerConfig {
            threshold: thr,
            threads: threads.unwrap_or(DiarizerConfig::default().threads),
            fast,
            ..Default::default()
        },
    )
    .unwrap();
    let t = Instant::now();
    let turns = d.diarize_sync(&x, n).unwrap();
    let dt = t.elapsed().as_secs_f64();
    let mut spk: Vec<_> = turns.iter().map(|t| t.speaker.0.clone()).collect();
    spk.sort();
    spk.dedup();
    eprintln!(
        "audio {:.1} s, diarized in {:.1} s (RTF {:.3}), {} turns, {} speakers",
        x.len() as f64 / 16000.0,
        dt,
        dt / (x.len() as f64 / 16000.0),
        turns.len(),
        spk.len()
    );
    let json = serde_json::to_string(&turns).unwrap();
    match out {
        Some(p) => std::fs::write(p, json).unwrap(),
        None => println!("{json}"),
    }
}
