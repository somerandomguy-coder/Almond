//! Live captions in the language chosen by setting `captions.language`, with the real engines.
//! Ignored (needs `--features whisper,sherpa` and `sherpa:zipformer-en-small-int8` installed):
//!
//! ```text
//! cargo test --release -p almond-core --features whisper,sherpa --test captions_lang -- --ignored --nocapture
//! ```

#![cfg(all(feature = "whisper", feature = "sherpa"))]

mod common;

use almond_audio::WavBackend;
use almond_types::api::notifications;
use almond_types::meeting::AudioChannel;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// ~20 s of English: the model's sample clips with pauses, as a mono 16-kHz WAV.
fn english_wav(dir: &std::path::Path) -> std::path::PathBuf {
    let src = almond_stt::models::default_models_dir()
        .join("sherpa__zipformer-en-small-int8")
        .join("test_wavs");
    let mut out: Vec<i16> = vec![];
    for f in ["0.wav", "1.wav"] {
        let mut r =
            hound::WavReader::open(src.join(f)).expect("install sherpa:zipformer-en-small-int8");
        out.extend(r.samples::<i16>().map(|s| s.unwrap()));
        out.extend(std::iter::repeat_n(0i16, 16_000));
    }
    let p = dir.join("en.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&p, spec).unwrap();
    for s in out {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
    p
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs speech models and native engines"]
async fn english_live_captions_when_chosen() {
    let (api, dir) = common::open_core("captions-en").await;
    let wav = english_wav(&dir);
    api.core
        .capture
        .set_audio_backend(Some(Arc::new(WavBackend {
            files: vec![(AudioChannel::Mic, wav, Some(0))],
            realtime: true,
        })));
    api.call(
        "settings.set",
        json!({ "key": "captions.language", "value": "en" }),
    )
    .await
    .unwrap();
    let mut rx = api.subscribe();
    api.call(
        "recording.start",
        json!({
            "meeting_id": null,
            "create": { "title": "en", "kind": { "type": "online", "app": null }, "calendar_event": null },
            "channels": ["mic"], "mic_device": null, "system_device": null, "live_transcript": true,
        }),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_secs(22)).await;
    api.call("recording.stop", Value::Null).await.unwrap();
    let mut finals = vec![];
    while let Ok(n) = rx.try_recv() {
        if n.name == notifications::TRANSCRIPT_UPDATE && n.payload["update"]["is_final"] == true {
            finals.push(
                n.payload["update"]["segment"]["text"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
    }
    eprintln!("finals: {finals:#?}");
    let text = finals.join(" ").to_uppercase();
    assert!(
        text.contains("NIGHTFALL") && text.contains("LAMPS"),
        "{text}"
    );
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}
