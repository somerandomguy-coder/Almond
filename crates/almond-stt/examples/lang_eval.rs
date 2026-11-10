//! Mixed Vietnamese / English evaluation: language routing, short utterances, code-switching,
//! live captions. Builds its test sets from the prepared eval manifests (VIVOS, FLEURS-en) and
//! prints one JSON line per (set, config).
//!
//! ```text
//! crates\almond-stt\scripts\cargo-cuda.cmd run --release -p almond-stt --features cuda,sherpa \
//!     --example lang_eval -- [--sets conv,mixed,live] [--configs routed,routed-raw,...] [--n 60]
//! ```
//!
//! Sets (all utterances level-normalized to -26 dBFS, as in `make_meeting.py`):
//! * `conv`  — one track, a conversation of Vietnamese and English utterances (70/30), a third
//!   of them *short* (1-3 words, cut at word boundaries found by decoding clips whose
//!   transcript was exactly right), gaps 0.3-1.5 s. Scored per reference utterance: language
//!   of the transcript there (LID accuracy, split long/short) and WER per language.
//! * `mixed` — each utterance a Vietnamese clip and an English clip joined with a 0-150 ms gap
//!   (code-switching inside one utterance), scored by WER on the joined reference and per part
//!   (hypothesis words assigned by time).
//! * `live`  — the `conv` track replayed through a live-caption stream in real time: latency of
//!   first-shown / final words, WER and LID of the finals.
//!
//! Configs: `routed` (policy + verify, the default), `routed-raw` (argmax LID, the old
//! behaviour), `routed-noverify`, `zip` (Zipformer only), `turbo-vi`, `turbo-auto` (Whisper
//! language detection per utterance among all languages), `turbo-vien` (Whisper LID between
//! vi/en, no policy), `pho` (PhoWhisper-medium, vi), for `live`: `live-zip` (Zipformer only,
//! the old live default), `live-routed` (LiveRouter as `Speech::live` builds it on the GPU).

use almond_stt::audio::{load_wav_16k_mono, ms_to_samples, rms, samples_to_ms, split_on_silence};
use almond_stt::langid::LidPolicy;
use almond_stt::live::{LiveModel, LiveRouter};
use almond_stt::metrics::{EditCounts, normalize, wer_counts};
use almond_stt::models::ModelManager;
use almond_stt::router::{LanguageId, RoutedEngine};
use almond_stt::sherpa::{SherpaEngine, ZipformerModel};
use almond_stt::streaming::frames;
use almond_stt::whisper::{WhisperConfig, WhisperEngine, WhisperModel};
use almond_types::LangTag;
use almond_types::artifact::body::Segment;
use almond_types::meeting::AudioChannel;
use almond_types::stt::{SttEngine, SttOptions};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::BufRead;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Utt {
    audio: Vec<f32>,
    text: String,
    lang: &'static str,
    short: bool,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn range(&mut self, a: u64, b: u64) -> u64 {
        a + self.next() % (b - a).max(1)
    }
}

fn level(x: &[f32]) -> Vec<f32> {
    // -26 dBFS RMS over voiced frames
    let voiced: Vec<f32> = x
        .chunks(480)
        .filter(|c| rms(c) > 1e-4)
        .flatten()
        .copied()
        .collect();
    let g = 10f32.powf(-26.0 / 20.0) / rms(&voiced).max(1e-6);
    x.iter().map(|s| (s * g).clamp(-1.0, 1.0)).collect()
}

fn trim(x: &[f32]) -> Vec<f32> {
    trim_pad(x, 150)
}

/// Cut leading / trailing silence, keeping `pad_ms` around the speech.
fn trim_pad(x: &[f32], pad_ms: u64) -> Vec<f32> {
    let p = almond_stt::audio::VadParams {
        pad_ms,
        ..Default::default()
    };
    let r = split_on_silence(x, &p);
    match (r.first(), r.last()) {
        (Some(a), Some(b)) => x[a.0..b.1].to_vec(),
        _ => x.to_vec(),
    }
}

