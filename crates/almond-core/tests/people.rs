//! Feature "people" through the core API with synthetic audio and a mock voice embedder (each
//! "voice" is a sine tone; the embedding is the energy at a few frequencies): labelling a
//! speaker creates the person and a voice print, a second meeting with the same voice is
//! labelled automatically, calendar attendees match by email, user labels win, merge, the
//! person page, and people syncing to a second core.

mod common;

use almond_core::Api;
use almond_core::capture::voice::VoiceEmbedder;
use almond_types::artifact::body::{Segment, Speaker, SpeakerLabels, Transcript};
use almond_types::artifact::{ArtifactDraft, Provenance};
use almond_types::calendar::{Attendee, CalendarEvent};
use almond_types::event::EventBody;
use almond_types::ids::SpeakerId;
use almond_types::meeting::{AudioChannel, AudioTrack, EventRef};
use almond_types::mock::fixtures;
use almond_types::people::Person;
use almond_types::store::{BlobStore, Store};
use almond_types::time::{Hlc, now_ms};
use almond_types::{Error, Id, Result};
use common::open_core;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SR: usize = 16_000;
const LAN: f32 = 300.0;
const DAVID: f32 = 700.0;
const ME_VOICE: f32 = 500.0;
const STRANGER: f32 = 1100.0;
const AN: f32 = 1300.0;
const FREQS: [f32; 5] = [LAN, ME_VOICE, DAVID, STRANGER, AN];

/// Energy at each of `FREQS` (Goertzel): one tone → one dominant dimension.
struct ToneEmbedder;

impl VoiceEmbedder for ToneEmbedder {
    fn model(&self) -> String {
        "test:tones".into()
    }
    fn embed(&self, x: &[f32]) -> Result<Vec<f32>> {
        if x.len() < SR / 2 {
            return Err(Error::Invalid("too short".into()));
        }
        Ok(FREQS
            .iter()
            .map(|f| {
                let w = 2.0 * std::f32::consts::PI * f / SR as f32;
                let (mut s1, mut s2) = (0.0f32, 0.0f32);
                for &v in x {
                    let s = v + 2.0 * w.cos() * s1 - s2;
                    s2 = s1;
                    s1 = s;
                }
                (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2)
                    .max(0.0)
                    .sqrt()
            })
            .collect())
    }
}

/// `(voice Hz or 0 for silence, seconds)` pieces → samples.
fn audio(parts: &[(f32, usize)]) -> Vec<f32> {
    let mut out = vec![];
    for &(f, secs) in parts {
        for i in 0..secs * SR {
            let t = i as f32 / SR as f32;
            out.push(if f > 0.0 {
                0.3 * (2.0 * std::f32::consts::PI * f * t).sin()
            } else {
                0.0
            });
        }
    }
    out
}

fn wav_track(api: &Api, ch: AudioChannel, x: &[f32]) -> AudioTrack {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SR as u32,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = hound::WavWriter::new(&mut buf, spec).unwrap();
        for &v in x {
            w.write_sample((v * i16::MAX as f32) as i16).unwrap();
        }
        w.finalize().unwrap();
    }
    let blob = api.core.blobs().put(buf.get_ref()).unwrap();
    AudioTrack {
        channel: ch,
        blob,
        codec: "wav".into(),
        sample_rate: SR as u32,
        offset_ms: 0,
        duration_ms: (x.len() * 1000 / SR) as u32,
    }
}

