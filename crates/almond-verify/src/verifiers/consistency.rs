//! `verify.consistency`: cross-claim rules over `triples.v1` logical forms, evaluated with the
//! `ascent` Datalog engine. Each fired rule becomes a verdict whose `trace` is the derivation
//! (head + body facts), i.e. a machine-checkable proof the UI can render.
//!
//! Rules (program `consistency.v1`):
//! ```text
//! conflict(C1,C2,S,P,O1,O2)   <- triple(C1,S,P,T,O1), triple(C2,S,P,T,O2), functional(P), C1<C2, O1!=O2.
//! clash(C,D,S,P,O,OD)         <- conflict(C,D,S,P,O,OD).   clash(D,C,S,P,OD,O) <- conflict(C,D,S,P,O,OD).
//! blamed(C,D,S,P,O,OD)        <- clash(C,D,S,P,O,OD), grounded(D,P,OD), !grounded(C,P,O).
//! unresolved(C,D,S,P,O,OD)    <- clash(C,D,S,P,O,OD), !grounded(C,P,O), !grounded(D,P,OD).
//! owner_not_attendee(C,S,W)   <- owner(C,S,W), !attendee(W).          (only when attendees are known)
//! due_before_meeting(C,S,D,M) <- due_day(C,S,D), meeting_day(M), D<M.
//! ```
//!
//! `grounded(C,P,O)` is computed in Rust: the literal O of claim C occurs in C's *cited* evidence
//! (same number / same resolved date / person mentioned or speaking). It turns a symmetric
//! conflict into a blame assignment: when two claims disagree and only one is grounded, the other
//! one is contradicted; when neither is grounded, both need review; when both are, nothing fires.

use super::{VerifyConfig, verdict};
use crate::evidence::{EvidenceIndex, ResolvedSpan, mentions};
use crate::names;
use crate::triples::{self, Literal, TriplesV1, canonical_predicate, subject_key};
use almond_types::artifact::body::{Claim, Verdict, VerdictLabel};
use almond_types::verify::{Verifier, VerifierDescriptor, VerifyContext};
use ascent::ascent;
use async_trait::async_trait;
use chrono::Datelike;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const NAME: &str = "verify.consistency";
pub const PROGRAM: &str = "consistency.v1";

ascent! {
    struct Program;
    // claim, subject, predicate, literal type, literal value
    relation triple(String, String, String, String, String);
    relation functional(String);
    // claim, subject, person key
    relation owner(String, String, String);
    relation attendee(String);
    // claim, subject, day number (days from CE)
    relation due_day(String, String, i32);
    relation meeting_day(i32);

    relation conflict(String, String, String, String, String, String);
    conflict(c1, c2, s, p, o1, o2) <--
        triple(c1, s, p, t, o1), triple(c2, s, p, t, o2), functional(p),
        if c1 < c2, if o1 != o2;

    // claim, predicate, canonical value: the value occurs in the claim's cited evidence
    relation grounded(String, String, String);
    relation clash(String, String, String, String, String, String);
    clash(c1, c2, s, p, o1, o2) <-- conflict(c1, c2, s, p, o1, o2);
    clash(c2, c1, s, p, o2, o1) <-- conflict(c1, c2, s, p, o1, o2);
    relation blamed(String, String, String, String, String, String);
    blamed(c, d, s, p, o, od) <-- clash(c, d, s, p, o, od), grounded(d, p, od), !grounded(c, p, o);
    relation unresolved(String, String, String, String, String, String);
    unresolved(c, d, s, p, o, od) <--
        clash(c, d, s, p, o, od), !grounded(c, p, o), !grounded(d, p, od);

    relation owner_not_attendee(String, String, String);
    owner_not_attendee(c, s, w) <-- owner(c, s, w), !attendee(w);

    relation due_before_meeting(String, String, i32, i32);
    due_before_meeting(c, s, d, m) <-- due_day(c, s, d), meeting_day(m), if d < m;
}

#[derive(Debug, Clone)]
pub struct ConsistencyVerifier {
    pub config: VerifyConfig,
    /// Derive triples heuristically for claims that carry no `triples.v1` logical form.
    pub derive_missing: bool,
}

impl Default for ConsistencyVerifier {
    fn default() -> Self {
        ConsistencyVerifier {
            config: VerifyConfig::default(),
            derive_missing: true,
        }
    }
}

fn fact(rel: &str, args: &[Value]) -> Value {
    json!({"rel": rel, "args": args})
}

