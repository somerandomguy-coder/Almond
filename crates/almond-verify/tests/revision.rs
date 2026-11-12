//! `verify.revision` on the later-reversal fixtures (`eval/reversals.json`): every bullet that keeps
//! the early version of a decision is flagged by it; no control bullet is.

use almond_types::artifact::body::VerdictLabel;
use almond_verify::eval::{self, Fixtures};
use almond_verify::extract::ClaimExtractor;
use almond_verify::verifiers::revision::NAME;
use almond_verify::{AggregatePolicy, VerifierRegistry, VerifyConfig};

#[tokio::test]
async fn later_reversals_are_caught_and_controls_left_alone() {
    let fx: Fixtures = serde_json::from_str(include_str!("../eval/reversals.json")).unwrap();
    let r = eval::run(
        "reversals",
        &fx,
        &ClaimExtractor::rules(),
        &VerifierRegistry::symbolic(VerifyConfig {
            tz_offset_min: Some(420),
        }),
        &AggregatePolicy::default(),
    )
    .await;
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let mut reversed = 0;
    for row in &r.rows {
        let rev = row.labels.get(NAME);
        if row.gold {
            reversed += 1;
            assert!(
                matches!(
                    rev,
                    Some(VerdictLabel::Contradicted | VerdictLabel::NeedsReview)
                ),
                "missed reversal: {} -> {rev:?}\n{}",
                row.text,
                r.to_markdown(true)
            );
            assert_ne!(row.aggregate, VerdictLabel::Supported, "{}", row.text);
        } else {
            assert!(
                rev.is_none(),
                "false alarm on control: {} -> {rev:?}",
                row.text
            );
        }
    }
    assert_eq!(reversed, 4);
}

/// The old regression gate must still hold with the new verifier in the set.
#[tokio::test]
async fn no_revision_alarms_on_the_hallucination_fixtures() {
    for fx in [eval::load(), eval::load_heldout()] {
        let r = eval::run(
            "dev",
            &fx,
            &ClaimExtractor::rules(),
            &VerifierRegistry::symbolic(VerifyConfig {
                tz_offset_min: Some(420),
            }),
            &AggregatePolicy::default(),
        )
        .await;
        let fa: Vec<_> = r
            .rows
            .iter()
            .filter(|row| !row.gold && row.labels.contains_key(NAME))
            .map(|row| row.text.clone())
            .collect();
        assert!(fa.is_empty(), "revision false alarms: {fa:?}");
    }
}
