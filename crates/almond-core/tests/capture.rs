//! Capture module through the core API: recording from a WAV "device" pair (no hardware),
//! meeting + blobs + events, live captions, `transcribe.local`, `audio.url`, crash recovery.
//!
//! Ignored tests need this PC's hardware / models and the native features:
//! ```text
//! crates\almond-stt\scripts\cargo-cuda.cmd test --release -p almond-core --features cuda,sherpa \
//!     --test capture -- --ignored --nocapture
//! ```

mod common;

use almond_audio::WavBackend;
use almond_core::capture::{SpeechOverride, TRANSCRIBE_LOCAL};
use almond_core::{Api, Core, CoreConfig};
use almond_types::api::notifications;
use almond_types::artifact::body::Transcript;
use almond_types::meeting::{AudioChannel, Meeting};
use almond_types::mock::MockStt;
use almond_types::pipeline::JobCtx;
use almond_types::store::{BlobStore, Store};
use almond_types::{Artifact, Id};
use serde_json::{Value, json};
use std::path::Path;
#[cfg(all(feature = "whisper", feature = "sherpa"))]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Stereo 16 kHz WAV: left = 300 Hz "speech" bursts (mic), right = 500 Hz bursts (system).
fn write_stereo_wav(path: &Path, secs: u32) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..16_000 * secs {
        let t = i as f32 / 16_000.0;
        let env = |phase: f32| if ((t + phase) % 2.0) < 1.2 { 1.0 } else { 0.0 };
        let l = 0.25 * env(0.0) * (2.0 * std::f32::consts::PI * 300.0 * t).sin();
        let r = 0.25 * env(1.0) * (2.0 * std::f32::consts::PI * 500.0 * t).sin();
        w.write_sample((l * 32767.0) as i16).unwrap();
        w.write_sample((r * 32767.0) as i16).unwrap();
    }
    w.finalize().unwrap();
}

fn wav_backend(path: &Path, realtime: bool) -> Arc<WavBackend> {
    Arc::new(WavBackend {
        files: vec![
            (AudioChannel::Mic, path.to_path_buf(), Some(0)),
            (AudioChannel::System, path.to_path_buf(), Some(1)),
        ],
        realtime,
    })
}

fn mock_speech() -> SpeechOverride {
    let stt = Arc::new(MockStt::default());
    SpeechOverride {
        offline: stt.clone(),
        live: stt,
        diarizer: None,
    }
}

fn start_params(live: bool) -> Value {
    json!({
        "meeting_id": null,
        "create": { "title": "Capture test", "kind": { "type": "online", "app": null }, "calendar_event": null },
        "channels": ["mic", "system"],
        "mic_device": null,
        "system_device": null,
        "live_transcript": live,
    })
}

fn meeting(api: &Api, id: &Value) -> Meeting {
    let id = Id(id.as_str().unwrap().to_string());
    api.core.store().get_meeting(&id).unwrap().expect("meeting")
}

/// Run a registered processor the way the job runner does.
async fn run_processor(api: &Api, name: &str, m: &Meeting) -> almond_types::Result<Vec<Artifact>> {
    let p = api.core.registry.get(name).expect("processor registered");
    let ctx = JobCtx {
        job_id: Id::new(),
        meeting: m.clone(),
        params: json!({}),
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(|f, msg| eprintln!("  progress {:.0}% {msg}", f * 100.0)),
        blobs: api.core.blobs().clone() as Arc<dyn BlobStore>,
    };
    let drafts = p.run(&ctx, vec![]).await?;
    Ok(drafts
        .into_iter()
        .map(|d| d.into_artifact(Id::new(), Default::default(), api.core.device_id().clone()))
        .collect())
}

async fn open_at(dir: &Path) -> Api {
    let mut cfg = CoreConfig::new(dir, "Test device");
    cfg.network = false;
    Core::open(cfg).await.expect("open core")
}

