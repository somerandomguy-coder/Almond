//! Job state derived from the replicated event log (`job.requested` / `job.claimed` /
//! `job.finished`). The `Store` view only exposes a status; leases and outcomes come from the log.
//!
//! Lease rules (from `almond_types::job::JobLease`):
//! - a claim is *live* until `expires_at` (wall clock) unless a `job.finished` arrives;
//! - among concurrent live claims the lowest `(claimed_at, device)` wins;
//! - once every claim has expired, any capable device may reclaim.

use almond_types::job::{Job, JobLease, JobOutcome, JobStatus};
use almond_types::store::Store;
use almond_types::time::UnixMs;
use almond_types::{DeviceId, Id, Result};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobState {
    pub job: Job,
    pub leases: Vec<JobLease>,
    pub outcomes: Vec<JobOutcome>,
}

impl JobState {
    /// Effective status: any `Done` wins, then `Cancelled`, then `Failed`; otherwise `Claimed`
    /// if any claim exists, else `Pending`.
    pub fn status(&self) -> JobStatus {
        for s in [JobStatus::Done, JobStatus::Cancelled, JobStatus::Failed] {
            if self.outcomes.iter().any(|o| o.status == s) {
                return s;
            }
        }
        if self.leases.is_empty() {
            JobStatus::Pending
        } else {
            JobStatus::Claimed
        }
    }

    pub fn is_finished(&self) -> bool {
        !self.outcomes.is_empty()
    }

    /// The winning live lease at `now`, if any.
    pub fn live_lease(&self, now: UnixMs) -> Option<&JobLease> {
        self.leases
            .iter()
            .filter(|l| l.expires_at > now)
            .min_by(|a, b| (a.claimed_at, &a.device).cmp(&(b.claimed_at, &b.device)))
    }

    /// True if `device` currently holds the winning lease.
    pub fn held_by(&self, device: &DeviceId, now: UnixMs) -> bool {
        self.live_lease(now).is_some_and(|l| &l.device == device)
    }

    /// Distinct claim rounds (a renewal by the same holder isn't a new attempt).
    pub fn claim_count(&self) -> usize {
        let mut n = 0;
        let mut last: Option<&DeviceId> = None;
        for l in &self.leases {
            if last != Some(&l.device) {
                n += 1;
            }
            last = Some(&l.device);
        }
        n
    }
}

/// Per-job states (claims and outcomes) from the store's job view.
pub fn job_states(store: &dyn Store) -> Result<BTreeMap<Id, JobState>> {
    Ok(store
        .job_records(None)?
        .into_iter()
        .map(|r| {
            (
                r.job.id.clone(),
                JobState {
                    job: r.job,
                    leases: r.leases,
                    outcomes: r.outcomes,
                },
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::time::Hlc;

    fn lease(dev: &str, at: i64, exp: i64) -> JobLease {
        JobLease {
            job_id: Id("j".into()),
            device: DeviceId(dev.into()),
            claimed_at: Hlc {
                wall_ms: at,
                logical: 0,
            },
            expires_at: exp,
        }
    }

    #[test]
    fn lease_resolution() {
        let job = Job {
            id: Id("j".into()),
            meeting_id: Id("m".into()),
            processor: "p".into(),
            inputs: vec![],
            params: serde_json::Value::Null,
            requires: vec![],
            requested_by: DeviceId("phone".into()),
            requested_at: Hlc::default(),
            priority: 0,
        };
        let mut s = JobState {
            job,
            leases: vec![],
            outcomes: vec![],
        };
        assert_eq!(s.status(), JobStatus::Pending);
        s.leases.push(lease("pc", 20, 100));
        s.leases.push(lease("laptop", 10, 100));
        assert_eq!(s.status(), JobStatus::Claimed);
        // Earliest claim wins while both live.
        assert!(s.held_by(&DeviceId("laptop".into()), 50));
        // Laptop's claim expired earlier in this variant -> pc wins.
        s.leases[1].expires_at = 40;
        assert!(s.held_by(&DeviceId("pc".into()), 50));
        assert!(s.live_lease(200).is_none());
        assert_eq!(s.claim_count(), 2);
        s.outcomes.push(JobOutcome {
            job_id: Id("j".into()),
            device: DeviceId("pc".into()),
            status: JobStatus::Failed,
            outputs: vec![],
            error: Some("x".into()),
        });
        assert_eq!(s.status(), JobStatus::Failed);
        s.outcomes[0].status = JobStatus::Done;
        assert_eq!(s.status(), JobStatus::Done);
    }
}
