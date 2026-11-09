//! The artifact DAG: which processors should run for a meeting given the artifacts it has.
//!
//! Default graph (with the LLM processors registered):
//! ```text
//! transcript ─┬─► summarize.llm      ─► summary ─► (extract_claims ─► claims ─► verify.* ─► verdicts)
//!             ├─► enhance_notes.llm  ─► enhanced_notes
//!             ├─► action_items.llm   ─► action_items
//!             └─► title.llm          ─► title
//! ```
//! The graph is *implied by descriptors* (`consumes` -> `produces`), so new processors (verifiers,
//! external Python processors) slot in without code changes here.
//!
//! A processor is planned for a meeting when every kind it consumes has a **live** (not
//! superseded) artifact, and neither a job nor an artifact for the same processor + inputs exists.

use crate::jobs::JobState;
use crate::registry::ProcessorRegistry;
use almond_types::job::{Capability, JobStatus};
use almond_types::{Artifact, Id};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// What to schedule automatically.
#[derive(Debug, Clone, Default)]
pub struct PlanPolicy {
    /// Processor names to auto-schedule; `None` = every known processor that consumes artifacts.
    pub auto: Option<BTreeSet<String>>,
    /// Params per processor (e.g. `{"lang": "vi"}` for `summarize.llm`).
    pub params: BTreeMap<String, Value>,
    pub priority: i32,
    /// Re-plan processors whose previous job failed (default: no, avoid retry storms).
    pub retry_failed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedJob {
    pub processor: String,
    pub inputs: Vec<Id>,
    pub requires: Vec<Capability>,
    pub params: Value,
    pub priority: i32,
}

/// Artifacts that no other artifact supersedes, latest first.
pub fn live_artifacts(all: &[Artifact]) -> Vec<&Artifact> {
    let superseded: HashSet<&Id> = all.iter().filter_map(|a| a.supersedes.as_ref()).collect();
    let mut v: Vec<&Artifact> = all.iter().filter(|a| !superseded.contains(&a.id)).collect();
    v.sort_by_key(|a| std::cmp::Reverse(a.created_at));
    v
}

/// Latest live artifact of `kind`.
pub fn latest_live<'a>(all: &'a [Artifact], kind: &str) -> Option<&'a Artifact> {
    live_artifacts(all).into_iter().find(|a| a.kind.is(kind))
}

/// Jobs to request for one meeting (in dependency order).
pub fn plan(
    registry: &ProcessorRegistry,
    meeting: &Id,
    artifacts: &[Artifact],
    jobs: &BTreeMap<Id, JobState>,
    policy: &PlanPolicy,
) -> Vec<PlannedJob> {
    let order = registry.topo_order().unwrap_or_default();
    let mut out = vec![];
    for name in order {
        if let Some(auto) = &policy.auto
            && !auto.contains(&name)
        {
            continue;
        }
        let Some(d) = registry.descriptor(&name) else {
            continue;
        };
        if d.needs_audio || d.consumes.is_empty() {
            continue; // transcription is started by the recorder, not the DAG
        }
        let inputs: Option<Vec<Id>> = d
            .consumes
            .iter()
            .map(|k| latest_live(artifacts, k).map(|a| a.id.clone()))
            .collect();
        let Some(inputs) = inputs else {
            continue;
        };
        let want: BTreeSet<&Id> = inputs.iter().collect();
        let job_exists = jobs.values().any(|s| {
            s.job.meeting_id == *meeting
                && s.job.processor == name
                && s.job.inputs.iter().collect::<BTreeSet<_>>() == want
                && (!policy.retry_failed || s.status() != JobStatus::Failed)
        });
        let artifact_exists = artifacts.iter().any(|a| {
            a.provenance.processor == name
                && a.provenance.inputs.iter().collect::<BTreeSet<_>>() == want
        });
        if job_exists || artifact_exists {
            continue;
        }
        out.push(PlannedJob {
            processor: name.clone(),
            inputs,
            requires: d.requires.clone(),
            params: policy.params.get(&name).cloned().unwrap_or(Value::Null),
            priority: policy.priority,
        });
    }
    out
}
