//! Replay a WAV through a live-caption stream in (simulated) real time and report caption
//! latency and the final text.
//!
//! ```text
//! cargo run --release -p almond-stt --features whisper,sherpa --example live -- \
//!     --model sherpa:zipformer-vi-30m-int8 file.wav [--lang vi] [--fast] [--cpu] [--quiet]
//! ```
//! `--fast` feeds frames as fast as decoding allows (measures throughput instead of latency).
//! Latency = wall time from when a word's audio "arrived" (its end time) to when it was first
//! shown as part of a final segment.

// Without an engine feature only the arg parsing compiles; silence the resulting lints.
#![cfg_attr(
    not(any(feature = "whisper", feature = "sherpa")),
    allow(unused, unreachable_code)
)]

use almond_stt::models::ModelManager;
use almond_stt::streaming::frames;
use almond_types::LangTag;
use almond_types::meeting::AudioChannel;
use almond_types::stt::{SttEngine, SttOptions};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let (mut model, mut wav, mut lang) = (String::new(), String::new(), None);
    let (mut fast, mut cpu, mut quiet) = (false, false, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--model" => model = args.next().unwrap(),
            "--lang" => lang = args.next().map(LangTag),
            "--fast" => fast = true,
            "--cpu" => cpu = true,
            "--quiet" => quiet = true,
            _ => wav = a,
        }
    }
    let _ = cpu; // only read by the whisper arm
    let mgr = ModelManager::open_default().unwrap();
    let engine: Arc<dyn SttEngine> = match mgr.entry(&model).unwrap().engine.as_str() {
        #[cfg(feature = "whisper")]
        "whisper" => {
            let mut cfg = almond_stt::whisper::WhisperConfig::default();
            if cpu {
                cfg.use_gpu = false;
                cfg.flash_attn = false;
            }
            Arc::new(almond_stt::whisper::WhisperEngine::new(Arc::new(
                almond_stt::whisper::WhisperModel::from_manager(&mgr, &model, cfg).unwrap(),
            )))
        }
        #[cfg(feature = "sherpa")]
        "sherpa-transducer" => Arc::new(almond_stt::sherpa::SherpaEngine::new(Arc::new(
            almond_stt::sherpa::ZipformerModel::from_manager(&mgr, &model, Default::default())
                .unwrap(),
        ))),
        e => panic!("engine {e} not compiled in"),
    };
    let x = almond_stt::audio::load_wav_16k_mono(&wav).unwrap();
    let mut stream = engine
        .start_stream(SttOptions {
            language: lang,
            ..Default::default()
        })
        .unwrap();

    let start = Instant::now();
    let mut finals = vec![];
    let mut latencies_ms: Vec<f64> = vec![];
    let mut n_partials = 0usize;
    // First-shown latency: when a word position of a segment is displayed for the first time
    // (partial or final), how long after its audio ended.
    let mut shown: std::collections::HashMap<u32, usize> = Default::default();
    let mut first_ms: Vec<f64> = vec![];
    let fs = frames(&x, 100, AudioChannel::Mic);
    let mut handle = |ups: Vec<almond_types::stt::SttUpdate>, now_ms: f64| {
        for u in ups {
            let words = u.segment.words.as_deref().unwrap_or(&[]);
            let seen = shown.entry(u.segment.idx).or_default();
            for w in words.iter().skip(*seen) {
                first_ms.push(now_ms - w.t1_ms as f64);
            }
            *seen = (*seen).max(words.len());
            if u.is_final {
                for w in u.segment.words.iter().flatten() {
                    latencies_ms.push(now_ms - w.t1_ms as f64);
                }
                if !quiet {
                    eprintln!(
                        "[{:>7.1}s] FINAL #{:<3} {}",
                        now_ms / 1000.0,
                        u.segment.idx,
                        u.segment.text
                    );
                }
                finals.push(u.segment.text);
            } else {
                n_partials += 1;
                if !quiet {
                    eprintln!(
                        "[{:>7.1}s]  part #{:<3} {}",
                        now_ms / 1000.0,
                        u.segment.idx,
                        u.segment.text
                    );
                }
            }
        }
    };
    for f in fs {
        let audio_end = f.t0_ms + (f.samples.len() as u64 * 1000 / 16_000);
        if !fast {
            let due = Duration::from_millis(audio_end);
            let el = start.elapsed();
            if due > el {
                tokio::time::sleep(due - el).await;
            }
        }
        let ups = stream.push(f).await.unwrap();
        let now = if fast {
            audio_end as f64
        } else {
            start.elapsed().as_secs_f64() * 1000.0
        };
        handle(ups, now);
    }
    let ups = stream.finish().await.unwrap();
    let now = if fast {
        x.len() as f64 / 16.0
    } else {
        start.elapsed().as_secs_f64() * 1000.0
    };
    handle(ups, now);
    let wall = start.elapsed().as_secs_f64();
    latencies_ms.sort_by(|a, b| a.total_cmp(b));
    first_ms.sort_by(|a, b| a.total_cmp(b));
    let pct_of = |v: &Vec<f64>, p: f64| {
        v.get(((v.len() as f64 - 1.0) * p) as usize)
            .copied()
            .unwrap_or(f64::NAN)
    };
    let pct = |p: f64| pct_of(&latencies_ms, p);
    println!(
        "{}",
        serde_json::json!({
            "model": model, "audio_s": x.len() as f64 / 16000.0, "wall_s": wall,
            "finals": finals.len(), "partials": n_partials,
            "final_latency_ms_p50": pct(0.5), "final_latency_ms_p90": pct(0.9),
            "first_shown_latency_ms_p50": pct_of(&first_ms, 0.5),
            "first_shown_latency_ms_p90": pct_of(&first_ms, 0.9),
            "text": finals.join(" "),
        })
    );
}
