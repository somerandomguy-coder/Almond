//! Deleting meeting audio to save space: per meeting, ⭐ keep, retention, and other devices.

mod common;

use almond_core::Api;
use almond_types::event::EventBody;
use almond_types::meeting::{AudioChannel, AudioTrack};
use almond_types::mock::fixtures;
use almond_types::store::{BlobStore, DELETED_BLOBS, Store};
use almond_types::sync::VersionVector;
use almond_types::{BlobHash, Id};
use common::open_core;
use serde_json::{Value, json};

async fn call(api: &Api, m: &str, p: Value) -> Value {
    match api.call(m, p.clone()).await {
        Ok(v) => v,
        Err(e) => panic!("{m}({p}) failed: {e}"),
    }
}

/// A finished meeting with two audio tracks (and a transcript if `transcribed`).
fn meeting_with_audio(api: &Api, bytes: usize, transcribed: bool) -> (Id, Vec<BlobHash>) {
    meeting_with_audio_salt(api, bytes, transcribed, bytes as u8)
}

fn meeting_with_audio_salt(
    api: &Api,
    bytes: usize,
    transcribed: bool,
    salt: u8,
) -> (Id, Vec<BlobHash>) {
    let core = &api.core;
    let mut m = fixtures::meeting(almond_types::time::now_ms() - 3_600_000);
    let mut hashes = vec![];
    for (i, ch) in [AudioChannel::Mic, AudioChannel::System]
        .into_iter()
        .enumerate()
    {
        let mut data = vec![i as u8 + 1; bytes];
        data[0] = salt;
        let h = core.blobs().put(&data).unwrap();
        core.store()
            .append_local(EventBody::BlobAvailable {
                hash: h.clone(),
                size: bytes as u64,
                mime: "audio/ogg".into(),
            })
            .unwrap();
        m.audio.push(AudioTrack {
            channel: ch,
            blob: h.clone(),
            codec: "opus".into(),
            sample_rate: 16_000,
            offset_ms: 0,
            duration_ms: 60_000,
        });
        hashes.push(h);
    }
    core.store()
        .append_local(EventBody::MeetingUpsert(m.clone()))
        .unwrap();
    if transcribed {
        core.store()
            .append_local(EventBody::ArtifactAdd(fixtures::transcript(
                &m.id,
                core.device_id(),
            )))
            .unwrap();
    }
    (m.id, hashes)
}

fn row(list: &Value, id: &Id) -> Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|r| r["meeting_id"] == id.as_str())
        .cloned()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_audio_keep_text_and_star() {
    let (api, dir) = open_core("storage").await;
    let core = api.core.clone();
    let (m1, h1) = meeting_with_audio(&api, 50_000, true);
    let (m2, _) = meeting_with_audio(&api, 20_000, false);

    // Listing: largest audio first, with transcript state.
    let list = call(&api, "storage.meetings", json!({})).await;
    assert_eq!(list[0]["meeting_id"], m1.as_str());
    assert_eq!(row(&list, &m1)["audio_bytes"], 100_000);
    assert!(row(&list, &m1)["transcribed_at"].is_number());
    assert!(row(&list, &m2)["transcribed_at"].is_null());

    // Not transcribed: refused unless forced; transcribed: deleted, text kept.
    let r = call(
        &api,
        "storage.deleteAudio",
        json!({ "meeting_ids": [m1, m2] }),
    )
    .await;
    assert_eq!(r["deleted"], 1);
    assert_eq!(r["freed_bytes"], 100_000);
    assert_eq!(r["skipped"][0][0], m2.as_str());
    for h in &h1 {
        assert!(!core.blobs().has(h).unwrap(), "audio file deleted");
        assert!(
            core.store()
                .get_record(DELETED_BLOBS, &h.0)
                .unwrap()
                .is_some()
        );
    }
    assert_eq!(
        core.store()
            .list_artifacts(&m1, Some("transcript"))
            .unwrap()
            .len(),
        1,
        "the transcript stays"
    );
    let st = call(&api, "storage.audioState", json!({ "id": m1 })).await;
    assert!(st["deleted_at"].is_number());
    assert_eq!(st["deleted_reason"], "manual");
    let url = call(&api, "audio.url", json!({ "meeting_id": m1 })).await;
    assert!(url["url"].is_null(), "nothing to play any more: {url}");

    // ⭐ keep + retention: only the unkept, transcribed, old-enough meeting goes.
    let (m3, h3) = meeting_with_audio_salt(&api, 10_000, true, 3);
    let (m4, h4) = meeting_with_audio_salt(&api, 10_000, true, 4);
    // a copy of m3's audio imported again as m5: deleting m5's audio must not touch m3's file
    let (m5, h5) = meeting_with_audio_salt(&api, 10_000, true, 3);
    assert_eq!(h3, h5);
    let r = call(&api, "storage.deleteAudio", json!({ "meeting_ids": [m5] })).await;
    assert_eq!(r["freed_bytes"], 0, "shared file kept: {r}");
    assert!(h3.iter().all(|h| core.blobs().has(h).unwrap()));
    call(
        &api,
        "storage.keepAudio",
        json!({ "meeting_id": m3, "keep": true }),
    )
    .await;
    call(
        &api,
        "settings.set",
        json!({ "key": "storage.audio_retention_days", "value": 0.00001 }),
    )
    .await; // ~1 s
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let r = call(&api, "storage.cleanup", json!({})).await;
    assert_eq!(r["deleted"], 1, "{r}");
    assert!(h3.iter().all(|h| core.blobs().has(h).unwrap()), "kept");
    assert!(
        h4.iter().all(|h| !core.blobs().has(h).unwrap()),
        "retention"
    );
    assert_eq!(
        call(&api, "storage.audioState", json!({ "id": m4 })).await["deleted_reason"],
        "retention"
    );
    let _ = m3;

    // Another device that has the audio deletes its copy once the tombstones arrive.
    let (other, dir2) = open_core("storage-peer").await;
    let (_, hp) = meeting_with_audio(&other, 5_000, true);
    let salted = |v: u8| {
        let mut d = vec![v; 50_000];
        d[0] = 50_000usize as u8;
        d
    };
    for (h, data) in h1.iter().zip([salted(1), salted(2)]) {
        assert_eq!(&other.core.blobs().put(&data).unwrap(), h);
    }
    let evs = core
        .store()
        .events_missing(&VersionVector::default(), usize::MAX)
        .unwrap();
    other.core.store().apply_remote(evs).unwrap();
    let r = call(&other, "storage.cleanup", json!({})).await;
    assert!(r["freed_bytes"].as_u64().unwrap() >= 100_000, "{r}");
    assert!(h1.iter().all(|h| !other.core.blobs().has(h).unwrap()));
    assert!(
        hp.iter().all(|h| other.core.blobs().has(h).unwrap()),
        "its own audio stays"
    );

    api.core.shutdown().await;
    other.core.shutdown().await;
    drop((api, other));
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(dir2);
}
