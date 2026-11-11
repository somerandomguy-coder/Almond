//! # almond-pipeline
//!
//! - [`registry::ProcessorRegistry`]: runnable + descriptor-only processors, cycle-checked DAG.
//! - [`planner`]: which processors to run for a meeting given its live artifacts.
//! - [`runner::JobRunner`]: replicated job lifecycle over the `Store` event log
//!   (`job.requested` → `job.claimed{lease}` → `artifact.add` → `job.finished`), capability
//!   matching, lease renewal/expiry, retries, cancel, rerun-with-supersede, progress events.
//! - [`external`]: JSON-RPC 2.0 external processors over stdio or HTTP
//!   (see `examples/external_processor.py`).
//!
//! Store calls are synchronous (per the `Store` contract) and made from async code; they are
//! expected to be quick (SQLite / in-memory).

pub mod external;
pub mod jobs;
pub mod planner;
pub mod registry;
pub mod runner;

pub use almond_types as types;
pub use jobs::{JobState, job_states};
pub use planner::{PlanPolicy, PlannedJob};
pub use registry::ProcessorRegistry;
pub use runner::{DeferFn, JobRequest, JobRunner, RunResult, RunnerConfig};
