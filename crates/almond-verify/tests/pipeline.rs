//! End-to-end through the `Processor` adapters with the shared mock fixtures
//! (`fixtures::summary` says 300 triệu where the transcript says 200 triệu).

use almond_types::artifact::body::{Claims, VerdictLabel, Verdicts};
use almond_types::calendar::{Attendee, CalendarEvent};
use almond_types::ids::DeviceId;
use almond_types::mock::{MemBlobStore, MockLlm, fixtures};
use almond_types::pipeline::{JobCtx, Processor};
use almond_types::{Artifact, Hlc, Id, Meeting};
use almond_verify::extract::{ClaimExtractor, ExtractClaimsProcessor};
use almond_verify::verifiers::{NliVerifier, NumbersVerifier, VerifyConfig};
use almond_verify::{VerifierRegistry, VerifyProcessor};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Monday 2026-11-09 09:00 at UTC+7.
const START: i64 = 1_794_189_600_000;
const TZ: i32 = 420;

fn job(meeting: &Meeting, params: serde_json::Value) -> JobCtx {
    JobCtx {
        job_id: Id::new(),
        meeting: meeting.clone(),
        params,
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(|_, _| {}),
        blobs: Arc::new(MemBlobStore::default()),
    }
}

fn setup() -> (Meeting, Artifact, Artifact, DeviceId) {
    let dev = DeviceId("dev-test".into());
    let m = fixtures::meeting(START);
    let t = fixtures::transcript(&m.id, &dev);
    let s = fixtures::summary(&t, &dev);
    (m, t, s, dev)
}

fn materialize(d: almond_types::ArtifactDraft, dev: &DeviceId) -> Artifact {
    d.into_artifact(Id::new(), Hlc::default(), dev.clone())
}

fn config() -> VerifyConfig {
    VerifyConfig {
        tz_offset_min: Some(TZ),
    }
}

#[tokio::test]
async fn reference_day_is_monday() {
    assert_eq!(
        almond_verify::dates::reference_day(START, Some(TZ)).to_string(),
        "2026-11-09"
    );
}

#[tokio::test]
async fn rules_extraction_then_symbolic_verification_catches_300_vs_200() {
    let (m, t, s, dev) = setup();
    let mut ex = ExtractClaimsProcessor::new(ClaimExtractor::rules());
    ex.tz_offset_min = Some(TZ);
    let out = ex
        .run(&job(&m, json!({})), vec![s.clone(), t.clone()])
        .await
        .unwrap();
    assert_eq!(out.len(), 1);
    let claims_art = materialize(out.into_iter().next().unwrap(), &dev);
    let claims: Claims = claims_art.decode().unwrap();
    assert_eq!(claims.claims.len(), 3);
    assert_eq!(claims.claims[0].source.artifact, s.id);
    assert_eq!(claims.claims[0].evidence[0].artifact, t.id);
    assert_eq!(
        claims.claims[2].logical_form_kind.as_deref(),
        Some("triples.v1")
    );

    let p = VerifyProcessor::all(VerifierRegistry::symbolic(config()));
    assert_eq!(p.descriptor().produces, ["verdicts"]);
    let out = p
        .run(&job(&m, json!({})), vec![claims_art.clone(), t.clone(), s])
        .await
        .unwrap();
    let vs: Verdicts = materialize(out.into_iter().next().unwrap(), &dev)
        .decode()
        .unwrap();
    let get = |claim: &str, verifier: &str| {
        vs.verdicts
            .iter()
            .find(|v| v.claim_id == claim && v.verifier == verifier)
            .cloned()
    };
    // c1: "Ngân sách marketing Q4 là 300 triệu đồng." vs "200 triệu đồng"
    let n = get("c1", "verify.numbers").unwrap();
    assert_eq!(n.label, VerdictLabel::Contradicted, "{n:?}");
    assert!(n.explanation.contains("300") && n.explanation.contains("200"));
    assert_eq!(n.trace.as_ref().unwrap()["rule"], "numbers.v1");
    assert_eq!(n.claims_artifact, claims_art.id);
    let a = get("c1", "verify.aggregate").unwrap();
    assert_eq!(a.label, VerdictLabel::Contradicted);
    assert_eq!(a.trace.as_ref().unwrap()["fired"], "R1");
    // c2: "Tuyển 2 kỹ sư backend trước tháng 12." vs "hire two backend engineers by December"
    assert_eq!(
        get("c2", "verify.numbers").unwrap().label,
        VerdictLabel::Supported
    );
    assert_eq!(
        get("c2", "verify.dates").unwrap().label,
        VerdictLabel::Supported
    );
    assert_eq!(
        get("c2", "verify.aggregate").unwrap().label,
        VerdictLabel::Supported
    );
    // c3: "Lan gửi kế hoạch trước thứ Sáu." vs "Ok, Lan sẽ gửi bản kế hoạch trước thứ Sáu."
    assert_eq!(
        get("c3", "verify.entities").unwrap().label,
        VerdictLabel::Supported
    );
    let d = get("c3", "verify.dates").unwrap();
    assert_eq!(d.label, VerdictLabel::Supported);
    assert!(d.explanation.contains("2026-11-13"), "{}", d.explanation);
    assert_eq!(
        get("c3", "verify.aggregate").unwrap().label,
        VerdictLabel::Supported
    );
}

