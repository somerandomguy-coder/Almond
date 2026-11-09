//! # almond-verify — verification layer (WS-H)
//!
//! Turns AI output (summary bullets, AI note blocks, action items) into atomic **claims** and
//! checks them with a registry of **verifiers**, producing **verdicts** with evidence and (for
//! symbolic verifiers) a machine-readable derivation in `Verdict::trace`.
//!
//! ```text
//! summary ─► verify.extract_claims ─► claims ─► verify.all ─► verdicts (+ verify.aggregate)
//!                                                │
//!                 citation_exists, numbers, dates, entities, calendar, consistency (Datalog), nli
//! ```
//!
//! - [`extract`]: claim extraction (rules or LLM with JSON schema) incl. `triples.v1` logical forms.
//! - [`verifiers`]: baseline verifiers; [`registry`]: name -> verifier; [`aggregate`]: precedence
//!   rules to one label per claim; [`adapter`]: verifiers as pipeline `Processor`s.
//! - [`eval`]: hallucination fixtures + precision/recall harness (see `EVAL.md`).
//!
//! Normalizers ([`numbers`], [`dates`], [`names`]) understand Vietnamese and English
//! ("hai trăm triệu", "200tr", "thứ Sáu tuần sau", "trước tháng 12", "by December").

pub mod adapter;
pub mod aggregate;
pub mod dates;
pub mod eval;
pub mod evidence;
pub mod extract;
pub mod llm;
pub mod names;
pub mod numbers;
pub mod registry;
pub mod text;
pub mod triples;
pub mod verifiers;

pub use adapter::{VerifyProcessor, build_context};
pub use aggregate::{AggregatePolicy, aggregate};
pub use extract::{ClaimExtractor, ExtractClaimsProcessor};
pub use registry::VerifierRegistry;
pub use verifiers::VerifyConfig;

pub use almond_types as types;