/// A meeting `hours_ago` with mic + system audio and a transcript with `segs`
/// (`t0 s, t1 s, channel, speaker`). Returns (meeting id, transcript id).
fn add_meeting(
    api: &Api,
    hours_ago: i64,
    mic: &[(f32, usize)],
    system: &[(f32, usize)],
    segs: &[(u32, u32, AudioChannel, &str)],
    event: Option<EventRef>,
) -> (Id, Id) {
    let core = &api.core;
    let mut m = fixtures::meeting(now_ms() - hours_ago * 3_600_000);
    m.title = format!("meeting {hours_ago}h ago");
    m.calendar_event = event;
    m.audio = vec![
        wav_track(api, AudioChannel::Mic, &audio(mic)),
        wav_track(api, AudioChannel::System, &audio(system)),
    ];
    core.store()
        .append_local(EventBody::MeetingUpsert(m.clone()))
        .unwrap();
    let segments: Vec<Segment> = segs
        .iter()
        .enumerate()
        .map(|(i, (t0, t1, ch, spk))| Segment {
            idx: i as u32,
            t0_ms: t0 * 1000,
            t1_ms: t1 * 1000,
            channel: *ch,
            speaker: Some(SpeakerId(spk.to_string())),
            lang: None,
            text: format!("line {i}"),
            confidence: None,
            words: None,
        })
        .collect();
    let mut speakers: Vec<Speaker> = vec![];
    for s in &segments {
        let id = s.speaker.clone().unwrap();
        if !speakers.iter().any(|x| x.id == id) {
            speakers.push(Speaker {
                is_me: id.0 == "me",
                id,
                name: None,
                email: None,
            });
        }
    }
    let t = ArtifactDraft::new(
        m.id.clone(),
        &Transcript { segments, speakers },
        Provenance {
            processor: "transcribe.local".into(),
            ..Default::default()
        },
    )
    .unwrap()
    .into_artifact(
        Id::new(),
        Hlc {
            wall_ms: now_ms(),
            logical: 0,
        },
        core.device_id().clone(),
    );
    core.store()
        .append_local(EventBody::ArtifactAdd(t.clone()))
        .unwrap();
    (m.id, t.id)
}

async fn call(api: &Api, m: &str, p: Value) -> Value {
    match api.call(m, p.clone()).await {
        Ok(v) => v,
        Err(e) => panic!("{m}({p}) failed: {e}"),
    }
}

/// Current labels of a meeting (speaker → (person, source, confidence)).
async fn labels(api: &Api, meeting: &Id) -> Option<SpeakerLabels> {
    let arts = call(
        api,
        "artifacts.list",
        json!({ "meeting_id": meeting, "kind": "speaker_labels" }),
    )
    .await;
    let arts: Vec<almond_types::Artifact> = serde_json::from_value(arts).unwrap();
    let superseded: Vec<_> = arts.iter().filter_map(|a| a.supersedes.clone()).collect();
    arts.iter()
        .filter(|a| !superseded.contains(&a.id))
        .max_by_key(|a| a.created_at)
        .map(|a| a.decode().unwrap())
}