#[tokio::test]
async fn llm_extraction_with_mock_llm_and_number_guard() {
    let (m, t, s, dev) = setup();
    let llm = MockLlm {
        json_reply: json!({"claims": [
            {"unit": 0, "text": "Ngân sách marketing Q4 là 300 triệu đồng.",
             "triples": [{"s": "ngân sách marketing Q4", "p": "amount", "o": "300 triệu đồng", "o_type": "money"}]},
            {"unit": 1, "text": "Tuyển 2 kỹ sư backend.", "triples": []},
            {"unit": 1, "text": "Hạn tuyển kỹ sư backend là trước tháng 12.", "triples": [
                {"s": "tuyển kỹ sư backend", "p": "due", "o": "trước tháng 12", "o_type": "date"}]},
            // invents a number -> rejected, unit 2 falls back to verbatim
            {"unit": 2, "text": "Lan gửi 3 bản kế hoạch.", "triples": []}
        ]}),
        ..Default::default()
    };
    let ex = ExtractClaimsProcessor::new(ClaimExtractor::llm(Arc::new(llm), "mock"));
    let d = ex
        .run(&job(&m, json!({"tz_offset_min": TZ})), vec![s, t])
        .await
        .unwrap()
        .remove(0);
    assert_eq!(d.provenance.model.as_deref(), Some("mock:mock:latest@mock"));
    let claims: Claims = materialize(d, &dev).decode().unwrap();
    let texts: Vec<_> = claims.claims.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "Ngân sách marketing Q4 là 300 triệu đồng.",
            "Tuyển 2 kỹ sư backend.",
            "Hạn tuyển kỹ sư backend là trước tháng 12.",
            "Lan gửi kế hoạch trước thứ Sáu.",
        ]
    );
    let lf = claims.claims[0].logical_form.as_ref().unwrap();
    assert_eq!(lf["triples"][0]["o"]["type"], "money");
    assert_eq!(lf["triples"][0]["o"]["value"], 300_000_000.0);
    let lf = claims.claims[2].logical_form.as_ref().unwrap();
    assert_eq!(lf["triples"][0]["p"], "due");
    assert_eq!(lf["triples"][0]["o"]["month"], 12);
    assert_eq!(claims.claims[1].source.segment, 1);
    assert_eq!(claims.claims[2].source.segment, 1);
}

#[tokio::test]
async fn nli_with_mock_judge_and_quote_check() {
    let (m, t, s, dev) = setup();
    let claims_art = materialize(
        ExtractClaimsProcessor::new(ClaimExtractor::rules())
            .run(&job(&m, json!({})), vec![s, t.clone()])
            .await
            .unwrap()
            .remove(0),
        &dev,
    );
    // Real quote -> contradicted stands.
    let good = MockLlm {
        json_reply: json!({"reason": "transcript says 200", "quote": "200 triệu đồng", "segment": 1,
                           "label": "contradicted", "confidence": 0.95}),
        ..Default::default()
    };
    let p = VerifyProcessor::single(Arc::new(NliVerifier::new(Arc::new(good), "mock")));
    assert_eq!(p.descriptor().name, "verify.nli");
    let vs: Verdicts = materialize(
        p.run(&job(&m, json!({})), vec![claims_art.clone(), t.clone()])
            .await
            .unwrap()
            .remove(0),
        &dev,
    )
    .decode()
    .unwrap();
    assert_eq!(vs.verdicts.len(), 3);
    assert_eq!(vs.verdicts[0].label, VerdictLabel::Contradicted);
    assert_eq!(vs.verdicts[0].evidence[0].segment, 1);

    // Hallucinated quote -> downgraded to needs_review.
    let bad = MockLlm {
        json_reply: json!({"reason": "x", "quote": "ngân sách 500 tỷ", "segment": 1,
                           "label": "entailed", "confidence": 0.99}),
        ..Default::default()
    };
    let p = VerifyProcessor::single(Arc::new(NliVerifier::new(Arc::new(bad), "mock")));
    let vs: Verdicts = materialize(
        p.run(&job(&m, json!({})), vec![claims_art, t])
            .await
            .unwrap()
            .remove(0),
        &dev,
    )
    .decode()
    .unwrap();
    assert!(
        vs.verdicts
            .iter()
            .all(|v| v.label == VerdictLabel::NeedsReview)
    );
}

