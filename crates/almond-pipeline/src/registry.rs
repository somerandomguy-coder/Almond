//! Processor registry + the artifact DAG implied by processor descriptors
//! (`consumes` -> `produces`).
//!
//! Two kinds of entries:
//! - **runnable** processors (an `Arc<dyn Processor>` on this device), and
//! - **descriptor-only** entries for processors this device cannot run but may *request* (e.g. the
//!   phone knows `summarize.llm` exists and needs `llm`; a PC claims the job after sync).

use almond_types::pipeline::{Processor, ProcessorDescriptor};
use almond_types::{Error, Result};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, RwLock};

#[derive(Default)]
pub struct ProcessorRegistry {
    runnable: RwLock<BTreeMap<String, Arc<dyn Processor>>>,
    known: RwLock<BTreeMap<String, ProcessorDescriptor>>,
}

impl ProcessorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a runnable processor (replaces an entry with the same name). Fails if it would
    /// make the processor graph cyclic.
    pub fn register(&self, p: Arc<dyn Processor>) -> Result<()> {
        let d = p.descriptor();
        self.check_with(&d)?;
        self.known
            .write()
            .unwrap()
            .insert(d.name.clone(), d.clone());
        self.runnable.write().unwrap().insert(d.name, p);
        Ok(())
    }

    pub fn register_all(&self, ps: impl IntoIterator<Item = Arc<dyn Processor>>) -> Result<()> {
        for p in ps {
            self.register(p)?;
        }
        Ok(())
    }

    /// Know about a processor that runs elsewhere (for planning + capability requirements).
    pub fn register_descriptor(&self, d: ProcessorDescriptor) -> Result<()> {
        self.check_with(&d)?;
        self.known.write().unwrap().insert(d.name.clone(), d);
        Ok(())
    }

    pub fn unregister(&self, name: &str) {
        self.runnable.write().unwrap().remove(name);
        self.known.write().unwrap().remove(name);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Processor>> {
        self.runnable.read().unwrap().get(name).cloned()
    }

    pub fn descriptor(&self, name: &str) -> Option<ProcessorDescriptor> {
        self.known.read().unwrap().get(name).cloned()
    }

    pub fn is_runnable(&self, name: &str) -> bool {
        self.runnable.read().unwrap().contains_key(name)
    }

    /// All known descriptors (runnable + descriptor-only), by name.
    pub fn descriptors(&self) -> Vec<ProcessorDescriptor> {
        self.known.read().unwrap().values().cloned().collect()
    }

    /// Processors that consume `kind`.
    pub fn consumers_of(&self, kind: &str) -> Vec<ProcessorDescriptor> {
        self.known
            .read()
            .unwrap()
            .values()
            .filter(|d| d.consumes.iter().any(|k| k == kind))
            .cloned()
            .collect()
    }

    /// Processor names in dependency order (producers before consumers).
    pub fn topo_order(&self) -> Result<Vec<String>> {
        topo(&self.descriptors())
    }

    fn check_with(&self, d: &ProcessorDescriptor) -> Result<()> {
        let mut all: BTreeMap<String, ProcessorDescriptor> = self.known.read().unwrap().clone();
        all.insert(d.name.clone(), d.clone());
        topo(&all.into_values().collect::<Vec<_>>()).map(|_| ())
    }
}

/// Kahn's algorithm over "p produces a kind q consumes" edges.
pub fn topo(ds: &[ProcessorDescriptor]) -> Result<Vec<String>> {
    let mut indeg: BTreeMap<&str, usize> = ds.iter().map(|d| (d.name.as_str(), 0)).collect();
    let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for p in ds {
        for q in ds {
            if p.name != q.name && p.produces.iter().any(|k| q.consumes.contains(k)) {
                if edges.entry(&p.name).or_default().insert(&q.name) {
                    *indeg.get_mut(q.name.as_str()).unwrap() += 1;
                }
            } else if p.name == q.name && p.produces.iter().any(|k| p.consumes.contains(k)) {
                return Err(Error::Invalid(format!(
                    "processor {} consumes its own output",
                    p.name
                )));
            }
        }
    }
    let mut q: VecDeque<&str> = indeg
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(k, _)| *k)
        .collect();
    let mut out = vec![];
    while let Some(n) = q.pop_front() {
        out.push(n.to_string());
        for m in edges.get(n).into_iter().flatten() {
            let e = indeg.get_mut(m).unwrap();
            *e -= 1;
            if *e == 0 {
                q.push_back(m);
            }
        }
    }
    if out.len() != ds.len() {
        let stuck: Vec<&str> = indeg
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(k, _)| *k)
            .collect();
        return Err(Error::Invalid(format!(
            "processor graph has a cycle among {stuck:?}"
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::job::Capability;
    use almond_types::mock::MockProcessor;

    pub(crate) fn desc(name: &str, consumes: &[&str], produces: &[&str]) -> ProcessorDescriptor {
        ProcessorDescriptor {
            name: name.into(),
            version: "0".into(),
            consumes: consumes.iter().map(|s| s.to_string()).collect(),
            produces: produces.iter().map(|s| s.to_string()).collect(),
            needs_audio: consumes.is_empty(),
            requires: vec![Capability::new("llm")],
            description: String::new(),
        }
    }

    fn mock(d: ProcessorDescriptor) -> Arc<dyn Processor> {
        Arc::new(MockProcessor {
            desc: d,
            output: vec![],
        })
    }

    #[test]
    fn register_topo_and_cycles() {
        let r = ProcessorRegistry::new();
        r.register(mock(desc("summarize.llm", &["transcript"], &["summary"])))
            .unwrap();
        r.register_descriptor(desc("transcribe.whisper", &[], &["transcript"]))
            .unwrap();
        r.register(mock(desc("extract_claims", &["summary"], &["claims"])))
            .unwrap();
        assert_eq!(
            r.topo_order().unwrap(),
            vec!["transcribe.whisper", "summarize.llm", "extract_claims"]
        );
        assert!(r.is_runnable("summarize.llm"));
        assert!(!r.is_runnable("transcribe.whisper"));
        assert!(r.descriptor("transcribe.whisper").is_some());
        assert_eq!(r.consumers_of("transcript").len(), 1);
        // claims -> transcript would close a cycle.
        assert!(
            r.register(mock(desc("bad", &["claims"], &["transcript"])))
                .is_err()
        );
        assert!(r.register(mock(desc("self", &["x"], &["x"]))).is_err());
        assert!(r.descriptor("bad").is_none());
        r.unregister("extract_claims");
        assert_eq!(r.descriptors().len(), 2);
    }
}