/// Does the literal occur in the cited evidence? (numbers by value, dates by resolved value,
/// people by mention or as the segment's speaker).
fn grounded(o: &Literal, ev: &[ResolvedSpan], r: chrono::NaiveDate) -> bool {
    match o {
        Literal::Number { value, .. }
        | Literal::Money { value, .. }
        | Literal::Percent { value, .. } => ev.iter().any(|s| {
            crate::numbers::extract(&s.text)
                .iter()
                .any(|q| (q.value - value).abs() <= 1e-6 * value.abs().max(1.0))
        }),
        Literal::Date { value, .. } => ev.iter().any(|s| {
            crate::dates::extract(&s.text, r)
                .iter()
                .any(|d| d.value == *value)
        }),
        Literal::Person { name } | Literal::Entity { name } => ev.iter().any(|s| {
            mentions(&s.text, name)
                || s.speaker
                    .as_deref()
                    .is_some_and(|sp| mentions(sp, name) || names::key(sp) == names::key(name))
        }),
        Literal::Text { .. } => false,
    }
}

/// Triples of a claim: its logical form if it is `triples.v1`, else (optionally) heuristics.
pub fn claim_triples(
    c: &Claim,
    derive_missing: bool,
    people: &[String],
    reference: chrono::NaiveDate,
) -> Option<TriplesV1> {
    if c.logical_form_kind.as_deref() == Some(triples::KIND)
        && let Some(lf) = &c.logical_form
        && let Ok(t) = serde_json::from_value::<TriplesV1>(lf.clone())
    {
        return Some(t);
    }
    derive_missing.then(|| triples::heuristic(&c.text, None, None, people, reference))
}

#[async_trait]
impl Verifier for ConsistencyVerifier {
    fn descriptor(&self) -> VerifierDescriptor {
        VerifierDescriptor {
            name: NAME.into(),
            version: "1".into(),
            family: "symbolic".into(),
            logical_forms: vec![triples::KIND.into()],
            description: "Datalog (ascent) rules over triples: conflicting values for the same \
                          (subject, predicate), owners who are not attendees, deadlines before \
                          the meeting."
                .into(),
        }
    }

