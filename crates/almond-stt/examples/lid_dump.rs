//! Dump Whisper language-ID probabilities (and optionally decode scores) for tuning the
//! language-routing policy (`almond_stt::langid`). Output: JSON lines on stdout.
//!
//! ```text
//! # per utterance of a manifest, at several crop lengths of the speech (0 = whole utterance)
//! cargo-cuda.cmd run --release -p almond-stt --features cuda,sherpa --example lid_dump -- \
//!     --manifest ~/.almond/eval/stt/vivos-test/manifest.jsonl --limit 200 --crops 500,1000,2000,0 [--decode]
//! # per VAD region of a recording (router VAD), with decodes in vi / en and Zipformer
//! cargo-cuda.cmd run ... --example lid_dump -- --wav mic.wav [--channel 0]
//! ```

use almond_stt::audio::{VadParams, load_wav_channels, resample_linear, samples_to_ms, split_on_silence};
use almond_stt::models::ModelManager;
use almond_stt::whisper::{WhisperConfig, WhisperModel};
use almond_types::LangTag;
use serde_json::{Value, json};
use std::io::BufRead;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut manifest, mut wav, mut limit, mut channel) = (None, None, 0usize, 0usize);
    let mut crops: Vec<u64> = vec![0];
    let mut decode = false;
    let mut every = 1usize;
    let mut lid_model = "whisper:large-v3-turbo".to_string();
    // Scale each clip to this RMS (dBFS) before LID / decoding.
    let mut norm: Option<f32> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--manifest" => manifest = args.next(),
            "--wav" => wav = args.next(),
            "--limit" => limit = args.next().unwrap().parse().unwrap(),
            "--every" => every = args.next().unwrap().parse().unwrap(),
            "--channel" => channel = args.next().unwrap().parse().unwrap(),
            "--crops" => {
                crops = args
                    .next()
                    .unwrap()
                    .split([',', ':'])
                    .map(|s| s.parse().unwrap())
                    .collect()
            }
            "--decode" => decode = true,
            "--lid-model" => lid_model = args.next().unwrap(),
            "--norm" => norm = Some(args.next().unwrap().parse::<f32>().unwrap()),
            o => panic!("unknown arg {o}"),
        }
    }
    let mgr = ModelManager::open_default().unwrap();
    let m = WhisperModel::from_manager(&mgr, &lid_model, WhisperConfig::default()).unwrap();
    let mut st = m.create_state().unwrap();
    let allowed = [LangTag::vi(), LangTag::en()];
    let scale = |x: &[f32]| -> Vec<f32> {
        match norm {
            Some(db) => {
                let r = almond_stt::audio::rms(x).max(1e-6);
                let g = 10f32.powf(db / 20.0) / r;
                x.iter().map(|s| (s * g).clamp(-1.0, 1.0)).collect()
            }
            None => x.to_vec(),
        }
    };
    let probs = |st: &mut _, x: &[f32]| -> Value {
        let x = &scale(x);
        let p = m.language_probs(st, x, &allowed).unwrap();
        let any = m.language_probs(st, x, &[]).unwrap();
        json!({
            "vi": p.iter().find(|c| c.0.0 == "vi").map(|c| c.1),
            "any": any.iter().map(|(l, p)| json!([l.0, p])).collect::<Vec<_>>(),
        })
    };
    let scored = |st: &mut _, x: &[f32], l: LangTag| -> Value {
        let s = m.decode_scored(st, &scale(x), &l, 1).unwrap();
        json!({"text": s.text, "lp": s.avg_logprob, "mp": s.mean_p, "ns": s.no_speech_p, "n": s.n_tokens})
    };

    if let Some(path) = manifest {
        let f = std::fs::File::open(path).unwrap();
        let mut rows: Vec<Value> = std::io::BufReader::new(f)
            .lines()
            .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
            .collect();
        rows = rows.into_iter().step_by(every.max(1)).collect();
        if limit > 0 {
            rows.truncate(limit);
        }
        for r in rows {
            let x = almond_stt::audio::load_wav_16k_mono(r["audio"].as_str().unwrap()).unwrap();
            // speech span (trim leading/trailing silence)
            let reg = split_on_silence(&x, &VadParams::default());
            let (a, b) = match (reg.first(), reg.last()) {
                (Some(f), Some(l)) => (f.0, l.1),
                _ => (0, x.len()),
            };
            let sp = &x[a..b];
            for &c in &crops {
                let n = if c == 0 {
                    sp.len()
                } else {
                    almond_stt::audio::ms_to_samples(c).min(sp.len())
                };
                if c != 0 && n < almond_stt::audio::ms_to_samples(c) {
                    continue;
                }
                let y = &sp[..n];
                let mut o = json!({
                    "id": r["id"], "lang": r["lang"], "crop": c, "dur_ms": samples_to_ms(n),
                    "p": probs(&mut st, y),
                });
                if decode {
                    o["dec_vi"] = scored(&mut st, y, LangTag::vi());
                    o["dec_en"] = scored(&mut st, y, LangTag::en());
                }
                println!("{o}");
            }
        }
    }

    if let Some(path) = wav {
        let (ch, sr) = load_wav_channels(&path).unwrap();
        let x = resample_linear(&ch[channel.min(ch.len() - 1)], sr, 16_000);
        #[cfg(feature = "sherpa")]
        let zip = almond_stt::sherpa::ZipformerModel::from_manager(
            &mgr,
            "sherpa:zipformer-vi-30m-int8",
            Default::default(),
        )
        .ok();
        let vad = VadParams {
            min_silence_ms: 200,
            ..VadParams::default()
        };
        for (a, b) in split_on_silence(&x, &vad) {
            let y = &x[a..b];
            let mut o = json!({
                "t0": samples_to_ms(a), "t1": samples_to_ms(b), "dur_ms": samples_to_ms(b - a),
                "rms": almond_stt::audio::rms(y),
                "p": probs(&mut st, y),
                "dec_vi": scored(&mut st, y, LangTag::vi()),
                "dec_en": scored(&mut st, y, LangTag::en()),
            });
            #[cfg(feature = "sherpa")]
            if let Some(z) = &zip {
                let w = z.decode_words(y).unwrap();
                o["zip"] = json!(w.iter().map(|w| w.text.as_str()).collect::<String>().trim());
            }
            println!("{o}");
        }
    }
}