fn manifest(path: &std::path::Path, every: usize, limit: usize) -> Vec<Value> {
    let f = std::fs::File::open(path).expect("manifest");
    std::io::BufReader::new(f)
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .step_by(every)
        .take(limit)
        .collect()
}

/// Short clips (first 1-3 words) of utterances the given engine transcribes exactly, cut at the
/// engine's word boundary (+ 120 ms).
async fn shorts(
    e: &dyn SttEngine,
    rows: &[Value],
    lang: &'static str,
    want: usize,
    rng: &mut Rng,
) -> Vec<Utt> {
    let mut out = vec![];
    for r in rows {
        if out.len() >= want {
            break;
        }
        let x = level(&trim(
            &load_wav_16k_mono(r["audio"].as_str().unwrap()).unwrap(),
        ));
        let o = SttOptions {
            language: Some(LangTag(lang.into())),
            word_timestamps: true,
            ..Default::default()
        };
        let Ok(segs) = e.transcribe(&x, 0, AudioChannel::Mixed, &o).await else {
            continue;
        };
        let hyp: String = segs
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if normalize(&hyp) != normalize(r["text"].as_str().unwrap()) {
            continue;
        }
        let words: Vec<_> = segs
            .iter()
            .flat_map(|s| s.words.clone().unwrap_or_default())
            .collect();
        let n = rng.range(1, 4) as usize;
        if words.len() <= n + 1 {
            continue;
        }
        // cut in the gap before the next word
        let next_t0 = words[n].t0_ms as u64;
        let end = (words[n - 1].t1_ms as u64 + 120).min(next_t0.max(words[n - 1].t1_ms as u64));
        let end = ms_to_samples(end.max(300)).min(x.len());
        let text = words[..n]
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        out.push(Utt {
            audio: x[..end].to_vec(),
            text,
            lang,
            short: true,
        });
    }
    out
}

fn full(rows: &[Value], lang: &'static str) -> Vec<Utt> {
    rows.iter()
        .map(|r| Utt {
            audio: level(&trim(
                &load_wav_16k_mono(r["audio"].as_str().unwrap()).unwrap(),
            )),
            text: r["text"].as_str().unwrap().to_string(),
            lang,
            short: false,
        })
        .collect()
}

struct RefUtt {
    t0: u64,
    t1: u64,
    text: String,
    lang: &'static str,
    short: bool,
}

/// Concatenate utterances with random gaps (light noise floor, like the meeting).
fn conversation(utts: Vec<Utt>, rng: &mut Rng) -> (Vec<f32>, Vec<RefUtt>) {
    let mut x: Vec<f32> = vec![];
    let mut refs = vec![];
    let mut noise = Rng(7);
    let mut pad = |x: &mut Vec<f32>, ms: u64| {
        for _ in 0..ms_to_samples(ms) {
            x.push((noise.next() % 1000) as f32 / 1000.0 * 0.0006 - 0.0003);
        }
    };
    pad(&mut x, 500);
    for u in utts {
        let t0 = samples_to_ms(x.len());
        x.extend_from_slice(&u.audio);
        refs.push(RefUtt {
            t0,
            t1: samples_to_ms(x.len()),
            text: u.text,
            lang: u.lang,
            short: u.short,
        });
        let g = rng.range(300, 1500);
        pad(&mut x, g);
    }
    pad(&mut x, 1000);
    (x, refs)
}

fn shuffle<T>(v: &mut [T], rng: &mut Rng) {
    for i in (1..v.len()).rev() {
        let j = rng.range(0, i as u64 + 1) as usize;
        v.swap(i, j);
    }
}

#[derive(Default)]
struct Score {
    wer: BTreeMap<&'static str, EditCounts>,
    lid_ok: BTreeMap<String, (u32, u32)>,
    halluc: u32,
}

