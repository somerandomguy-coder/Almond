//! `meetings.import` through the core API: WAV generated here, Ogg Opus written with the crate's
//! own Opus writer, and the small committed fixtures in `tests/fixtures/import/` (2 s tone bursts
//! in MP3, M4A/AAC, FLAC, Ogg Vorbis, Ogg Opus, MP4 + MOV with AAC and a video track, MKV Vorbis,
//! WebM Opus with a video track, and an AC-3 MKV for the ffmpeg fallback). Then the meeting,
//! its `mixed` Opus track, `blob.available`, `import.progress`, cancel, and that processing
//! plans `transcribe.local` (which runs on the imported audio with the mock speech engines).
//!
//! A long-file check (time + memory) is `#[ignore]`d and reads its file from `ALMOND_IMPORT_LONG`:
//! ```text
//! set ALMOND_IMPORT_LONG=C:\path\to\2h.mp3
//! cargo test --release -p almond-core --test import -- --ignored --nocapture
//! ```

mod common;

use almond_audio::recorder::{Codec, decode_track};
use almond_core::Api;
use almond_core::capture::import::{IMPORT_PROGRESS, ImportProgress, ImportStage};
use almond_core::capture::{SpeechOverride, TRANSCRIBE_LOCAL};
use almond_types::Artifact;
use almond_types::artifact::body::Transcript;
use almond_types::meeting::{AudioChannel, Meeting, MeetingKind};
use almond_types::mock::MockStt;
use almond_types::pipeline::JobCtx;
use almond_types::store::{BlobStore, Store};
use almond_types::{Id, LangTag};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/import")
        .join(name)
}

/// `secs` of 300 Hz (left) / 500 Hz (right) bursts: 0.6 s on, 0.4 s off, every second.
fn write_wav(path: &Path, rate: u32, channels: u16, secs: f32) {
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..(rate as f32 * secs) as u32 {
        let t = i as f32 / rate as f32;
        let env = if (t % 1.0) < 0.6 { 1.0 } else { 0.0 };
        for c in 0..channels {
            let hz = if c == 0 { 300.0 } else { 500.0 };
            let x = 0.3 * env * (2.0 * std::f32::consts::PI * hz * t).sin();
            w.write_sample((x * 32767.0) as i16).unwrap();
        }
    }
    w.finalize().unwrap();
}

fn mock_speech() -> SpeechOverride {
    let stt = Arc::new(MockStt::default());
    SpeechOverride {
        offline: stt.clone(),
        live: stt,
        diarizer: None,
    }
}

async fn import(api: &Api, params: Value) -> Result<Meeting, almond_core::ApiError> {
    api.call("meetings.import", params)
        .await
        .map(|v| serde_json::from_value(v).unwrap())
}