async fn wait_labels(
    api: &Api,
    meeting: &Id,
    pred: impl Fn(&SpeakerLabels) -> bool,
) -> SpeakerLabels {
    let t0 = Instant::now();
    loop {
        if let Some(l) = labels(api, meeting).await
            && pred(&l)
        {
            return l;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "labels for {meeting} never matched: {:?}",
            labels(api, meeting).await
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn label_of<'a>(
    l: &'a SpeakerLabels,
    speaker: &str,
) -> Option<&'a almond_types::artifact::body::SpeakerLabel> {
    l.labels.iter().find(|x| x.speaker.0 == speaker)
}

async fn people(api: &Api) -> Vec<Value> {
    call(api, "people.list", json!({}))
        .await
        .as_array()
        .unwrap()
        .clone()
}

async fn person_named(api: &Api, name: &str) -> Value {
    people(api)
        .await
        .into_iter()
        .find(|p| p["person"]["name"] == name)
        .unwrap_or_else(|| panic!("no person {name}"))["person"]
        .clone()
}

fn id_of(p: &Value) -> Id {
    Id(p["id"].as_str().unwrap().to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn label_suggest_calendar_merge_page_and_sync() {
    let (a, dir_a) = open_core("people-a").await;
    a.core
        .people
        .set_voice_embedder(Some(Arc::new(ToneEmbedder)));
    use AudioChannel::{Mic, System};

    // ---- meeting 1: me + two remote voices; nobody known yet → only "me" is labelled
    let (m1, _t1) = add_meeting(
        &a,
        48,
        &[(ME_VOICE, 8), (0.0, 12)],
        &[(LAN, 8), (0.0, 2), (DAVID, 8), (0.0, 2)],
        &[
            (0, 8, Mic, "me"),
            (0, 8, System, "remote:S1"),
            (10, 18, System, "remote:S2"),
        ],
        None,
    );
    let l1 = wait_labels(&a, &m1, |_| true).await;
    assert_eq!(l1.labels.len(), 1, "{l1:?}");
    let me = label_of(&l1, "me").unwrap();
    let selfp = call(&a, "people.get", json!({ "id": me.person })).await["person"].clone();
    assert_eq!(selfp["is_self"], true);
    assert_eq!(selfp["id"], "self");
    assert_eq!(
        selfp["voiceprints"].as_array().unwrap().len(),
        1,
        "me teaches the self person"
    );

    // ---- label the two remote speakers: people are created with voice prints
    let r = call(
        &a,
        "people.labelSpeaker",
        json!({ "meeting_id": m1, "speaker": "remote:S1", "name": "Trần Lan" }),
    )
    .await;
    assert_eq!(r["labels"].as_array().unwrap().len(), 2);
    call(
        &a,
        "people.labelSpeaker",
        json!({ "meeting_id": m1, "speaker": "remote:S2", "name": "David Kim" }),
    )
    .await;
    let lan = person_named(&a, "Trần Lan").await;
    let david = person_named(&a, "David Kim").await;
    let (lan_id, david_id) = (id_of(&lan), id_of(&david));
    let vp = &lan["voiceprints"][0];
    assert_eq!(vp["model"], "test:tones");
    assert_eq!(vp["speaker"], "remote:S1");
    assert_eq!(vp["vector"], json!([]), "API responses carry no vectors");
    assert!((vp["speech_s"].as_f64().unwrap() - 8.0).abs() < 0.1);
    let stored: Person = a.core.record("person", lan_id.as_str()).unwrap().unwrap();
    assert_eq!(stored.voiceprints[0].vector.len(), FREQS.len());
    // the same name again reuses the person
    call(
        &a,
        "people.labelSpeaker",
        json!({ "meeting_id": m1, "speaker": "remote:S1", "name": "trần lan" }),
    )
    .await;
    assert_eq!(people(&a).await.len(), 3);

    // ---- meeting 2: the same voices in other speaker slots → labelled by voice
    let (m2, _) = add_meeting(
        &a,
        24,
        &[(ME_VOICE, 6), (0.0, 24)],
        &[
            (DAVID, 8),
            (0.0, 2),
            (LAN, 8),
            (0.0, 2),
            (STRANGER, 8),
            (0.0, 2),
        ],
        &[
            (0, 6, Mic, "me"),
            (0, 8, System, "remote:S1"),
            (10, 18, System, "remote:S2"),
            (20, 28, System, "remote:S3"),
        ],
        None,
    );
    let l2 = wait_labels(&a, &m2, |_| true).await;
    let s1 = label_of(&l2, "remote:S1").expect("S1 suggested");
    assert_eq!(s1.person, david_id);
    assert_eq!(s1.source, almond_types::artifact::body::LabelSource::Voice);
    assert!(s1.confidence.unwrap() > 0.9);
    assert_eq!(label_of(&l2, "remote:S2").unwrap().person, lan_id);
    assert!(
        label_of(&l2, "remote:S3").is_none(),
        "unknown voice stays unlabelled"
    );
    assert_eq!(label_of(&l2, "me").unwrap().person.0, "self");

    // ---- user labels win: correct S1, then suggestions leave it alone
    call(
        &a,
        "people.labelSpeaker",
        json!({ "meeting_id": m2, "speaker": "remote:S1", "name": "Lê Minh" }),
    )
    .await;
    let minh_id = id_of(&person_named(&a, "Lê Minh").await);
    let sugg = call(&a, "people.suggestions", json!({ "meeting_id": m2 })).await;
    let sugg: Vec<almond_types::artifact::body::SpeakerLabel> =
        serde_json::from_value(sugg).unwrap();
    assert!(sugg.iter().all(|l| l.speaker.0 != "remote:S1"), "{sugg:?}");
    assert!(
        sugg.iter()
            .any(|l| l.speaker.0 == "remote:S2" && l.person == lan_id)
    );
    let l2 = labels(&a, &m2).await.unwrap();
    let s1 = label_of(&l2, "remote:S1").unwrap();
    assert_eq!(
        (&s1.person, s1.source),
        (&minh_id, almond_types::artifact::body::LabelSource::User)
    );
    // David's print came from meeting 1 and is still there
    let d: Person = a.core.record("person", david_id.as_str()).unwrap().unwrap();
    assert_eq!(d.voiceprints.len(), 1);

    // ---- calendar: a 1:1 with Lan (by email), a voice nobody knows
    call(
        &a,
        "people.save",
        json!({ "id": lan_id, "name": "Trần Lan", "emails": ["Lan.Tran@Almond.dev "] }),
    )
    .await;
    let ev = |id: &str, attendees: Vec<Attendee>| CalendarEvent {
        source: "ics:test".into(),
        event_id: id.into(),
        calendar_id: "c".into(),
        calendar_name: None,
        title: id.into(),
        description: None,
        location: None,
        start: now_ms() - 3_600_000,
        end: now_ms() - 1_800_000,
        all_day: false,
        attendees,
        conference_url: None,
        conference_app: None,
        organizer: None,
        status: "confirmed".into(),
        updated: 0,
    };
    let att = |email: &str, name: &str, is_self: bool| Attendee {
        email: Some(email.into()),
        name: Some(name.into()),
        response: Some("accepted".into()),
        is_self,
    };
    for e in [
        ev(
            "one-on-one",
            vec![
                att("me@almond.dev", "Me", true),
                att("lan.tran@almond.dev", "Lan", false),
            ],
        ),
        ev(
            "three-way",
            vec![
                att("me@almond.dev", "Me", true),
                att("lan.tran@almond.dev", "Lan", false),
                att("an@almond.dev", "An", false),
            ],
        ),
    ] {
        a.core
            .store()
            .append_local(EventBody::CalendarUpsert(e))
            .unwrap();
    }
    let eref = |id: &str| {
        Some(EventRef {
            source: "ics:test".into(),
            event_id: id.into(),
        })
    };
    let (m3, _) = add_meeting(
        &a,
        1,
        &[(0.0, 10)],
        &[(AN, 8), (0.0, 2)],
        &[(0, 8, System, "remote:S1")],
        eref("one-on-one"),
    );
    let l3 = wait_labels(&a, &m3, |_| true).await;
    let c = label_of(&l3, "remote:S1").unwrap();
    assert_eq!(c.person, lan_id);
    assert_eq!(
        c.source,
        almond_types::artifact::body::LabelSource::Calendar
    );
    // two candidate attendees for one speaker: ambiguous, no calendar label
    let (m4, _) = add_meeting(
        &a,
        2,
        &[(0.0, 10)],
        &[(AN, 8), (0.0, 2)],
        &[(0, 8, System, "remote:S1")],
        eref("three-way"),
    );
    let s4 = call(&a, "people.suggestions", json!({ "meeting_id": m4 })).await;
    assert_eq!(s4, json!([]));

    // ---- merge a duplicate into Lan
    let dup = call(
        &a,
        "people.save",
        json!({ "name": "Lan T.", "emails": ["lan@work.vn"], "notes": "Design lead" }),
    )
    .await;
    let dup_id = id_of(&dup);
    call(
        &a,
        "people.labelSpeaker",
        json!({ "meeting_id": m2, "speaker": "remote:S3", "person_id": dup_id }),
    )
    .await;
    let merged = call(
        &a,
        "people.merge",
        json!({ "keep": lan_id, "remove": dup_id }),
    )
    .await;
    assert_eq!(merged["aliases"], json!(["Lan T."]));
    assert_eq!(
        merged["emails"],
        json!(["lan.tran@almond.dev", "lan@work.vn"])
    );
    assert_eq!(merged["notes"], "Design lead");
    assert_eq!(merged["voiceprints"].as_array().unwrap().len(), 2);
    assert!(a.call("people.get", json!({ "id": dup_id })).await.is_err());
    let l2 = labels(&a, &m2).await.unwrap();
    assert_eq!(label_of(&l2, "remote:S3").unwrap().person, lan_id);

    // ---- person page: meetings by label and by attendee email, talk time, last met
    let page = call(&a, "people.get", json!({ "id": lan_id })).await;
    let ms: Vec<&str> = page["meetings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["meeting_id"].as_str().unwrap())
        .collect();
    // newest first: m3 (calendar label), m4 (attendee only), m2 (voice S2 + user S3), m1 (user S1)
    assert_eq!(ms, vec![m3.as_str(), m4.as_str(), m2.as_str(), m1.as_str()]);
    let m4e = &page["meetings"][1];
    assert_eq!(
        (
            m4e["attendee"].clone(),
            m4e["source"].clone(),
            m4e["talk_time_s"].as_f64().unwrap()
        ),
        (json!(true), Value::Null, 0.0)
    );
    assert_eq!(
        page["meetings"][2]["speakers"],
        json!(["remote:S2", "remote:S3"])
    );
    assert_eq!(page["meetings"][2]["source"], "user");
    assert!(
        (page["talk_time_s"].as_f64().unwrap() - 32.0).abs() < 0.01,
        "{page}"
    );
    let m3_started = a.core.store().get_meeting(&m3).unwrap().unwrap().started_at;
    assert_eq!(page["last_met"], json!(m3_started));
    let list = people(&a).await;
    assert_eq!(list[0]["person"]["is_self"], true, "me first");
    let lan_row = list
        .iter()
        .find(|p| p["person"]["id"] == lan_id.as_str())
        .unwrap();
    assert_eq!(lan_row["meeting_count"], 4);

    // ---- unlabel takes the user's voice print back; delete drops the person from labels
    call(
        &a,
        "people.unlabelSpeaker",
        json!({ "meeting_id": m2, "speaker": "remote:S3" }),
    )
    .await;
    let l: Person = a.core.record("person", lan_id.as_str()).unwrap().unwrap();
    assert_eq!(l.voiceprints.len(), 1);
    call(&a, "people.delete", json!({ "id": minh_id })).await;
    let l2 = labels(&a, &m2).await.unwrap();
    assert!(label_of(&l2, "remote:S1").is_none());
    assert!(label_of(&l2, "remote:S2").is_some());

    // ---- sync: a second core gets the people (and labels) from the event log
    let (b, dir_b) = open_core("people-b").await;
    let have = b.core.store().version_vector().unwrap();
    let evs = a.core.store().events_missing(&have, 100_000).unwrap();
    b.core.store().apply_remote(evs).unwrap();
    let names = |v: Vec<Value>| {
        let mut n: Vec<String> = v
            .iter()
            .map(|p| p["person"]["name"].as_str().unwrap().to_string())
            .collect();
        n.sort();
        n
    };
    assert_eq!(names(people(&b).await), names(people(&a).await));
    let page_b = call(&b, "people.get", json!({ "id": lan_id })).await;
    assert_eq!(page_b["meetings"].as_array().unwrap().len(), 4);
    let lb: Person = b.core.record("person", lan_id.as_str()).unwrap().unwrap();
    assert_eq!(lb.voiceprints.len(), 1);
    assert_eq!(lb.voiceprints[0].vector.len(), FREQS.len(), "vectors sync");

    a.core.shutdown().await;
    b.core.shutdown().await;
    let _ = std::fs::remove_dir_all(dir_a);
    let _ = std::fs::remove_dir_all(dir_b);
}

/// Without a voice embedder (CI builds have no sherpa), labels still work, just without prints.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn labels_work_without_voice_matching() {
    let (a, dir) = open_core("people-novoice").await;
    if a.core.people.voice_embedder().is_ok() {
        eprintln!("this build has a voice embedder; skipping");
        return;
    }
    let (m, _) = add_meeting(
        &a,
        1,
        &[(ME_VOICE, 6)],
        &[(LAN, 6)],
        &[(0, 6, AudioChannel::System, "remote:S1")],
        None,
    );
    let r = call(
        &a,
        "people.labelSpeaker",
        json!({ "meeting_id": m, "speaker": "remote:S1", "name": "Lan" }),
    )
    .await;
    assert_eq!(r["labels"][0]["source"], "user");
    let lan = person_named(&a, "Lan").await;
    assert_eq!(lan["voiceprints"], json!([]));
    // bad requests
    let e = a
        .call(
            "people.labelSpeaker",
            json!({ "meeting_id": m, "speaker": "remote:S9", "name": "X" }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, -32004);
    assert!(
        a.call(
            "people.labelSpeaker",
            json!({ "meeting_id": m, "speaker": "remote:S1" })
        )
        .await
        .is_err()
    );
    a.core.shutdown().await;
    let _ = std::fs::remove_dir_all(dir);
}
