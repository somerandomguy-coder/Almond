//! Microphone test (`audio.testStart` / `audio.testStop`) on the mock audio backend: levels are
//! streamed, the test never runs during a recording, and `recording.start` stops it.

mod common;

use almond_types::mock::MockAudioBackend;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

async fn wait_for(
    rx: &mut tokio::sync::broadcast::Receiver<almond_core::Notification>,
    name: &str,
) -> Vec<Value> {
    let t0 = Instant::now();
    let mut seen = vec![];
    loop {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Ok(n)) if n.name == name => return seen,
            Ok(Ok(n)) if n.name == "audio.testLevel" => seen.push(n.payload),
            Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            other => panic!("waiting for {name}: {other:?}"),
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "no {name}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mic_test_streams_levels_and_yields_to_recording() {
    let (api, dir) = common::open_core("mictest").await;

    // 1. A short test: tone levels (0.2 amplitude sine ≈ -17 dBFS RMS), then it stops by itself.
    api.core
        .capture
        .set_audio_backend(Some(Arc::new(MockAudioBackend { frames: 20 })));
    let mut rx = api.subscribe();
    let r = api.call("audio.testStart", json!({})).await.unwrap();
    assert_eq!(r["device"], "Mock Microphone");
    let levels = wait_for(&mut rx, "audio.testStopped").await;
    assert!(!levels.is_empty());
    for l in &levels {
        let db = l["rms_db"].as_f64().unwrap();
        assert!((-18.0..-16.0).contains(&db), "{l}");
        assert!(l["level"].as_f64().unwrap() > 0.7, "{l}");
    }

    // 2. recording.start stops a running test (and records).
    api.core
        .capture
        .set_audio_backend(Some(Arc::new(MockAudioBackend { frames: u32::MAX })));
    api.call("audio.testStart", json!({})).await.unwrap();
    let mut rx = api.subscribe();
    let st = api
        .call(
            "recording.start",
            json!({
                "meeting_id": null,
                "create": { "title": "Mic test", "kind": { "type": "online", "app": null }, "calendar_event": null },
                "channels": ["mic"], "mic_device": null, "system_device": null, "live_transcript": false,
            }),
        )
        .await
        .unwrap();
    assert_eq!(st["state"], "recording", "{st}");
    wait_for(&mut rx, "audio.testStopped").await;

    // 3. No test during a recording.
    let e = api.call("audio.testStart", json!({})).await.unwrap_err();
    assert!(e.message.contains("recording"), "{}", e.message);

    api.call("recording.stop", Value::Null).await.unwrap();
    api.call("audio.testStop", Value::Null).await.unwrap(); // no-op
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}