/// The track's audio decoded back to 16 kHz.
fn track_audio(api: &Api, m: &Meeting) -> Vec<f32> {
    assert_eq!(m.audio.len(), 1, "{:?}", m.audio);
    let p = api.core.blobs().path(&m.audio[0].blob).unwrap().unwrap();
    decode_track(&p, Codec::Opus).unwrap()
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// Bursts on during [0, 0.6) s of each second, silent during [0.6, 1): checks level and timing.
fn assert_bursts(name: &str, x: &[f32], secs: usize) {
    for s in 0..secs {
        let on = &x[s * 16_000 + 1_600..s * 16_000 + 8_000]; // 0.1–0.5 s
        let off = &x[s * 16_000 + 11_200..s * 16_000 + 14_400]; // 0.7–0.9 s
        assert!(rms(on) > 0.08, "{name}: second {s} on-rms {}", rms(on));
        assert!(rms(off) < 0.03, "{name}: second {s} off-rms {}", rms(off));
    }
}

fn assert_imported(api: &Api, m: &Meeting, expect_ms: u32, tol_ms: u32) {
    assert_eq!(m.kind, MeetingKind::Imported);
    assert!(m.tz.is_some());
    let t = &m.audio[0];
    assert_eq!(t.channel, AudioChannel::Mixed);
    assert_eq!(t.codec, "opus");
    assert_eq!(t.sample_rate, 16_000);
    assert!(
        t.duration_ms.abs_diff(expect_ms) <= tol_ms,
        "duration {} vs {expect_ms}",
        t.duration_ms
    );
    assert_eq!(
        m.ended_at.unwrap() - m.started_at,
        t.duration_ms as i64,
        "ended_at - started_at = duration"
    );
    assert!(api.core.blobs().has(&t.blob).unwrap());
    let known = api.core.store().known_blobs().unwrap();
    let b = known
        .iter()
        .find(|b| b.hash == t.blob)
        .expect("blob.available");
    assert_eq!(b.mime, "audio/ogg");
    assert!(b.size > 500);
    // stored meeting == returned meeting
    assert_eq!(
        api.core.store().get_meeting(&m.id).unwrap().as_ref(),
        Some(m)
    );
}

async fn run_processor(api: &Api, name: &str, m: &Meeting) -> Vec<Artifact> {
    let p = api.core.registry.get(name).expect("processor registered");
    let ctx = JobCtx {
        job_id: Id::new(),
        meeting: m.clone(),
        params: json!({}),
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(|_, _| {}),
        blobs: api.core.blobs().clone() as Arc<dyn BlobStore>,
    };
    p.run(&ctx, vec![])
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.into_artifact(Id::new(), Default::default(), api.core.device_id().clone()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn imports_wav_and_processing_plans_transcription() {
    let (api, dir) = common::open_core("import-wav").await;
    api.core.capture.set_speech_override(Some(mock_speech()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("Weekly sync.wav");
    write_wav(&wav, 44_100, 2, 5.0);
    let mtime = std::fs::metadata(&wav)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let mut rx = api.subscribe();

    let m = import(&api, json!({ "path": wav, "language": "vi" }))
        .await
        .unwrap();
    assert_eq!(m.title, "Weekly sync");
    assert_eq!(m.languages, vec![LangTag::vi()]);
    assert_eq!(m.ended_at, Some(mtime), "file time = end of the meeting");
    assert_imported(&api, &m, 5_000, 30);
    let x = track_audio(&api, &m);
    assert!(x.len().abs_diff(80_000) < 500, "{} samples", x.len());
    assert_bursts("wav", &x, 5);

    // import.progress: decoding … storing, done; all for this meeting
    let mut stages = vec![];
    while let Ok(n) = rx.try_recv() {
        if n.name == IMPORT_PROGRESS {
            let p: ImportProgress = serde_json::from_value(n.payload).unwrap();
            assert_eq!(p.meeting_id, m.id);
            assert_eq!(p.file_name, "Weekly sync.wav");
            stages.push(p.stage);
        }
    }
    assert_eq!(stages.first(), Some(&ImportStage::Decoding));
    assert_eq!(stages.last(), Some(&ImportStage::Done));
    assert!(stages.contains(&ImportStage::Storing));

    // processing plans transcribe.local for the imported meeting
    let t0 = Instant::now();
    loop {
        let jobs = api.call("jobs.list", json!({})).await.unwrap();
        let planned = jobs.as_array().unwrap().iter().any(|j| {
            j["job"]["processor"] == TRANSCRIBE_LOCAL && j["job"]["meeting_id"] == json!(m.id)
        });
        if planned {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "transcribe.local not planned: {jobs:#}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // …and transcribes the imported track (mock engines)
    let arts = run_processor(&api, TRANSCRIBE_LOCAL, &m).await;
    let t: Transcript = arts[0].decode().unwrap();
    assert!(!t.segments.is_empty());
    assert!(t.segments.iter().all(|s| s.channel == AudioChannel::Mixed));

    // listed like any meeting
    let list = api
        .call("meetings.list", json!({ "text": null, "from": null, "to": null, "tag": null, "limit": 10, "offset": null }))
        .await
        .unwrap();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"] == json!(m.id))
    );
    assert!(
        std::fs::read_dir(dir.join("imports"))
            .map(|d| d.count() == 0)
            .unwrap_or(true),
        "work files left behind"
    );

    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn imports_ogg_opus_written_by_almond_audio() {
    let (api, dir) = common::open_core("import-opus").await;
    std::fs::create_dir_all(&dir).unwrap();
    // 3 s of bursts through the recorder's own Opus writer (16 kHz mono, one chunk)
    let mut x = vec![];
    for i in 0..48_000 {
        let t = i as f32 / 16_000.0;
        let env = if (t % 1.0) < 0.6 { 1.0 } else { 0.0 };
        x.push(0.3 * env * (2.0 * std::f32::consts::PI * 300.0 * t).sin());
    }
    let mut w = almond_audio::recorder::ogg_opus::OpusChannelWriter::create(
        &dir,
        "src",
        3600,
        24_000,
        Duration::from_secs(60),
    )
    .unwrap();
    w.push(&x).unwrap();
    w.finish().unwrap();
    let src = dir.join("talk.opus");
    std::fs::rename(dir.join("src-00000.opus"), &src).unwrap();

    let m = import(&api, json!({ "path": src, "title": "  My talk " }))
        .await
        .unwrap();
    assert_eq!(m.title, "My talk");
    assert!(m.languages.is_empty());
    assert_imported(&api, &m, 3_000, 30);
    assert_bursts("opus", &track_audio(&api, &m), 3);
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn imports_compressed_fixtures() {
    let (api, dir) = common::open_core("import-fixtures").await;
    for name in [
        "tone.mp3",
        "tone.m4a",
        "tone.flac",
        "tone.ogg",
        "tone.opus",
        "tone.mp4",
        "tone.mov",
        "tone.mkv",
        "tone.webm",
    ] {
        let m = import(&api, json!({ "path": fixture(name), "language": "en" }))
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        // encoders pad (AAC/MP3 priming), so allow a frame or two
        assert_imported(&api, &m, 2_000, 120);
        assert_eq!(m.title, "tone");
        let x = track_audio(&api, &m);
        eprintln!("{name}: {} ms, rms {:.3}", m.audio[0].duration_ms, rms(&x));
        assert_bursts(name, &x, 2);
    }
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

fn ffmpeg_on_path() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_codec_needs_ffmpeg() {
    let (api, dir) = common::open_core("import-ac3").await;
    let r = import(&api, json!({ "path": fixture("tone_ac3.mkv") })).await;
    if ffmpeg_on_path() {
        let m = r.expect("AC-3 through ffmpeg");
        assert_imported(&api, &m, 2_000, 120);
    } else {
        let e = r.unwrap_err();
        assert_eq!(e.code, -32005, "{e}");
        assert!(
            e.message.contains("MKV") && e.message.contains("ffmpeg"),
            "{e}"
        );
        assert!(
            api.core
                .store()
                .list_meetings(&Default::default())
                .unwrap()
                .is_empty()
        );
    }
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_input_is_rejected() {
    let (api, dir) = common::open_core("import-bad").await;
    std::fs::create_dir_all(&dir).unwrap();
    let e = import(&api, json!({ "path": dir.join("missing.wav") }))
        .await
        .unwrap_err();
    assert_eq!(e.code, -32004, "{e}");
    let wav = dir.join("a.wav");
    write_wav(&wav, 16_000, 1, 1.0);
    let e = import(&api, json!({ "path": wav, "language": "fr" }))
        .await
        .unwrap_err();
    assert_eq!(e.code, -32602, "{e}");
    let junk = dir.join("junk.mp3");
    std::fs::write(&junk, vec![0x5au8; 4096]).unwrap();
    let e = import(&api, json!({ "path": junk })).await.unwrap_err();
    assert!(e.code == -32005 || e.code == -32602, "{e}");
    assert!(
        api.core
            .store()
            .list_meetings(&Default::default())
            .unwrap()
            .is_empty()
    );
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn import_can_be_cancelled() {
    let (api, dir) = common::open_core("import-cancel").await;
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("long.wav");
    write_wav(&wav, 16_000, 1, 600.0); // 10 min
    let mut rx = api.subscribe();
    let api2 = api.clone();
    let wav2 = wav.clone();
    let task = tokio::spawn(async move { import(&api2, json!({ "path": wav2 })).await });
    // the first progress notification names the meeting id to cancel
    let id = loop {
        let n = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if n.name == IMPORT_PROGRESS {
            break n.payload["meeting_id"].clone();
        }
    };
    let ok = api
        .call("meetings.importCancel", json!({ "meeting_id": id }))
        .await
        .unwrap();
    assert_eq!(ok, json!(true));
    let r = task.await.unwrap();
    assert!(r.is_err(), "cancelled import returned a meeting");
    let mut last = None;
    while let Ok(n) = rx.try_recv() {
        if n.name == IMPORT_PROGRESS {
            last = Some(n.payload["stage"].clone());
        }
    }
    assert_eq!(last, Some(json!("cancelled")));
    assert!(
        api.core
            .store()
            .list_meetings(&Default::default())
            .unwrap()
            .is_empty()
    );
    assert!(
        std::fs::read_dir(dir.join("imports"))
            .map(|d| d.count() == 0)
            .unwrap_or(true)
    );
    // unknown id: nothing to cancel
    let ok = api
        .call("meetings.importCancel", json!({ "meeting_id": "nope" }))
        .await
        .unwrap();
    assert_eq!(ok, json!(false));
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

/// Long-file check: time and progress for the file in `ALMOND_IMPORT_LONG` (watch the process's
/// peak memory from outside; the import streams, so it should stay flat).
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn long_file_import() {
    let Ok(path) = std::env::var("ALMOND_IMPORT_LONG") else {
        eprintln!("set ALMOND_IMPORT_LONG to an audio/video file");
        return;
    };
    let (api, dir) = common::open_core("import-long").await;
    let mut rx = api.subscribe();
    let counter = tokio::spawn(async move {
        let mut n = 0;
        let mut last = None;
        while let Ok(x) = rx.recv().await {
            if x.name == IMPORT_PROGRESS {
                n += 1;
                last = Some(x.payload);
            }
        }
        (n, last)
    });
    let t0 = Instant::now();
    let m = import(&api, json!({ "path": path })).await.unwrap();
    let took = t0.elapsed();
    let size = std::fs::metadata(api.core.blobs().path(&m.audio[0].blob).unwrap().unwrap())
        .unwrap()
        .len();
    eprintln!(
        "imported {:.1} min of audio in {:.1} s ({:.0}x realtime); track {:.1} MB",
        m.audio[0].duration_ms as f64 / 60_000.0,
        took.as_secs_f64(),
        m.audio[0].duration_ms as f64 / 1000.0 / took.as_secs_f64(),
        size as f64 / 1e6
    );
    api.core.shutdown().await;
    drop(api);
    if let Ok(Ok((n, last))) = tokio::time::timeout(Duration::from_secs(5), counter).await {
        eprintln!("{n} import.progress notifications; last {last:?}");
    }
    let _ = std::fs::remove_dir_all(dir);
}
