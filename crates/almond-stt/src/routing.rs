//! Per-language model routing table (ARCHITECTURE §10), keyed by device class.
//!
//! Defaults come from the measurements in `eval/stt/RESULTS.md`. The table is plain data so the
//! app can override it from settings.

use almond_types::LangTag;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Coarse hardware classes we pick defaults for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClass {
    /// Desktop with a CUDA/Vulkan GPU, >= 8 GB VRAM (the RTX 5060 Ti PC).
    DesktopGpu,
    /// Laptop/desktop, CPU only (the Fedora laptop).
    DesktopCpu,
    /// Phone (Samsung A26): small on-device models only.
    Mobile,
}

/// Which model to use for what.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Route {
    /// Offline / post-meeting transcription (quality first).
    pub offline: String,
    /// Live captions (latency first).
    pub live: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingTable {
    pub device: DeviceClass,
    /// Primary language subtag ("vi", "en", "zh") -> route.
    pub routes: BTreeMap<String, Route>,
    /// Used for languages not in `routes` and for language identification.
    pub fallback: Route,
}

impl RoutingTable {
    pub fn default_for(device: DeviceClass) -> Self {
        let r = |o: &str, l: &str| Route {
            offline: o.into(),
            live: l.into(),
        };
        let (vi, en, fallback) = match device {
            // See eval/stt/RESULTS.md. PhoWhisper-medium: 4.9 % WER on VIVOS with punctuation and
            // casing; PhoWhisper-large is not measured yet. Zipformer-30M (4.5 %, RTF 0.03 on
            // CPU) drives live captions everywhere.
            DeviceClass::DesktopGpu => (
                r("whisper:phowhisper-medium", "sherpa:zipformer-vi-30m-int8"),
                r("whisper:large-v3-turbo", "whisper:large-v3-turbo"),
                r("whisper:large-v3-turbo", "whisper:large-v3-turbo"),
            ),
            // Whisper large-class models run slower than real time on CPU: Vietnamese goes to
            // Zipformer, English to whisper-small (or is offloaded to the PC).
            DeviceClass::DesktopCpu => (
                r(
                    "sherpa:zipformer-vi-30m-int8",
                    "sherpa:zipformer-vi-30m-int8",
                ),
                r("whisper:small", "whisper:small"),
                r("whisper:small", "whisper:small"),
            ),
            DeviceClass::Mobile => (
                r(
                    "sherpa:zipformer-vi-30m-int8",
                    "sherpa:zipformer-vi-30m-int8",
                ),
                // No EN on-device model yet; offload EN meetings to the PC.
                r("whisper:small", "whisper:small"),
                r("whisper:small", "whisper:small"),
            ),
        };
        let mut routes = BTreeMap::new();
        routes.insert("vi".into(), vi);
        routes.insert("en".into(), en.clone());
        routes.insert("zh".into(), en);
        RoutingTable {
            device,
            routes,
            fallback,
        }
    }

    pub fn route(&self, lang: Option<&LangTag>) -> &Route {
        lang.and_then(|l| self.routes.get(&l.primary()))
            .unwrap_or(&self.fallback)
    }

    /// Every model id referenced by this table (for "download recommended models").
    pub fn model_ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .routes
            .values()
            .chain(std::iter::once(&self.fallback))
            .flat_map(|r| [r.offline.clone(), r.live.clone()])
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Registry;

    #[test]
    fn all_routed_models_exist_in_registry() {
        let reg = Registry::builtin();
        for d in [
            DeviceClass::DesktopGpu,
            DeviceClass::DesktopCpu,
            DeviceClass::Mobile,
        ] {
            for id in RoutingTable::default_for(d).model_ids() {
                assert!(reg.get(&id).is_some(), "{d:?}: {id} missing from registry");
            }
        }
    }

    #[test]
    fn routes_by_primary_subtag() {
        let t = RoutingTable::default_for(DeviceClass::DesktopGpu);
        assert_eq!(
            t.route(Some(&LangTag("vi-VN".into()))).offline,
            "whisper:phowhisper-medium"
        );
        assert_eq!(t.route(None).offline, t.fallback.offline);
        assert_eq!(
            t.route(Some(&LangTag("fr".into()))).offline,
            t.fallback.offline
        );
    }
}
