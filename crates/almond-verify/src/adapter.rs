//! Verifiers as pipeline `Processor`s (`claims` [+ `transcript`, `summary`, ...] -> `verdicts`).
//!
//! Job params understood:
//! - `calendar_event`: a `CalendarEvent` (JSON) to check against (until the pipeline can pass one
//!   in `VerifyContext` directly, see docs/contract-requests/ws-h.md).
//! - `verifiers`: `["verify.numbers", ...]` to run a subset.

use crate::aggregate::{self, AggregatePolicy};
use crate::registry::VerifierRegistry;
use almond_types::artifact::body::{Claims, Transcript, Verdicts};
use almond_types::calendar::CalendarEvent;
use almond_types::job::Capability;
use almond_types::pipeline::{JobCtx, Processor, ProcessorDescriptor};
use almond_types::verify::{Verifier, VerifyContext};
use almond_types::{Artifact, ArtifactDraft, ArtifactKind, Provenance};
use async_trait::async_trait;
use std::sync::Arc;

/// Build a `VerifyContext` from processor inputs. Picks the (latest) claims artifact; the
/// transcript is the one the claims cite most, else the latest transcript.
pub fn build_context(
    ctx: &JobCtx,
    inputs: Vec<Artifact>,
) -> almond_types::Result<(VerifyContext, Claims)> {
    let mut claims_art: Option<Artifact> = None;
    let mut rest = Vec::new();
    for a in inputs {
        if a.kind.is(ArtifactKind::CLAIMS)
            && claims_art
                .as_ref()
                .is_none_or(|c| a.created_at > c.created_at)
        {
            if let Some(old) = claims_art.replace(a) {
                rest.push(old);
            }
        } else {
            rest.push(a);
        }
    }
    let claims_art = claims_art.ok_or_else(|| {
        almond_types::Error::Invalid("verify: no claims artifact in inputs".into())
    })?;
    let claims: Claims = claims_art.decode()?;
    let cited = |a: &Artifact| {
        claims
            .claims
            .iter()
            .flat_map(|c| &c.evidence)
            .filter(|s| s.artifact == a.id)
            .count()
    };
    let transcript = rest
        .iter()
        .filter(|a| a.kind.is(ArtifactKind::TRANSCRIPT))
        .max_by(|a, b| {
            cited(a)
                .cmp(&cited(b))
                .then(a.created_at.cmp(&b.created_at))
        })
        .cloned();
    let transcript = match transcript {
        Some(a) => {
            let t: Transcript = a.decode()?;
            rest.retain(|x| x.id != a.id);
            Some((a, t))
        }
        None => None,
    };
    let calendar_event = ctx
        .params
        .get("calendar_event")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value::<CalendarEvent>(v.clone()))
        .transpose()?;
    Ok((
        VerifyContext {
            meeting: ctx.meeting.clone(),
            transcript,
            calendar_event,
            claims_artifact: claims_art,
            extra: rest,
        },
        claims,
    ))
}

/// Runs a registry of verifiers (optionally + aggregation) as one processor.
pub struct VerifyProcessor {
    pub name: String,
    pub registry: VerifierRegistry,
    /// Append one `verify.aggregate` verdict per claim.
    pub aggregate: Option<AggregatePolicy>,
    pub requires: Vec<Capability>,
}

impl VerifyProcessor {
    /// `verify.all`: every registered verifier + aggregation.
    pub fn all(registry: VerifierRegistry) -> Self {
        VerifyProcessor {
            name: "verify.all".into(),
            registry,
            aggregate: Some(AggregatePolicy::default()),
            requires: vec![],
        }
    }

    /// Wrap a single verifier; the processor takes the verifier's name.
    pub fn single(v: Arc<dyn Verifier>) -> Self {
        let mut registry = VerifierRegistry::new();
        let name = v.descriptor().name;
        registry.register(v);
        VerifyProcessor {
            name,
            registry,
            aggregate: None,
            requires: vec![],
        }
    }

    pub fn requiring(mut self, caps: Vec<Capability>) -> Self {
        self.requires = caps;
        self
    }
}

#[async_trait]
impl Processor for VerifyProcessor {
    fn descriptor(&self) -> ProcessorDescriptor {
        let ds = self.registry.descriptors();
        ProcessorDescriptor {
            name: self.name.clone(),
            version: "1".into(),
            consumes: vec![ArtifactKind::CLAIMS.into(), ArtifactKind::TRANSCRIPT.into()],
            produces: vec![ArtifactKind::VERDICTS.into()],
            needs_audio: false,
            requires: self.requires.clone(),
            description: if ds.len() == 1 {
                ds[0].description.clone()
            } else {
                format!(
                    "Runs {}{}.",
                    ds.iter()
                        .map(|d| d.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    if self.aggregate.is_some() {
                        " and aggregates them into one label per claim"
                    } else {
                        ""
                    }
                )
            },
        }
    }

    async fn run(
        &self,
        ctx: &JobCtx,
        inputs: Vec<Artifact>,
    ) -> almond_types::Result<Vec<ArtifactDraft>> {
        ctx.check_cancel()?;
        let mut input_ids: Vec<_> = inputs.iter().map(|a| a.id.clone()).collect();
        let (vctx, claims) = build_context(ctx, inputs)?;
        input_ids.sort();
        let only: Option<Vec<String>> = ctx
            .params
            .get("verifiers")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        (ctx.progress)(0.05, "verifying claims");
        let out = self
            .registry
            .run(&claims.claims, &vctx, only.as_deref())
            .await;
        ctx.check_cancel()?;
        if out.verdicts.is_empty()
            && !out.errors.is_empty()
            && out.errors.len() == self.registry.names().len()
        {
            return Err(almond_types::Error::Other(format!(
                "all verifiers failed: {:?}",
                out.errors
            )));
        }
        let mut verdicts = out.verdicts;
        if let Some(p) = &self.aggregate {
            let ids: Vec<String> = claims.claims.iter().map(|c| c.id.clone()).collect();
            let agg = aggregate::aggregate(&vctx.claims_artifact.id, &ids, &verdicts, p);
            verdicts.extend(agg);
        }
        let model = verdicts
            .iter()
            .filter_map(|v| v.trace.as_ref()?.get("model")?.as_str().map(String::from))
            .next();
        (ctx.progress)(1.0, "done");
        // Note: per-verifier failures (out.errors) have no slot in `Verdicts` yet; see
        // docs/contract-requests/ws-h.md. Other verifiers' verdicts are still returned.
        let prov = Provenance {
            processor: self.name.clone(),
            processor_version: "1".into(),
            model,
            params_hash: None,
            inputs: input_ids,
        };
        Ok(vec![ArtifactDraft::new(
            ctx.meeting.id.clone(),
            &Verdicts { verdicts },
            prov,
        )?])
    }
}