    async fn verify(
        &self,
        claims: &[Claim],
        ctx: &VerifyContext,
    ) -> almond_types::Result<Vec<Verdict>> {
        let idx = EvidenceIndex::new(ctx, self.config.tz(ctx));
        let r = idx.reference_day;
        let mut p = Program {
            functional: triples::FUNCTIONAL
                .iter()
                .map(|s| (s.to_string(),))
                .collect(),
            meeting_day: vec![(r.num_days_from_ce(),)],
            ..Default::default()
        };
        let have_attendees = !idx.people.is_empty();
        for a in &idx.people {
            let k = names::key(a);
            for tok in k.split(' ') {
                p.attendee.push((tok.to_string(),));
            }
            p.attendee.push((k,));
        }
        let by_id: BTreeMap<&str, &Claim> = claims.iter().map(|c| (c.id.as_str(), c)).collect();
        for c in claims {
            let Some(t) = claim_triples(c, self.derive_missing, &idx.people, r) else {
                continue;
            };
            let ev = idx.resolve_all(&c.evidence);
            for tr in &t.triples {
                let s = subject_key(&tr.s);
                let pred = canonical_predicate(&tr.p);
                let (ty, val) = tr.o.canonical();
                if s.is_empty() {
                    continue;
                }
                if pred == "owner"
                    && have_attendees
                    && let Literal::Person { name } = &tr.o
                    && !names::is_group(name)
                {
                    // A titled or partial name of an attendee counts as that attendee.
                    let w = idx
                        .people
                        .iter()
                        .find(|a| names::same_person(a, name))
                        .map(|a| names::key(a))
                        .unwrap_or_else(|| names::key(name));
                    p.owner.push((c.id.clone(), s.clone(), w));
                }
                if pred == "due"
                    && let Literal::Date { value, .. } = &tr.o
                {
                    p.due_day
                        .push((c.id.clone(), s.clone(), value.last_day().num_days_from_ce()));
                }
                if grounded(&tr.o, &ev, r) {
                    p.grounded.push((c.id.clone(), pred.clone(), val.clone()));
                }
                p.triple.push((c.id.clone(), s, pred, ty, val));
            }
        }
        p.run();

        // claim id -> (label, confidence, explanation, derivation)
        let mut findings: BTreeMap<String, Vec<(VerdictLabel, f32, String, Value)>> =
            BTreeMap::new();
        let ty_of = |c: &String, s: &String, pr: &String, o: &String| {
            p.triple
                .iter()
                .find(|t| &t.0 == c && &t.1 == s && &t.2 == pr && &t.4 == o)
                .map(|t| t.3.clone())
                .unwrap_or_default()
        };
        let conflict_proof = |c: &String,
                              d: &String,
                              s: &String,
                              pr: &String,
                              o: &String,
                              od: &String| {
            let ty = ty_of(c, s, pr, o);
            json!({
                "rule": "functional_conflict",
                "head": fact("clash", &[json!(c), json!(d), json!(s), json!(pr), json!(o), json!(od)]),
                "body": [
                    fact("triple", &[json!(c), json!(s), json!(pr), json!(ty), json!(o)]),
                    fact("triple", &[json!(d), json!(s), json!(pr), json!(ty), json!(od)]),
                    fact("functional", &[json!(pr)]),
                    {"builtin": "!=", "args": [o, od]},
                ]
            })
        };
        let text_of = |id: &String| {
            by_id
                .get(id.as_str())
                .map(|c| c.text.clone())
                .unwrap_or_default()
        };
        for (c, d, s, pr, o, od) in &p.blamed {
            findings.entry(c.clone()).or_default().push((
                VerdictLabel::Contradicted,
                0.8,
                format!(
                    "Conflicts with claim {d} (\"{}\"): {pr} of \"{s}\" is {o} here but {od} there, and only {od} is backed by the cited evidence.",
                    text_of(d)
                ),
                json!({
                    "rule": "blamed",
                    "head": fact("blamed", &[json!(c), json!(d), json!(s), json!(pr), json!(o), json!(od)]),
                    "body": [
                        conflict_proof(c, d, s, pr, o, od),
                        fact("grounded", &[json!(d), json!(pr), json!(od)]),
                        {"not": fact("grounded", &[json!(c), json!(pr), json!(o)])},
                    ]
                }),
            ));
        }
        for (c, d, s, pr, o, od) in &p.unresolved {
            findings.entry(c.clone()).or_default().push((
                VerdictLabel::NeedsReview,
                0.6,
                format!(
                    "Conflicts with claim {d} (\"{}\"): {pr} of \"{s}\" is {o} here but {od} there; neither is backed by the cited evidence.",
                    text_of(d)
                ),
                json!({
                    "rule": "unresolved",
                    "head": fact("unresolved", &[json!(c), json!(d), json!(s), json!(pr), json!(o), json!(od)]),
                    "body": [
                        conflict_proof(c, d, s, pr, o, od),
                        {"not": fact("grounded", &[json!(c), json!(pr), json!(o)])},
                        {"not": fact("grounded", &[json!(d), json!(pr), json!(od)])},
                    ]
                }),
            ));
        }
        for (c, s, w) in &p.owner_not_attendee {
            findings.entry(c.clone()).or_default().push((
                VerdictLabel::NeedsReview,
                0.6,
                format!("Owner \"{w}\" of \"{s}\" is not an attendee or speaker of this meeting."),
                json!({
                    "rule": "owner_not_attendee",
                    "head": fact("owner_not_attendee", &[json!(c), json!(s), json!(w)]),
                    "body": [
                        fact("owner", &[json!(c), json!(s), json!(w)]),
                        {"not": fact("attendee", &[json!(w)])},
                    ]
                }),
            ));
        }
        for (c, s, d, m) in &p.due_before_meeting {
            let day = |n: i32| {
                chrono::NaiveDate::from_num_days_from_ce_opt(n)
                    .map(|d| d.to_string())
                    .unwrap_or_default()
            };
            findings.entry(c.clone()).or_default().push((
                VerdictLabel::Contradicted,
                0.75,
                format!(
                    "Deadline {} for \"{s}\" is before the meeting date {}.",
                    day(*d),
                    day(*m)
                ),
                json!({
                    "rule": "due_before_meeting",
                    "head": fact("due_before_meeting", &[json!(c), json!(s), json!(day(*d)), json!(day(*m))]),
                    "body": [
                        fact("due_day", &[json!(c), json!(s), json!(day(*d))]),
                        fact("meeting_day", &[json!(day(*m))]),
                        {"builtin": "<", "args": [day(*d), day(*m)]},
                    ]
                }),
            ));
        }

        let mut out = Vec::new();
        for c in claims {
            let Some(f) = findings.remove(&c.id) else {
                continue;
            };
            let worst = f
                .iter()
                .max_by(|a, b| {
                    let rank = |l: VerdictLabel| match l {
                        VerdictLabel::Contradicted => 2,
                        VerdictLabel::NeedsReview => 1,
                        _ => 0,
                    };
                    rank(a.0).cmp(&rank(b.0)).then(a.1.total_cmp(&b.1))
                })
                .cloned()
                .unwrap();
            let explanation = f.iter().map(|x| x.2.as_str()).collect::<Vec<_>>().join(" ");
            out.push(verdict(
                ctx,
                c,
                NAME,
                worst.0,
                worst.1,
                vec![],
                explanation,
                Some(json!({
                    "rule": "consistency.v1",
                    "engine": "ascent",
                    "program": PROGRAM,
                    "derivations": f.into_iter().map(|x| x.3).collect::<Vec<_>>(),
                })),
            ));
        }
        Ok(out)
    }
}
