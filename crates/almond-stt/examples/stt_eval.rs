//! WER/CER/RTF evaluation over a manifest produced by `eval/stt/prepare_data.py`.
//!
//! ```text
//! cargo run --release -p almond-stt --features whisper,sherpa --example stt_eval -- \
//!     --model whisper:phowhisper-small --manifest ~/.almond/eval/stt/vivos-test/manifest.jsonl \
//!     [--limit 100] [--cpu] [--threads 8] [--beam 5] [--lang vi] [--tag note]
//! ```
//! (GPU on Windows: `crates\almond-stt\scripts\cargo-cuda.cmd run --release ...` with `cuda`.)
//!
//! Prints one JSON summary line and writes per-utterance hypotheses + the summary to
//! `$ALMOND_EVAL_DIR/results/` (default `~/.almond/eval/stt/results/`).

// Without an engine feature only the arg parsing compiles; silence the resulting lints.
#![cfg_attr(
    not(any(feature = "whisper", feature = "sherpa")),
    allow(unused, unreachable_code)
)]

use almond_stt::metrics::{EditCounts, cer_counts, wer_counts};
use almond_stt::models::ModelManager;
use almond_types::LangTag;
use almond_types::meeting::AudioChannel;
use almond_types::stt::SttOptions;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

type Transcriber = Box<dyn Fn(&[f32], &SttOptions) -> almond_types::Result<String>>;

struct Args {
    model: String,
    manifest: PathBuf,
    limit: usize,
    cpu: bool,
    threads: Option<i32>,
    beam: Option<i32>,
    lang: Option<String>,
    tag: String,
}

fn parse_args() -> Args {
    let mut a = Args {
        model: String::new(),
        manifest: PathBuf::new(),
        limit: 0,
        cpu: false,
        threads: None,
        beam: None,
        lang: None,
        tag: String::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().expect("missing value");
        match k.as_str() {
            "--model" => a.model = v(),
            "--manifest" => a.manifest = PathBuf::from(v()),
            "--limit" => a.limit = v().parse().unwrap(),
            "--cpu" => a.cpu = true,
            "--threads" => a.threads = Some(v().parse().unwrap()),
            "--beam" => a.beam = Some(v().parse().unwrap()),
            "--lang" => a.lang = Some(v()),
            "--tag" => a.tag = v(),
            other => panic!("unknown arg {other}"),
        }
    }
    assert!(!a.model.is_empty(), "--model required");
    a
}

fn load_engine(mgr: &ModelManager, a: &Args) -> (Transcriber, String) {
    let entry = mgr.entry(&a.model).expect("model in registry").clone();
    match entry.engine.as_str() {
        #[cfg(feature = "whisper")]
        "whisper" => {
            use almond_stt::whisper::{WhisperConfig, WhisperModel};
            let mut cfg = WhisperConfig::default();
            if a.cpu {
                cfg.use_gpu = false;
                cfg.flash_attn = false;
            }
            if let Some(t) = a.threads {
                cfg.threads = t;
            }
            if let Some(b) = a.beam {
                cfg.beam_size = b;
            }
            let device = if cfg.use_gpu { "gpu" } else { "cpu" };
            let device = format!("{device}/t{}/beam{}", cfg.threads, cfg.beam_size);
            let m = WhisperModel::from_manager(mgr, &a.model, cfg).expect("load whisper model");
            (
                Box::new(move |x, o| {
                    Ok(m.transcribe_sync(x, 0, AudioChannel::Mixed, o)?
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" "))
                }),
                device,
            )
        }
        #[cfg(feature = "sherpa")]
        "sherpa-transducer" => {
            use almond_stt::sherpa::{SherpaConfig, ZipformerModel};
            let mut cfg = SherpaConfig::default();
            if let Some(t) = a.threads {
                cfg.threads = t;
            }
            let device = format!("cpu/t{}", cfg.threads);
            let m = ZipformerModel::from_manager(mgr, &a.model, cfg).expect("load zipformer");
            (
                Box::new(move |x, o| {
                    Ok(m.transcribe_sync(x, 0, AudioChannel::Mixed, o)?
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" "))
                }),
                device,
            )
        }
        e => panic!("engine {e} not compiled in (enable the whisper / sherpa feature)"),
    }
}

