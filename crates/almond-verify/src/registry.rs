//! Verifier registry: name -> verifier, plus "run these on those claims".

use crate::verifiers::{
    CalendarVerifier, CitationExistsVerifier, DatesVerifier, DistortionVerifier, EntitiesVerifier,
    NliVerifier, NumbersVerifier, RevisionVerifier, VerifyConfig,
};
use almond_types::artifact::body::{Claim, Verdict};
use almond_types::llm::LlmProvider;
use almond_types::verify::{Verifier, VerifierDescriptor, VerifyContext};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct VerifierRegistry {
    verifiers: BTreeMap<String, Arc<dyn Verifier>>,
}

/// Result of running several verifiers: verdicts plus per-verifier failures (a failing verifier
/// never sinks the others).
#[derive(Debug, Clone, Default)]
pub struct RunOutput {
    pub verdicts: Vec<Verdict>,
    pub errors: Vec<(String, String)>,
}

impl VerifierRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// All deterministic verifiers (no LLM): citation_exists, numbers, dates, entities, calendar
    /// and (feature `datalog`) consistency.
    pub fn symbolic(config: VerifyConfig) -> Self {
        let mut r = Self::new();
        r.register(Arc::new(CitationExistsVerifier {
            config: config.clone(),
        }));
        r.register(Arc::new(NumbersVerifier {
            config: config.clone(),
        }));
        r.register(Arc::new(DatesVerifier {
            config: config.clone(),
        }));
        r.register(Arc::new(EntitiesVerifier {
            config: config.clone(),
        }));
        r.register(Arc::new(CalendarVerifier {
            config: config.clone(),
        }));
        r.register(Arc::new(DistortionVerifier {
            config: config.clone(),
        }));
        r.register(Arc::new(RevisionVerifier {
            config: config.clone(),
        }));
        #[cfg(feature = "datalog")]
        r.register(Arc::new(crate::verifiers::ConsistencyVerifier {
            config: config.clone(),
            derive_missing: true,
        }));
        r
    }

    /// Symbolic verifiers + `verify.nli` on the given LLM.
    pub fn with_llm(config: VerifyConfig, llm: Arc<dyn LlmProvider>, model: &str) -> Self {
        let mut r = Self::symbolic(config.clone());
        let mut nli = NliVerifier::new(llm, model);
        nli.config = config;
        r.register(Arc::new(nli));
        r
    }

    /// Add or replace (by descriptor name).
    pub fn register(&mut self, v: Arc<dyn Verifier>) -> &mut Self {
        self.verifiers.insert(v.descriptor().name, v);
        self
    }

    pub fn remove(&mut self, name: &str) -> Option<Arc<dyn Verifier>> {
        self.verifiers.remove(name)
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Verifier>> {
        self.verifiers.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.verifiers.keys().cloned().collect()
    }

    pub fn descriptors(&self) -> Vec<VerifierDescriptor> {
        self.verifiers.values().map(|v| v.descriptor()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.verifiers.is_empty()
    }

    /// Run the selected verifiers (all when `only` is `None`) sequentially.
    pub async fn run(
        &self,
        claims: &[Claim],
        ctx: &VerifyContext,
        only: Option<&[String]>,
    ) -> RunOutput {
        let mut out = RunOutput::default();
        for (name, v) in &self.verifiers {
            if only.is_some_and(|o| !o.iter().any(|n| n == name)) {
                continue;
            }
            match v.verify(claims, ctx).await {
                Ok(vs) => out.verdicts.extend(vs),
                Err(e) => out.errors.push((name.clone(), e.to_string())),
            }
        }
        out
    }
}
