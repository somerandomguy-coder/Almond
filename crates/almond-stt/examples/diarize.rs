//! Diarize one channel of a WAV with sherpa-onnx (pyannote-seg-3.0 + 3D-Speaker).
//!
//! ```text
//! cargo run --release -p almond-stt --features sherpa --example diarize -- file.wav \
//!     [--channel 1] [--speakers N] [--threshold 0.5] [--out turns.json]
//! ```

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
    while let Some(a) = args.next() {
        match a.as_str() {
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
