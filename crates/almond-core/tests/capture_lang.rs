//! Vietnamese + English through the capture module's speech engines (`Speech::offline` /
//! `Speech::live`), on clips from the eval sets (eval/stt/RESULTS.md, "Vietnamese + English").
//!
//! Real models only (ignored):
//! ```text
//! crates\almond-stt\scripts\cargo-cuda.cmd test --release -p almond-core --features cuda,sherpa \
//!     --test capture_lang -- --ignored --nocapture
//! ```
//! Needs `python eval/stt/prepare_data.py vivos-test fleurs-en` and the default models.

#[cfg(all(feature = "whisper", feature = "sherpa"))]
mod real {
    use almond_core::capture::engines::Speech;
    use almond_stt::audio::{load_wav_16k_mono, ms_to_samples, rms};
    use almond_stt::streaming::frames;
    use almond_types::LangTag;
    use almond_types::artifact::body::Segment;
    use almond_types::meeting::AudioChannel;
    use almond_types::stt::{SttEngine, SttOptions};
    use serde_json::Value;
    use std::io::BufRead;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn eval_dir() -> PathBuf {
        almond_stt::models::default_models_dir()
            .parent()
            .unwrap()
            .join("eval")
            .join("stt")
    }

    fn clip(set: &str, i: usize) -> Vec<f32> {
        let f = std::fs::File::open(eval_dir().join(set).join("manifest.jsonl")).unwrap();
        let row: Value = std::io::BufReader::new(f)
            .lines()
            .nth(i)
            .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
            .unwrap();
        let x = load_wav_16k_mono(row["audio"].as_str().unwrap()).unwrap();
        // -26 dBFS, like the synthetic meeting
        let g = 10f32.powf(-26.0 / 20.0) / rms(&x).max(1e-6);
        x.iter().map(|s| (s * g).clamp(-1.0, 1.0)).collect()
    }

    /// vi, short vi (first 0.8 s), en, vi with 0.6 s pauses; returns audio + utterance spans.
    fn conversation() -> (Vec<f32>, Vec<(u64, u64, &'static str)>) {
        let mut x = vec![0.0f32; ms_to_samples(500)];
        let mut spans = vec![];
        let short = clip("vivos-test", 3);
        let parts: Vec<(Vec<f32>, &str)> = vec![
            (clip("vivos-test", 0), "vi"),
            (short[..ms_to_samples(800).min(short.len())].to_vec(), "vi"),
            (clip("fleurs-en", 0), "en"),
            (clip("vivos-test", 7), "vi"),
        ];
        for (a, l) in parts {
            let t0 = x.len() as u64 / 16;
            x.extend(a);
            spans.push((t0, x.len() as u64 / 16, l));
            x.extend(vec![0.0f32; ms_to_samples(600)]);
        }
        (x, spans)
    }

    fn languages_by_span(segs: &[Segment], spans: &[(u64, u64, &str)]) -> Vec<String> {
        spans
            .iter()
            .map(|(a, b, _)| {
                segs.iter()
                    .filter(|s| (s.t0_ms as u64) < *b && (s.t1_ms as u64) > *a)
                    .max_by_key(|s| s.text.len())
                    .and_then(|s| s.lang.as_ref().map(|l| l.primary()))
                    .unwrap_or_default()
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs speech models, native engines and the eval sets"]
    async fn offline_routes_vietnamese_short_and_english() {
        let speech = Speech::new(almond_stt::models::default_models_dir());
        let setup = speech.offline(&[LangTag::vi(), LangTag::en()]).unwrap();
        let (x, spans) = conversation();
        let segs = setup
            .engine
            .transcribe(&x, 0, AudioChannel::Mic, &setup.opts)
            .await
            .unwrap();
        for s in &segs {
            eprintln!("  [{:>6}-{:>6}] {:?} {}", s.t0_ms, s.t1_ms, s.lang, s.text);
        }
        let got = languages_by_span(&segs, &spans);
        let want: Vec<&str> = spans.iter().map(|s| s.2).collect();
        assert_eq!(got, want, "models {}", setup.models);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs speech models, native engines and the eval sets"]
    async fn live_captions_switch_to_english_finals() {
        let speech = Speech::new(almond_stt::models::default_models_dir());
        let engine = speech.live(&[LangTag::vi(), LangTag::en()]).unwrap();
        eprintln!("live engine: {}", engine.model().id);
        let (x, spans) = conversation();
        let mut stream = engine
            .start_stream(SttOptions {
                allowed_languages: vec![LangTag::vi(), LangTag::en()],
                ..Default::default()
            })
            .unwrap();
        let start = Instant::now();
        let mut finals: Vec<Segment> = vec![];
        let mut lat = vec![];
        for f in frames(&x, 100, AudioChannel::Mic) {
            let end = f.t0_ms + f.samples.len() as u64 / 16;
            let due = Duration::from_millis(end);
            if due > start.elapsed() {
                tokio::time::sleep(due - start.elapsed()).await;
            }
            for u in stream.push(f).await.unwrap() {
                if u.is_final {
                    lat.push(start.elapsed().as_millis() as i64 - u.segment.t1_ms as i64);
                    finals.push(u.segment);
                }
            }
        }
        finals.extend(
            stream
                .finish()
                .await
                .unwrap()
                .into_iter()
                .filter(|u| u.is_final)
                .map(|u| u.segment),
        );
        for s in &finals {
            eprintln!(
                "  final [{:>6}-{:>6}] {:?} {}",
                s.t0_ms, s.t1_ms, s.lang, s.text
            );
        }
        eprintln!("final latency (ms): {lat:?}");
        let got = languages_by_span(&finals, &spans);
        assert_eq!(got[2], "en", "the English utterance: {got:?}");
        assert_eq!(got[0], "vi", "{got:?}");
        assert_eq!(got[3], "vi", "{got:?}");
    }
}
