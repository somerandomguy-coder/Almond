//! Baseline verifiers. Symbolic ones put a machine-readable derivation in `Verdict::trace`
//! (`{"rule": "<verifier>.v1", ...}`) so the UI can render "why".

pub mod calendar;
pub mod citation;
#[cfg(feature = "datalog")]
pub mod consistency;
pub mod dates;
pub mod distortion;
pub mod entities;
pub mod nli;
pub mod numbers;
pub mod revision;

pub use calendar::CalendarVerifier;
pub use citation::CitationExistsVerifier;
#[cfg(feature = "datalog")]
pub use consistency::ConsistencyVerifier;
pub use dates::DatesVerifier;
pub use distortion::DistortionVerifier;
pub use entities::EntitiesVerifier;
pub use nli::NliVerifier;
pub use numbers::NumbersVerifier;
pub use revision::RevisionVerifier;

use almond_types::SpanRef;
use almond_types::artifact::body::{Claim, Verdict, VerdictLabel};
use almond_types::verify::VerifyContext;

/// Per-verifier configuration shared by the symbolic verifiers.
#[derive(Debug, Clone, Default)]
pub struct VerifyConfig {
    /// Offset of the meeting's local time zone from UTC, in minutes (Vietnam = 420).
    /// `None` = this machine's local zone. Used to resolve "thứ Sáu", "tomorrow", meeting time.
    pub tz_offset_min: Option<i32>,
}

impl VerifyConfig {
    /// Effective UTC offset (minutes) for `ctx`: job params (`tz_offset_min`, then IANA `tz`),
    /// then the meeting's IANA `tz`, then [`Self::tz_offset_min`], else `None` (local zone).
    pub fn tz(&self, ctx: &VerifyContext) -> Option<i32> {
        crate::tz::offset_for(&ctx.params, &ctx.meeting).or(self.tz_offset_min)
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn verdict(
    ctx: &VerifyContext,
    claim: &Claim,
    verifier: &str,
    label: VerdictLabel,
    confidence: f32,
    evidence: Vec<SpanRef>,
    explanation: impl Into<String>,
    trace: Option<serde_json::Value>,
) -> Verdict {
    Verdict {
        claims_artifact: ctx.claims_artifact.id.clone(),
        claim_id: claim.id.clone(),
        verifier: verifier.to_string(),
        label,
        confidence: confidence.clamp(0.0, 1.0),
        evidence,
        explanation: explanation.into(),
        trace,
    }
}

/// Outcome of checking one atom (number / date / name) of a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AtomStatus {
    /// Found with the same value in the cited evidence.
    Match,
    /// Found, but only at a coarser granularity (e.g. a day inside a cited month).
    Partial,
    /// Not in cited evidence but present elsewhere in the transcript.
    Uncited,
    /// Not found anywhere.
    Absent,
    /// Cited evidence states a different value.
    Conflict,
}

/// Combine atom statuses into a label: any conflict -> contradicted, absent/uncited -> needs
/// review, partial -> weakly supported, all match -> supported.
pub(crate) fn label_for(statuses: &[AtomStatus]) -> (VerdictLabel, f32) {
    let worst = statuses.iter().max().copied().unwrap_or(AtomStatus::Match);
    match worst {
        AtomStatus::Conflict => (VerdictLabel::Contradicted, 0.9),
        AtomStatus::Absent => (VerdictLabel::NeedsReview, 0.6),
        AtomStatus::Uncited => (VerdictLabel::NeedsReview, 0.5),
        AtomStatus::Partial => (VerdictLabel::Supported, 0.6),
        AtomStatus::Match => (VerdictLabel::Supported, 0.9),
    }
}