impl Score {
    fn json(&self) -> Value {
        let mut o = json!({});
        for (l, c) in &self.wer {
            o[format!("wer_{l}")] = json!((c.rate() * 10000.0).round() / 100.0);
            o[format!("words_{l}")] = json!(c.ref_len);
        }
        for (k, (ok, n)) in &self.lid_ok {
            o[format!("lid_{k}")] = json!(format!("{ok}/{n}"));
            o[format!("lid_{k}_pct")] =
                json!((*ok as f64 * 1000.0 / (*n).max(1) as f64).round() / 10.0);
        }
        o["extra_words_outside_refs"] = json!(self.halluc);
        o
    }
}

/// Words (with times and the language of their segment) of a transcript.
fn timed_words(segs: &[Segment]) -> Vec<(u64, String, String)> {
    let mut v = vec![];
    for s in segs {
        let l = s.lang.as_ref().map(|l| l.primary()).unwrap_or_default();
        match &s.words {
            Some(ws) if !ws.is_empty() => {
                for w in ws {
                    v.push((
                        (w.t0_ms as u64 + w.t1_ms as u64) / 2,
                        w.text.clone(),
                        l.clone(),
                    ));
                }
            }
            _ => {
                // spread the text over the segment
                let ws: Vec<&str> = s.text.split_whitespace().collect();
                let d = (s.t1_ms - s.t0_ms) as u64 / ws.len().max(1) as u64;
                for (i, w) in ws.iter().enumerate() {
                    v.push((
                        s.t0_ms as u64 + d * i as u64 + d / 2,
                        w.to_string(),
                        l.clone(),
                    ));
                }
            }
        }
    }
    v
}

fn score_conv(segs: &[Segment], refs: &[RefUtt]) -> Score {
    let words = timed_words(segs);
    let mut used = vec![false; words.len()];
    let mut sc = Score::default();
    for r in refs {
        let mut hyp = vec![];
        let mut langs: BTreeMap<String, u32> = BTreeMap::new();
        for (i, (t, w, l)) in words.iter().enumerate() {
            if *t + 250 >= r.t0 && *t <= r.t1 + 250 && !used[i] {
                used[i] = true;
                hyp.push(w.as_str());
                *langs.entry(l.clone()).or_default() += 1;
            }
        }
        sc.wer
            .entry(r.lang)
            .or_default()
            .add(wer_counts(&r.text, &hyp.join(" ")));
        let got = langs
            .iter()
            .max_by_key(|x| x.1)
            .map(|x| x.0.clone())
            .unwrap_or_default();
        let key = format!("{}_{}", r.lang, if r.short { "short" } else { "long" });
        let e = sc.lid_ok.entry(key).or_default();
        e.1 += 1;
        // an utterance with no words is a LID miss too (lost)
        if got == r.lang {
            e.0 += 1;
        }
    }
    sc.halluc = used.iter().filter(|u| !**u).count() as u32;
    sc
}

struct Models {
    zip: Arc<SherpaEngine>,
    turbo: Arc<WhisperEngine>,
    turbo_model: Arc<WhisperModel>,
    pho: Option<Arc<WhisperEngine>>,
}

fn routed(m: &Models, policy: LidPolicy, verify: bool) -> RoutedEngine {
    let mut engines: BTreeMap<String, Arc<dyn SttEngine>> = BTreeMap::new();
    engines.insert("vi".into(), m.zip.clone());
    engines.insert("en".into(), m.turbo.clone());
    let lid: Arc<dyn LanguageId> = m.turbo_model.clone();
    let mut r = RoutedEngine::new(lid, engines, m.turbo.clone());
    r.policy = policy;
    if !verify {
        r.verify_min_confidence = None;
    }
    r
}

fn vien() -> Vec<LangTag> {
    vec![LangTag::vi(), LangTag::en()]
}