#[tokio::test]
async fn calendar_and_subset_param() {
    let (m, t, s, dev) = setup();
    let mut summary: almond_types::artifact::body::Summary = s.decode().unwrap();
    let cite = summary.sections[0].bullets[0].cites.clone();
    summary.sections[0]
        .bullets
        .push(almond_types::artifact::body::CitedText {
            text: "Cuộc họp bắt đầu lúc 10h sáng, Minh tham dự.".into(),
            cites: cite,
        });
    let mut s2 = s.clone();
    s2.body = serde_json::to_value(&summary).unwrap();
    let claims_art = materialize(
        ExtractClaimsProcessor::new(ClaimExtractor::rules())
            .run(&job(&m, json!({})), vec![s2, t.clone()])
            .await
            .unwrap()
            .remove(0),
        &dev,
    );
    let ev = CalendarEvent {
        source: "google:me".into(),
        event_id: "e1".into(),
        calendar_id: "primary".into(),
        calendar_name: None,
        title: "Q4".into(),
        description: None,
        location: None,
        start: START,
        end: START + 30 * 60_000,
        all_day: false,
        attendees: vec![
            Attendee {
                email: Some("lan@example.com".into()),
                name: Some("Lan".into()),
                response: Some("accepted".into()),
                is_self: false,
            },
            Attendee {
                email: Some("minh@example.com".into()),
                name: Some("Minh".into()),
                response: Some("declined".into()),
                is_self: false,
            },
        ],
        conference_url: None,
        conference_app: None,
        organizer: None,
        status: "confirmed".into(),
        updated: START,
    };
    let p = VerifyProcessor::all(VerifierRegistry::symbolic(config()));
    let vs: Verdicts = materialize(
        p.run(
            &job(
                &m,
                json!({"calendar_event": ev, "verifiers": ["verify.calendar"]}),
            ),
            vec![claims_art, t],
        )
        .await
        .unwrap()
        .remove(0),
        &dev,
    )
    .decode()
    .unwrap();
    let cal: Vec<_> = vs
        .verdicts
        .iter()
        .filter(|v| v.verifier == "verify.calendar")
        .collect();
    assert_eq!(cal.len(), 1, "{:#?}", vs.verdicts);
    assert_eq!(cal[0].claim_id, "c4");
    assert_eq!(cal[0].label, VerdictLabel::Contradicted);
    assert!(
        cal[0].explanation.contains("09:00"),
        "{}",
        cal[0].explanation
    );
    assert!(cal[0].explanation.contains("declined"));
    // only the requested verifier ran (+ aggregate)
    assert!(
        vs.verdicts
            .iter()
            .all(|v| v.verifier == "verify.calendar" || v.verifier == "verify.aggregate")
    );
}

#[tokio::test]
async fn numbers_verifier_registry_roundtrip() {
    let mut r = VerifierRegistry::symbolic(config());
    assert!(r.names().contains(&"verify.numbers".to_string()));
    #[cfg(feature = "datalog")]
    assert!(r.names().contains(&"verify.consistency".to_string()));
    r.register(Arc::new(NumbersVerifier::default()));
    assert_eq!(
        r.names()
            .iter()
            .filter(|n| n.as_str() == "verify.numbers")
            .count(),
        1
    );
    assert!(r.remove("verify.numbers").is_some());
    assert!(r.get("verify.numbers").is_none());
}
