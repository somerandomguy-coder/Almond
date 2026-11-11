//! Offload policy: a weak device (the Fedora laptop) leaves heavy jobs for a stronger paired peer
//! (the home PC, reachable over Tailscale) instead of claiming them itself.
//!
//! Setting `processing.offload` (unset: `"prefer_peer"` on devices without a GPU, else `"local"`):
//! - `"local"`: run everything this device can (the runner's normal behavior).
//! - `"prefer_peer"`: leave a job unclaimed while a paired peer with a `gpu` that can run it was
//!   synced with recently, for at most `processing.offload_wait_min` minutes (default 20) after
//!   the job was requested; then run it here anyway. Jobs pinned to this device are never left,
//!   and LLM jobs aren't left while the remote Ollama (`llm.remote_ollama_url`) is in use: they
//!   already run on the PC's GPU from here.
//!
//! The PC picks deferred jobs up on its next sync with this device (turn on
//! `sync.auto_while_open` on both, or press Sync), fetches the audio, runs them and syncs the
//! artifacts back.

use crate::Core;
use crate::settings::keys;
use almond_pipeline::DeferFn;
use almond_types::DeviceId;
use almond_types::job::{Capability, Job};
use almond_types::sync::Transport;
use almond_types::time::{UnixMs, now_ms};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

/// Last time each peer was reachable: a sync either side started, or discovery. Refreshed by the
/// job worker before each round ([`refresh_reachability`]): `discover()` is async, the defer hook
/// isn't, and `last_synced` alone only counts syncs this device started.
#[derive(Default)]
pub(crate) struct Reachability(Mutex<HashMap<DeviceId, UnixMs>>);

pub(crate) async fn refresh_reachability(core: &Core) {
    if mode(core) == Mode::Local {
        return;
    }
    match core.node.transport.discover().await {
        Ok(peers) => {
            let mut m = core.processing.reach.0.lock().unwrap();
            for p in peers {
                let t = p.last_seen.max(p.last_synced);
                if let Some(t) = t {
                    let e = m.entry(p.device.id).or_insert(t);
                    *e = (*e).max(t);
                }
            }
        }
        Err(e) => tracing::debug!("offload: discover failed: {e}"),
    }
}

/// A peer counts as reachable if a sync with it succeeded this recently.
const PEER_RECENT_MS: i64 = 15 * 60_000;
const DEFAULT_WAIT_MIN: i64 = 20;
/// The policy is asked once per claimable job; re-read settings and peers at most this often.
const SNAPSHOT_TTL_MS: i64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Local,
    PreferPeer,
}

/// What the decision needs, read from settings / store / transport.
#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    pub mode: Mode,
    pub wait_ms: i64,
    /// LLM calls go to the remote Ollama right now.
    pub remote_llm: bool,
    /// Paired peers: advertised capabilities and last successful sync.
    pub peers: Vec<(Vec<Capability>, Option<UnixMs>)>,
}

fn is_llm(c: &Capability) -> bool {
    let s = c.0.as_str();
    s == "llm" || s.starts_with("llm:") || s == "embed" || s.starts_with("embed:")
}

/// `true` = leave `job` for a stronger peer for now.
pub(crate) fn should_defer(job: &Job, now: UnixMs, s: &Snapshot) -> bool {
    if s.mode == Mode::Local {
        return false;
    }
    if job.requires.iter().any(|r| r.0.starts_with("device:")) {
        // Pinned to a device: to this one (run it) or another (we can't claim it anyway).
        return false;
    }
    if now - job.requested_at.wall_ms >= s.wait_ms {
        return false; // waited long enough: the peer isn't coming
    }
    if s.remote_llm && job.requires.iter().any(is_llm) {
        return false; // already uses the PC's GPU through its Ollama
    }
    s.peers.iter().any(|(caps, last_synced)| {
        let recent = last_synced.is_some_and(|t| now - t <= PEER_RECENT_MS);
        let strong = caps.iter().any(|c| c.0 == "gpu");
        let able = job
            .requires
            .iter()
            .all(|r| caps.iter().any(|c| c.satisfies(r)));
        recent && strong && able
    })
}

/// Effective policy: the setting, else `prefer_peer` without a GPU. On the Fedora laptop the
/// post-meeting transcript runs at ~4.5x real time on the CPU (whisper-small language ID) vs
/// ~0.05x on the PC; without a recently synced GPU peer `prefer_peer` changes nothing.
pub(crate) fn mode(core: &Core) -> Mode {
    match core
        .setting_as::<String>(keys::OFFLOAD)
        .ok()
        .flatten()
        .as_deref()
    {
        Some("prefer_peer") => Mode::PreferPeer,
        Some("local") => Mode::Local,
        // The advertised `gpu` capability (detected NVIDIA GPU or configured), as for peers.
        _ if !core
            .processing
            .capabilities()
            .capabilities
            .iter()
            .any(|c| c.0 == "gpu") =>
        {
            Mode::PreferPeer
        }
        _ => Mode::Local,
    }
}