fn main() {
    let a = parse_args();
    let mgr = ModelManager::open_default().expect("models dir");
    let t_load = Instant::now();
    let (transcribe, device) = load_engine(&mgr, &a);
    let load_s = t_load.elapsed().as_secs_f64();

    let f = std::fs::File::open(&a.manifest).expect("manifest");
    let mut rows: Vec<Value> = std::io::BufReader::new(f)
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect();
    if a.limit > 0 {
        rows.truncate(a.limit);
    }
    let set = a
        .manifest
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let (mut wer, mut cer) = (EditCounts::default(), EditCounts::default());
    let (mut audio_s, mut proc_s) = (0.0f64, 0.0f64);
    let mut hyps = vec![];
    // warm-up (first call allocates GPU buffers)
    if let Some(r) = rows.first() {
        let x = almond_stt::audio::load_wav_16k_mono(r["audio"].as_str().unwrap()).unwrap();
        let _ = transcribe(&x[..x.len().min(16_000)], &SttOptions::default());
    }
    for (i, r) in rows.iter().enumerate() {
        let x = almond_stt::audio::load_wav_16k_mono(r["audio"].as_str().unwrap()).unwrap();
        let lang = a
            .lang
            .clone()
            .or_else(|| r["lang"].as_str().map(String::from));
        let opts = SttOptions {
            language: lang.map(LangTag),
            ..Default::default()
        };
        let t = Instant::now();
        let hyp = transcribe(&x, &opts).unwrap_or_else(|e| format!("<error {e}>"));
        let dt = t.elapsed().as_secs_f64();
        let reference = r["text"].as_str().unwrap_or("");
        let w = wer_counts(reference, &hyp);
        let c = cer_counts(reference, &hyp);
        wer.add(w);
        cer.add(c);
        audio_s += x.len() as f64 / 16_000.0;
        proc_s += dt;
        hyps.push(json!({"id": r["id"], "ref": reference, "hyp": hyp,
                         "wer": w.rate(), "cer": c.rate(), "sec": dt}));
        if (i + 1) % 50 == 0 {
            eprintln!(
                "[{}/{}] WER {:.2}% CER {:.2}% RTF {:.3}",
                i + 1,
                rows.len(),
                100.0 * wer.rate(),
                100.0 * cer.rate(),
                proc_s / audio_s
            );
        }
    }
    let summary = json!({
        "model": a.model, "set": set, "n": rows.len(), "device": device, "tag": a.tag,
        "wer": wer.rate(), "cer": cer.rate(),
        "wer_sub": wer.substitutions, "wer_del": wer.deletions, "wer_ins": wer.insertions,
        "ref_words": wer.ref_len,
        "audio_s": audio_s, "proc_s": proc_s, "rtf": proc_s / audio_s.max(1e-9), "load_s": load_s,
    });
    println!("{summary}");

    let dir = std::env::var_os("ALMOND_EVAL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            almond_stt::models::default_models_dir()
                .parent()
                .unwrap()
                .join("eval")
                .join("stt")
        })
        .join("results");
    std::fs::create_dir_all(&dir).unwrap();
    let stem = format!(
        "{}__{}__{}{}",
        a.model.replace([':', '/'], "_"),
        set,
        device.replace('/', "_"),
        if a.tag.is_empty() {
            String::new()
        } else {
            format!("__{}", a.tag)
        }
    );
    let mut f = std::fs::File::create(dir.join(format!("{stem}.jsonl"))).unwrap();
    for h in hyps {
        writeln!(f, "{h}").unwrap();
    }
    std::fs::write(
        dir.join(format!("{stem}.summary.json")),
        summary.to_string(),
    )
    .unwrap();
}