/// (engine, options) for an offline config.
fn offline(m: &Models, name: &str) -> Option<(Arc<dyn SttEngine>, SttOptions)> {
    let o = |lang: Option<&str>, allowed: Vec<LangTag>| SttOptions {
        language: lang.map(|l| LangTag(l.into())),
        allowed_languages: allowed,
        word_timestamps: true,
        ..Default::default()
    };
    Some(match name {
        "routed" => (
            Arc::new(routed(m, LidPolicy::default(), true)) as _,
            o(None, vien()),
        ),
        "routed-noverify" => (
            Arc::new(routed(m, LidPolicy::default(), false)) as _,
            o(None, vien()),
        ),
        "routed-raw" => (
            Arc::new(routed(m, LidPolicy::raw(), false)) as _,
            o(None, vien()),
        ),
        "zip" => (m.zip.clone() as _, o(Some("vi"), vec![])),
        "turbo-vi" => (m.turbo.clone() as _, o(Some("vi"), vec![])),
        "turbo-en" => (m.turbo.clone() as _, o(Some("en"), vec![])),
        "turbo-auto" => (m.turbo.clone() as _, o(None, vec![])),
        "turbo-vien" => (m.turbo.clone() as _, o(None, vien())),
        "pho" => (m.pho.clone()? as _, o(Some("vi"), vec![])),
        _ => return None,
    })
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v.get(((v.len() as f64 - 1.0) * p).round() as usize)
        .copied()
        .unwrap_or(f64::NAN)
}