#[tokio::test(flavor = "multi_thread")]
async fn records_wav_pair_into_meeting_and_transcribes() {
    let (api, dir) = common::open_core("capture-rec").await;
    let wav = dir.join("in.wav");
    write_stereo_wav(&wav, 5);
    api.core
        .capture
        .set_audio_backend(Some(wav_backend(&wav, true)));
    api.core.capture.set_speech_override(Some(mock_speech()));
    let mut rx = api.subscribe();

    let st = api
        .call("recording.start", start_params(true))
        .await
        .unwrap();
    assert_eq!(st["state"], "recording");
    let mid = st["meeting_id"].clone();
    let err = api
        .call("recording.start", start_params(false))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32602, "{err}");

    tokio::time::sleep(Duration::from_millis(5600)).await;
    let st = api.call("recording.status", Value::Null).await.unwrap();
    assert_eq!(st["state"], "recording");
    let st = api.call("recording.stop", Value::Null).await.unwrap();
    assert_eq!(st["state"], "idle");
    assert_eq!(st["meeting_id"], mid);

    // meeting: two ~5 s Opus tracks, ended, local time zone
    let m = meeting(&api, &mid);
    assert_eq!(m.title, "Capture test");
    assert!(m.ended_at.is_some());
    assert!(m.tz.is_some(), "tz");
    assert_eq!(m.audio.len(), 2, "{:?}", m.audio);
    for t in &m.audio {
        assert_eq!(t.codec, "opus");
        assert!(
            (4800..=5300).contains(&t.duration_ms),
            "{:?} {}",
            t.channel,
            t.duration_ms
        );
        assert!(api.core.blobs().has(&t.blob).unwrap());
    }
    let known = api.core.store().known_blobs().unwrap();
    for t in &m.audio {
        let b = known
            .iter()
            .find(|b| b.hash == t.blob)
            .expect("blob.available");
        assert_eq!(b.mime, "audio/ogg");
        assert!(b.size > 1000);
    }
    // the session directory is gone once the tracks are stored
    assert!(
        !dir.join("recordings").join(m.id.as_str()).exists(),
        "recording dir left behind"
    );

    // notifications: status with levels on both channels, live captions for this meeting
    let (mut statuses, mut captions, mut max_level) = (0, 0, [0f64; 2]);
    while let Ok(n) = rx.try_recv() {
        match n.name {
            notifications::RECORDING_STATUS => {
                statuses += 1;
                for l in n.payload["levels"].as_array().unwrap() {
                    let i = (l[0] == "system") as usize;
                    max_level[i] = max_level[i].max(l[1].as_f64().unwrap());
                }
            }
            notifications::TRANSCRIPT_UPDATE => {
                assert_eq!(n.payload["meeting_id"], mid);
                captions += 1;
            }
            _ => {}
        }
    }
    assert!(statuses >= 15, "{statuses} status notifications in ~5.6 s");
    assert!(max_level[0] > 0.5 && max_level[1] > 0.5, "{max_level:?}");
    assert!(captions > 0);

    // transcribe.local (mock engine)
    let arts = run_processor(&api, TRANSCRIBE_LOCAL, &m).await.unwrap();
    assert_eq!(arts.len(), 1);
    let t: Transcript = arts[0].decode().unwrap();
    assert!(!t.segments.is_empty());
    // (the mock returns the same text on both channels at the same time, so the mic copy is
    // dropped as echo of the system channel; real mic speech is labelled "me")
    assert!(t.segments.iter().any(|s| s.channel == AudioChannel::System));
    assert!(t.speakers.iter().any(|s| s.id.0.starts_with("remote")));
    assert_eq!(arts[0].provenance.processor, TRANSCRIBE_LOCAL);

    // audio.url: one channel = its blob; both = the mixed playback file
    let u = api
        .call("audio.url", json!({ "meeting_id": mid, "channel": "mic" }))
        .await
        .unwrap();
    let url = u["url"].as_str().unwrap();
    assert!(url.starts_with("http://asset.localhost/") || url.starts_with("asset://localhost/"));
    let u = api
        .call("audio.url", json!({ "meeting_id": mid }))
        .await
        .unwrap();
    assert!(u["url"].as_str().unwrap().contains("playback"), "{u}");
    let mixed: Vec<_> = std::fs::read_dir(dir.join("playback")).unwrap().collect();
    assert_eq!(mixed.len(), 1);
    let err = api
        .call("audio.url", json!({ "meeting_id": "nope" }))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32004);

    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn records_into_an_existing_meeting_without_system_audio() {
    let (api, dir) = common::open_core("capture-existing").await;
    let wav = dir.join("in.wav");
    write_stereo_wav(&wav, 2);
    // only a mic "device": the system channel is skipped, the mic still records
    api.core
        .capture
        .set_audio_backend(Some(Arc::new(WavBackend {
            files: vec![(AudioChannel::Mic, wav.clone(), Some(0))],
            realtime: true,
        })));
    let id = Id::new();
    let mut p = start_params(false);
    p["meeting_id"] = json!(id);
    let st = api.call("recording.start", p).await.unwrap();
    assert_eq!(st["meeting_id"], json!(id));
    tokio::time::sleep(Duration::from_millis(2300)).await;
    api.call("recording.stop", Value::Null).await.unwrap();
    let m = meeting(&api, &json!(id));
    assert_eq!(m.audio.len(), 1);
    assert_eq!(m.audio[0].channel, AudioChannel::Mic);
    // stop without a recording is a no-op
    let st = api.call("recording.stop", Value::Null).await.unwrap();
    assert_eq!(st["state"], "idle");
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_recording_is_recovered_at_next_start() {
    let dir = std::env::temp_dir().join(format!(
        "almond-core-test-capture-recover-{}-{}",
        std::process::id(),
        almond_types::time::now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("in.wav");
    write_stereo_wav(&wav, 4);
    let api = open_at(&dir).await;
    api.core
        .capture
        .set_audio_backend(Some(wav_backend(&wav, true)));
    let st = api
        .call("recording.start", start_params(false))
        .await
        .unwrap();
    let mid = st["meeting_id"].clone();
    tokio::time::sleep(Duration::from_millis(3000)).await;
    assert!(meeting(&api, &mid).audio.is_empty());
    // "crash": the app shuts down without recording.stop
    api.core.shutdown().await;
    drop(api);
    tokio::time::sleep(Duration::from_millis(500)).await; // capture threads wind down

    let api = open_at(&dir).await;
    let mut m = meeting(&api, &mid);
    for _ in 0..100 {
        if !m.audio.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        m = meeting(&api, &mid);
    }
    assert_eq!(m.audio.len(), 2, "recovered tracks");
    for t in &m.audio {
        // ≤ 1 s may be lost (Ogg pages are flushed every second)
        assert!((1500..=3300).contains(&t.duration_ms), "{}", t.duration_ms);
    }
    assert!(m.ended_at.is_some());
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(not(any(feature = "whisper", feature = "sherpa")))]
#[tokio::test(flavor = "multi_thread")]
async fn transcribe_without_engines_is_unsupported() {
    let (api, dir) = common::open_core("capture-unsupported").await;
    let wav = dir.join("in.wav");
    write_stereo_wav(&wav, 2);
    api.core
        .capture
        .set_audio_backend(Some(wav_backend(&wav, false)));
    let st = api
        .call("recording.start", start_params(false))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    api.call("recording.stop", Value::Null).await.unwrap();
    let m = meeting(&api, &st["meeting_id"]);
    let err = run_processor(&api, TRANSCRIBE_LOCAL, &m).await.unwrap_err();
    assert!(
        matches!(err, almond_types::Error::Unsupported(_)),
        "{err:?}"
    );
    assert!(err.to_string().contains("--features"), "{err}");
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn speech_models_lists_registry() {
    let (api, dir) = common::open_core("capture-models").await;
    let models = almond_core::capture::speech_models(&api.core).unwrap();
    assert!(
        models
            .iter()
            .any(|m| m.id == "sherpa:zipformer-vi-30m-int8" && m.kind == "stt")
    );
    assert!(models.iter().any(|m| m.kind == "diarize"));
    let err = almond_core::capture::download_speech_model(&api.core, "no:such-model")
        .await
        .unwrap_err();
    assert!(matches!(err, almond_types::Error::NotFound(_)), "{err:?}");
    let devs = api.call("audio.devices", Value::Null).await;
    // real devices may or may not exist on a CI runner; the call must not panic
    let _ = devs;
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

// ------------------------------------------------------------------------------------------
// Real models / devices on the dev PC (ignored)

#[cfg(all(feature = "whisper", feature = "sherpa"))]
fn eval_meeting() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap();
    home.join(".almond")
        .join("eval")
        .join("stt")
        .join("meeting")
        .join("meeting_stereo.wav")
}

/// Record 15 s of the synthetic meeting (mic left, system right) at real-time pace through the
/// core, with live captions from the real engines, then run `transcribe.local` on it.
#[cfg(all(feature = "whisper", feature = "sherpa"))]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs speech models and native engines"]
async fn real_engines_record_caption_transcribe() {
    let (api, dir) = common::open_core("capture-real").await;
    api.core
        .capture
        .set_audio_backend(Some(wav_backend(&eval_meeting(), true)));
    let mut rx = api.subscribe();
    let st = api
        .call("recording.start", start_params(true))
        .await
        .unwrap();
    let mid = st["meeting_id"].clone();
    tokio::time::sleep(Duration::from_secs(15)).await;
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
    eprintln!("live finals: {finals:#?}");
    assert!(!finals.is_empty(), "no live caption finals");
    let m = meeting(&api, &mid);
    let t0 = std::time::Instant::now();
    let arts = run_processor(&api, TRANSCRIBE_LOCAL, &m).await.unwrap();
    let t: Transcript = arts[0].decode().unwrap();
    eprintln!(
        "transcribe.local: {:.1} s, model {:?}",
        t0.elapsed().as_secs_f64(),
        arts[0].provenance.model
    );
    for s in &t.segments {
        eprintln!(
            "  [{:>6}-{:>6}] {:?} {:?}: {}",
            s.t0_ms, s.t1_ms, s.channel, s.speaker, s.text
        );
    }
    assert!(!t.segments.is_empty());
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

/// Post-meeting pass on the 30-min synthetic meeting: import both channels as Opus tracks (the
/// same files a recording produces), run `transcribe.local`, print timings, and write the
/// transcript to `$ALMOND_TRANSCRIPT_OUT` for `eval/stt/score_meeting.py`.
#[cfg(all(feature = "whisper", feature = "sherpa"))]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs speech models, native engines and the eval meeting (~5 min)"]
async fn post_meeting_pass_30min() {
    use almond_audio::recorder::{RecorderConfig, SessionRecorder};
    use almond_types::audio::AudioStream;
    use almond_types::event::EventBody;

    let (api, dir) = common::open_core("capture-30min").await;
    let wav = eval_meeting();
    // import (offline decode → Opus chunks → blob store), as a finished recording would leave it
    let sess = dir.join("import");
    let mut rec = SessionRecorder::create(&sess, 0, RecorderConfig::default()).unwrap();
    for (ch, src) in [(AudioChannel::Mic, 0), (AudioChannel::System, 1)] {
        let mut s = almond_audio::open_wav_channel(&wav, ch, 100, Some(src)).unwrap();
        while let Some(f) = s.next().await {
            rec.push(&f.unwrap()).unwrap();
        }
    }
    let tracks = rec.finish(api.core.blobs().as_ref()).unwrap();
    let store = api.core.store();
    for t in &tracks {
        store
            .append_local(EventBody::BlobAvailable {
                hash: t.blob.clone(),
                size: 0,
                mime: "audio/ogg".into(),
            })
            .unwrap();
    }
    let m = Meeting {
        id: Id::new(),
        title: "30-min eval meeting".into(),
        started_at: almond_types::time::now_ms(),
        ended_at: None,
        kind: almond_types::meeting::MeetingKind::Online { app: None },
        calendar_event: None,
        languages: vec![],
        audio: tracks,
        notes_doc: Id::new(),
        tags: vec![],
        tz: None,
    };
    store
        .append_local(EventBody::MeetingUpsert(m.clone()))
        .unwrap();

    let t0 = std::time::Instant::now();
    let arts = run_processor(&api, TRANSCRIBE_LOCAL, &m).await.unwrap();
    let secs = t0.elapsed().as_secs_f64();
    let t: Transcript = arts[0].decode().unwrap();
    eprintln!(
        "post-meeting pass: {secs:.1} s ({:.1} min) for 29.7 min of audio; {} segments, {} speakers; model {:?}",
        secs / 60.0,
        t.segments.len(),
        t.speakers.len(),
        arts[0].provenance.model
    );
    if let Some(out) = std::env::var_os("ALMOND_TRANSCRIPT_OUT") {
        std::fs::write(out, serde_json::to_string_pretty(&t).unwrap()).unwrap();
    }
    assert!(t.segments.len() > 100);
    assert!(secs < 4.5 * 60.0, "post-meeting pass took {secs:.0} s");
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

/// ~10 s from this PC's real microphone + system loopback through the core, with a beep pattern
/// played on the default output so the loopback has signal. Prints levels and track durations.
#[cfg(not(target_os = "android"))]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs real audio devices"]
async fn real_devices_10s() {
    let (api, dir) = common::open_core("capture-devices").await;
    let devs = api.call("audio.devices", Value::Null).await.unwrap();
    eprintln!("devices: {}", serde_json::to_string_pretty(&devs).unwrap());
    let mut rx = api.subscribe();
    let st = api
        .call("recording.start", start_params(false))
        .await
        .unwrap();
    let mid = st["meeting_id"].clone();
    let tone = std::thread::spawn(|| play_beeps(Duration::from_secs(9)));
    tokio::time::sleep(Duration::from_secs(10)).await;
    let _ = tone.join();
    api.call("recording.stop", Value::Null).await.unwrap();
    let mut max = std::collections::BTreeMap::<String, f64>::new();
    let mut n = 0;
    while let Ok(x) = rx.try_recv() {
        if x.name == notifications::RECORDING_STATUS {
            n += 1;
            for l in x.payload["levels"].as_array().unwrap() {
                let e = max.entry(l[0].as_str().unwrap().into()).or_default();
                *e = e.max(l[1].as_f64().unwrap());
            }
        }
    }
    let m = meeting(&api, &mid);
    eprintln!("{n} status notifications; max meter level per channel: {max:?}");
    for t in &m.audio {
        let p = api.core.blobs().path(&t.blob).unwrap().unwrap();
        let x = almond_audio::recorder::decode_track(&p, almond_audio::Codec::Opus).unwrap();
        let zeros = x.iter().filter(|s| **s == 0.0).count() as f64 / x.len().max(1) as f64;
        eprintln!(
            "track {:?}: {} ms, {} (offset {} ms), RMS {:.1} dBFS, peak {:.1} dBFS, {:.0}% exact zeros",
            t.channel,
            t.duration_ms,
            t.codec,
            t.offset_ms,
            almond_audio::level::rms_dbfs(&x),
            almond_audio::level::peak_dbfs(&x),
            zeros * 100.0
        );
    }
    assert!(!m.audio.is_empty());
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

/// 660 Hz, 0.4 s on / 0.6 s off on the default output device.
#[cfg(not(target_os = "android"))]
fn play_beeps(d: Duration) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let Some(dev) = cpal::default_host().default_output_device() else {
        eprintln!("no output device: loopback will be silent");
        return;
    };
    let Ok(sc) = dev.default_output_config() else {
        return;
    };
    if sc.sample_format() != cpal::SampleFormat::F32 {
        eprintln!("output format {:?}: no beeps", sc.sample_format());
        return;
    }
    let cfg = sc.config();
    let (rate, ch) = (cfg.sample_rate as f32, cfg.channels as usize);
    let mut n: u64 = 0;
    let s = dev
        .build_output_stream::<f32, _, _>(
            cfg,
            move |out: &mut [f32], _| {
                for frame in out.chunks_mut(ch) {
                    let t = n as f32 / rate;
                    let v = if (t % 1.0) < 0.4 {
                        0.2 * (2.0 * std::f32::consts::PI * 660.0 * t).sin()
                    } else {
                        0.0
                    };
                    frame.fill(v);
                    n += 1;
                }
            },
            |e| eprintln!("beep stream: {e}"),
            None,
        )
        .ok();
    if let Some(s) = &s {
        let _ = s.play();
    }
    std::thread::sleep(d);
}
