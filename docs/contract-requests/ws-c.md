# WS-C contract requests (LLM + pipeline)

All additive. WS-C works today without them (local shims noted).

## 1. `Store::job_states` (lease + outcome view)

**What**
```rust
// almond-types/src/store.rs
pub struct JobRecord {
    pub job: Job,
    pub status: JobStatus,
    pub leases: Vec<JobLease>,      // all claims, in HLC order
    pub outcomes: Vec<JobOutcome>,  // may be >1 after offline races
}
pub trait Store {
    // ...
    fn job_records(&self, status: Option<JobStatus>) -> crate::Result<Vec<JobRecord>>;
}
```
**Why** `Store::jobs` only returns a status. Lease expiry/reclaim and the
"lowest (claimed_at, device) wins" rule need the leases. `almond-pipeline::jobs::job_states`
currently rebuilds them by scanning the whole log (`events_missing(&VersionVector::default(), usize::MAX)`),
which is O(log) per call.
**Compat** additive; MemStore + WS-D's SQLite store implement it from the events they already store.

Also: MemStore materializes `JobFinished` as last-writer status; with two devices finishing the
same job, `Done` should win over `Failed` (pipeline already resolves it this way).

## 2. `processor.cancel` notification in docs/API.md

**What** document host → processor notification `processor.cancel {job_id}` under
"External processors". The host already sends it (`almond-pipeline::external`); the Python example
honours it.
**Compat** docs only.

## 3. (Optional) `Artifact` body for `talk_time`

The example external processor produces `kind = "talk_time"`
(`{ total_ms, speakers: [{speaker, ms, segments, words, share, cites}] }`). Only needed if the UI
wants to show it; unknown kinds already sync as opaque JSON.

## 4. Notes text for `enhance_notes.llm`

`enhance_notes.llm` takes the user's notes as plain text in job params (`params.user_notes`),
because notes are a Loro doc and the pipeline shouldn't depend on Loro. The integrator (core)
should read the notes doc and put its plain text into the job params when requesting the job.
No type change needed; listed so Phase 2 wiring doesn't miss it.