impl Mode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Mode::Local => "local",
            Mode::PreferPeer => "prefer_peer",
        }
    }
}

fn snapshot(core: &Core) -> Snapshot {
    let mode = mode(core);
    let wait_min = core
        .setting_as::<i64>(keys::OFFLOAD_WAIT_MIN)
        .ok()
        .flatten()
        .unwrap_or(DEFAULT_WAIT_MIN)
        .max(0);
    let me = core.device_id().clone();
    let peers = if mode == Mode::Local {
        vec![]
    } else {
        let known = core.store().devices().unwrap_or_default();
        let reach = core.processing.reach.0.lock().unwrap().clone();
        core.node
            .transport
            .paired_peers()
            .into_iter()
            .filter(|p| p.info.id != me)
            .map(|p| {
                // Capabilities as the peer last announced them (replicated `device.info`).
                let caps = known
                    .iter()
                    .find(|d| d.id == p.info.id)
                    .map(|d| d.capabilities.clone())
                    .unwrap_or_else(|| p.info.capabilities.clone());
                let seen = reach.get(&p.info.id).copied();
                (caps, p.last_synced.max(seen))
            })
            .collect()
    };
    Snapshot {
        mode,
        wait_ms: wait_min * 60_000,
        remote_llm: core.processing.remote_ollama_live().is_some(),
        peers,
    }
}

/// The runner's defer hook for `core` (reads a snapshot cached for a few seconds).
pub(crate) fn defer_fn(core: Weak<Core>) -> DeferFn {
    let cache: Mutex<Option<(UnixMs, Arc<Snapshot>)>> = Mutex::new(None);
    Arc::new(move |job: &Job| {
        let Some(core) = core.upgrade() else {
            return false;
        };
        let now = now_ms();
        let snap = {
            let mut c = cache.lock().unwrap();
            match &*c {
                Some((at, s)) if now - at < SNAPSHOT_TTL_MS => s.clone(),
                _ => {
                    let s = Arc::new(snapshot(&core));
                    *c = Some((now, s.clone()));
                    s
                }
            }
        };
        should_defer(job, now, &snap)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::time::Hlc;
    use almond_types::{DeviceId, Id};

    fn caps(v: &[&str]) -> Vec<Capability> {
        v.iter().map(|c| Capability::new(*c)).collect()
    }

    fn job(requires: &[&str], at: UnixMs) -> Job {
        Job {
            id: Id::new(),
            meeting_id: Id::new(),
            processor: "x".into(),
            inputs: vec![],
            params: serde_json::Value::Null,
            requires: caps(requires),
            requested_by: DeviceId("laptop".into()),
            requested_at: Hlc {
                wall_ms: at,
                logical: 0,
            },
            priority: 0,
        }
    }

    const NOW: UnixMs = 1_000_000_000;

    fn snap(mode: Mode, remote_llm: bool, synced_ago_ms: Option<i64>) -> Snapshot {
        Snapshot {
            mode,
            wait_ms: 20 * 60_000,
            remote_llm,
            peers: vec![(
                caps(&[
                    "gpu",
                    "llm",
                    "llm:gemma4:12b",
                    "stt:vi",
                    "stt:en",
                    "diarize",
                ]),
                synced_ago_ms.map(|a| NOW - a),
            )],
        }
    }

    #[test]
    fn local_mode_never_defers() {
        assert!(!should_defer(
            &job(&["stt:vi"], NOW),
            NOW,
            &snap(Mode::Local, false, Some(0))
        ));
    }

    #[test]
    fn defers_to_a_recent_gpu_peer_that_can_run_it() {
        let s = snap(Mode::PreferPeer, false, Some(60_000));
        assert!(should_defer(&job(&["stt:vi"], NOW), NOW, &s));
        assert!(should_defer(&job(&["llm"], NOW), NOW, &s));
        // The peer can't run it: keep it.
        assert!(!should_defer(&job(&["stt:zh"], NOW), NOW, &s));
    }

    #[test]
    fn runs_here_after_waiting_or_when_the_peer_is_away() {
        let s = snap(Mode::PreferPeer, false, Some(60_000));
        assert!(!should_defer(&job(&["stt:vi"], NOW - 21 * 60_000), NOW, &s));
        let away = snap(Mode::PreferPeer, false, Some(2 * 60 * 60_000));
        assert!(!should_defer(&job(&["stt:vi"], NOW), NOW, &away));
        let never = snap(Mode::PreferPeer, false, None);
        assert!(!should_defer(&job(&["stt:vi"], NOW), NOW, &never));
    }

    #[test]
    fn llm_jobs_stay_when_the_remote_ollama_is_used() {
        let s = snap(Mode::PreferPeer, true, Some(0));
        assert!(!should_defer(&job(&["llm"], NOW), NOW, &s));
        assert!(should_defer(&job(&["stt:vi"], NOW), NOW, &s));
    }

    #[test]
    fn pinned_jobs_are_not_deferred() {
        let s = snap(Mode::PreferPeer, false, Some(0));
        assert!(!should_defer(&job(&["llm", "device:laptop"], NOW), NOW, &s));
    }
}