async fn run_live(engine: &dyn SttEngine, x: &[f32], refs: &[RefUtt]) -> (Value, Vec<Segment>) {
    let mut s = engine
        .start_stream(SttOptions {
            allowed_languages: vien(),
            ..Default::default()
        })
        .unwrap();
    let start = Instant::now();
    let mut finals: Vec<Segment> = vec![];
    let mut shown: BTreeMap<u32, usize> = BTreeMap::new();
    let (mut first, mut fin) = (vec![], vec![]);
    let mut handle = |ups: Vec<almond_types::stt::SttUpdate>, now: f64| {
        for u in ups {
            let ws = u.segment.words.clone().unwrap_or_default();
            let seen = shown.entry(u.segment.idx).or_default();
            for w in ws.iter().skip(*seen) {
                first.push(now - w.t1_ms as f64);
            }
            *seen = (*seen).max(ws.len());
            if u.is_final {
                // latency of the final: from the end of the segment's audio
                fin.push(now - u.segment.t1_ms as f64);
                finals.push(u.segment);
            }
        }
    };
    for f in frames(x, 100, AudioChannel::Mic) {
        let end = f.t0_ms + samples_to_ms(f.samples.len());
        let due = Duration::from_millis(end);
        let el = start.elapsed();
        if due > el {
            tokio::time::sleep(due - el).await;
        }
        let ups = s.push(f).await.unwrap();
        handle(ups, start.elapsed().as_secs_f64() * 1000.0);
    }
    let ups = s.finish().await.unwrap();
    handle(ups, start.elapsed().as_secs_f64() * 1000.0);
    let sc = score_conv(&finals, refs);
    let mut o = sc.json();
    o["first_shown_ms_p50"] = json!(pct(&mut first, 0.5).round());
    o["first_shown_ms_p90"] = json!(pct(&mut first, 0.9).round());
    o["final_ms_p50"] = json!(pct(&mut fin, 0.5).round());
    o["final_ms_p90"] = json!(pct(&mut fin, 0.9).round());
    o["finals"] = json!(finals.len());
    o["wall_s"] = json!(start.elapsed().as_secs_f64());
    (o, finals)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut sets = "conv,mixed".to_string();
    let mut configs =
        "routed,routed-raw,routed-noverify,zip,turbo-vi,turbo-auto,turbo-vien".to_string();
    let mut n = 60usize;
    let mut dump: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--sets" => sets = args.next().unwrap(),
            "--configs" => configs = args.next().unwrap(),
            "--n" => n = args.next().unwrap().parse().unwrap(),
            "--dump" => dump = args.next(),
            o => panic!("unknown arg {o}"),
        }
    }
    let mgr = ModelManager::open_default().unwrap();
    let turbo_model = Arc::new(
        WhisperModel::from_manager(&mgr, "whisper:large-v3-turbo", WhisperConfig::default())
            .unwrap(),
    );
    let zipm = Arc::new(
        ZipformerModel::from_manager(&mgr, "sherpa:zipformer-vi-30m-int8", Default::default())
            .unwrap(),
    );
    let pho = configs.contains("pho").then(|| {
        Arc::new(WhisperEngine::new(Arc::new(
            WhisperModel::from_manager(&mgr, "whisper:phowhisper-medium", WhisperConfig::default())
                .unwrap(),
        )))
    });
    let m = Models {
        zip: Arc::new(SherpaEngine::new(zipm)),
        turbo: Arc::new(WhisperEngine::new(turbo_model.clone())),
        turbo_model,
        pho,
    };

    let dir = almond_stt::models::default_models_dir()
        .parent()
        .unwrap()
        .join("eval")
        .join("stt");
    let vivos = manifest(&dir.join("vivos-test/manifest.jsonl"), 7, 1000);
    let fen = manifest(&dir.join("fleurs-en/manifest.jsonl"), 1, 1000);
    let mut rng = Rng(42);

    // conversation: n utterances, 70 % vi, 1/3 short
    let n_vi = n * 7 / 10;
    let n_en = n - n_vi;
    let (n_vi_s, n_en_s) = (n_vi / 3, n_en / 3);
    let mut utts = full(&vivos[..n_vi - n_vi_s], "vi");
    utts.extend(full(&fen[..n_en - n_en_s], "en"));
    utts.extend(shorts(m.zip.as_ref(), &vivos[n_vi..], "vi", n_vi_s, &mut rng).await);
    let o_en = m.turbo.clone();
    utts.extend(shorts(o_en.as_ref(), &fen[n_en..], "en", n_en_s, &mut rng).await);
    shuffle(&mut utts, &mut rng);
    eprintln!(
        "conv: {} utterances ({} short)",
        utts.len(),
        utts.iter().filter(|u| u.short).count()
    );
    let (conv, refs) = conversation(utts, &mut rng);
    if let Some(p) = &dump {
        almond_stt::audio::write_wav_16k(format!("{p}.conv.wav"), &conv).unwrap();
        let r: Vec<Value> = refs
            .iter()
            .map(|r| json!({"t0": r.t0, "t1": r.t1, "lang": r.lang, "short": r.short, "text": r.text}))
            .collect();
        std::fs::write(
            format!("{p}.conv.json"),
            serde_json::to_string_pretty(&r).unwrap(),
        )
        .unwrap();
    }

    // mixed: vi clip + 0-150 ms + en clip (and the reverse), one utterance each
    let mut mixed: Vec<(Vec<f32>, String, String, u64)> = vec![];
    for i in 0..n.min(40) {
        let v = &vivos[(n + i * 3) % vivos.len()];
        let e = &fen[(n + i * 2) % fen.len()];
        // tight trims: the join must not leave a pause the VAD could split at
        let xv = level(&trim_pad(
            &load_wav_16k_mono(v["audio"].as_str().unwrap()).unwrap(),
            30,
        ));
        let xe = level(&trim_pad(
            &load_wav_16k_mono(e["audio"].as_str().unwrap()).unwrap(),
            30,
        ));
        let gap = vec![0.0f32; ms_to_samples(rng.range(0, 150))];
        let (tv, te) = (v["text"].as_str().unwrap(), e["text"].as_str().unwrap());
        let mut x = vec![0.0f32; ms_to_samples(300)];
        let (first_lang, a, b, ta, tb) = if i % 2 == 0 {
            ("vi", &xv, &xe, tv, te)
        } else {
            ("en", &xe, &xv, te, tv)
        };
        x.extend_from_slice(a);
        let boundary = samples_to_ms(x.len()) + samples_to_ms(gap.len()) / 2;
        x.extend_from_slice(&gap);
        x.extend_from_slice(b);
        x.extend(vec![0.0f32; ms_to_samples(300)]);
        let _ = first_lang;
        mixed.push((x, ta.to_string(), tb.to_string(), boundary));
    }

    for set in sets.split([',', ':']) {
        for c in configs.split([',', ':']) {
            let t = Instant::now();
            let mut out = json!({"set": set, "config": c});
            match set {
                "conv" => {
                    let Some((e, o)) = offline(&m, c) else {
                        continue;
                    };
                    let segs = e.transcribe(&conv, 0, AudioChannel::Mic, &o).await.unwrap();
                    let sc = score_conv(&segs, &refs);
                    for (k, v) in sc.json().as_object().unwrap() {
                        out[k] = v.clone();
                    }
                    if let Some(p) = &dump {
                        std::fs::write(
                            format!("{p}.conv.{c}.json"),
                            serde_json::to_string_pretty(&segs).unwrap(),
                        )
                        .unwrap();
                    }
                }
                "mixed" => {
                    let Some((e, o)) = offline(&m, c) else {
                        continue;
                    };
                    let (mut all, mut pa, mut pb) = (
                        EditCounts::default(),
                        EditCounts::default(),
                        EditCounts::default(),
                    );
                    let mut samples = vec![];
                    for (x, ta, tb, boundary) in &mixed {
                        let segs = e.transcribe(x, 0, AudioChannel::Mic, &o).await.unwrap();
                        let ws = timed_words(&segs);
                        let hyp: Vec<&str> = ws.iter().map(|w| w.1.as_str()).collect();
                        all.add(wer_counts(&format!("{ta} {tb}"), &hyp.join(" ")));
                        let ha: Vec<&str> = ws
                            .iter()
                            .filter(|w| w.0 < *boundary)
                            .map(|w| w.1.as_str())
                            .collect();
                        let hb: Vec<&str> = ws
                            .iter()
                            .filter(|w| w.0 >= *boundary)
                            .map(|w| w.1.as_str())
                            .collect();
                        pa.add(wer_counts(ta, &ha.join(" ")));
                        pb.add(wer_counts(tb, &hb.join(" ")));
                        if samples.len() < 4 {
                            samples.push(hyp.join(" "));
                        }
                    }
                    out["wer_all"] = json!((all.rate() * 10000.0).round() / 100.0);
                    out["wer_first_part"] = json!((pa.rate() * 10000.0).round() / 100.0);
                    out["wer_second_part"] = json!((pb.rate() * 10000.0).round() / 100.0);
                    out["n"] = json!(mixed.len());
                    out["examples"] = json!(samples);
                }
                "live" => {
                    let engine: Arc<dyn SttEngine> = match c {
                        "live-zip" => m.zip.clone(),
                        "live-routed" | "live-routed-nolidpartials" | "live-routed-norefine-vi" => {
                            let zip: Arc<dyn LiveModel> = m.zip.clone();
                            let turbo: Arc<dyn LiveModel> = m.turbo.clone();
                            let mut finals: BTreeMap<String, Arc<dyn LiveModel>> =
                                [("en".to_string(), turbo.clone())].into();
                            if c != "live-routed-norefine-vi" {
                                finals.insert("vi".into(), zip.clone());
                            }
                            Arc::new(LiveRouter {
                                lid: Some(m.turbo_model.clone()),
                                langs: vien(),
                                partial: [("vi".to_string(), zip), ("en".to_string(), turbo)]
                                    .into(),
                                finals,
                                policy: LidPolicy::default(),
                                lid_partials: c != "live-routed-nolidpartials",
                                stream: Default::default(),
                            })
                        }
                        _ => continue,
                    };
                    let (r, finals) = run_live(engine.as_ref(), &conv, &refs).await;
                    if let Some(p) = &dump {
                        std::fs::write(
                            format!("{p}.conv.{c}.json"),
                            serde_json::to_string_pretty(&finals).unwrap(),
                        )
                        .unwrap();
                    }
                    for (k, v) in r.as_object().unwrap() {
                        out[k] = v.clone();
                    }
                }
                _ => continue,
            }
            out["sec"] = json!((t.elapsed().as_secs_f64() * 10.0).round() / 10.0);
            println!("{out}");
        }
    }
}
