//! Fast offline diarization for long recordings (engine-agnostic part).
//!
//! The sherpa-onnx pyannote pipeline took ~270 s (RTF 0.15) on the 30-min test meeting, ~80 % of
//! the post-meeting time: it computes a speaker embedding for every (10 s window, local speaker)
//! pair of a sliding window with a 1 s step, i.e. every second of audio is embedded ~10 times.
//! [`FastDiarizer`] embeds every second of *speech* about once:
//!
//! 1. **Speech regions** from the energy VAD (silence >= `min_silence_ms` separates turns).
//! 2. **Windows**: each region is cut into windows of ~`window_ms` (the remainder joins the last
//!    window), embedded in parallel by `workers` extractors.
//! 3. **Clustering**: agglomerative, average linkage on cosine distance, stopping at
//!    `threshold` ([`agglomerate`]); windows too short to embed take the label of the nearest
//!    labelled window in time; clusters with less than `min_speaker_ms` of speech are folded
//!    into the closest larger cluster.
//! 4. **Turns**: consecutive windows of one speaker merge into one turn.
//!
//! The trade-off: a speaker change without any pause inside one window is not split (the
//! pyannote segmentation model would split it). Meetings are mostly turn-taking with pauses;
//! the accuracy on the synthetic meeting is in `eval/stt/RESULTS.md`.
//!
//! The model-specific part is an [`Embedder`] (sherpa-onnx 3D-Speaker in [`crate::sherpa`]).

use crate::audio::{VadParams, ms_to_samples, samples_to_ms, split_on_silence};
use almond_types::Result;
use almond_types::ids::SpeakerId;
use almond_types::stt::SpeakerTurn;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Speaker embeddings. Blocking; must be callable from several threads at once.
pub trait Embedder: Send + Sync {
    /// Embedding of `samples` (16 kHz mono). May fail for very short input.
    fn embed(&self, samples: &[f32]) -> Result<Vec<f32>>;
}

/// Tuning for [`FastDiarizer`].
#[derive(Debug, Clone)]
pub struct FastConfig {
    /// Silence that separates two speech regions (a turn boundary).
    pub min_silence_ms: u64,
    /// Target window length inside a region.
    pub window_ms: u64,
    /// Shorter windows are not embedded (labelled from their neighbours).
    pub min_window_ms: u64,
    /// Cosine distance (1 - cosine similarity) up to which clusters merge.
    pub threshold: f32,
    /// Clusters with less speech than this are folded into the closest larger one.
    pub min_speaker_ms: u64,
    /// Parallel embedding workers.
    pub workers: usize,
}

impl Default for FastConfig {
    fn default() -> Self {
        FastConfig {
            min_silence_ms: 300,
            window_ms: 3000,
            min_window_ms: 800,
            threshold: 0.6,
            min_speaker_ms: 4000,
            workers: std::thread::available_parallelism()
                .map(|n| (n.get() / 3).clamp(1, 4))
                .unwrap_or(2),
        }
    }
}

/// Speech regions cut into windows of about `window_ms` (sample ranges).
pub fn windows(x: &[f32], cfg: &FastConfig) -> Vec<(usize, usize)> {
    let vad = VadParams {
        min_silence_ms: cfg.min_silence_ms,
        max_region_ms: u64::MAX / 2,
        pad_ms: 50,
        ..VadParams::default()
    };
    let w = ms_to_samples(cfg.window_ms).max(1);
    let mut out = vec![];
    for (a, b) in split_on_silence(x, &vad) {
        let n = ((b - a) / w).max(1);
        let step = (b - a) / n;
        for i in 0..n {
            let s = a + i * step;
            let e = if i + 1 == n { b } else { s + step };
            out.push((s, e));
        }
    }
    out
}

fn normalize(v: &mut [f32]) -> bool {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n == 0.0 || !n.is_finite() {
        return false;
    }
    v.iter_mut().for_each(|x| *x /= n);
    true
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Average-linkage agglomerative clustering of unit vectors on cosine distance, merging while
/// the closest pair of clusters is within `threshold`. Returns a label per item (0-based, in
/// order of first appearance). O(n^3) worst case; n is the number of windows (~600 for 30 min).
pub fn agglomerate(items: &[Vec<f32>], threshold: f32) -> Vec<usize> {
    let n = items.len();
    if n == 0 {
        return vec![];
    }
    // condensed full matrix of cluster distances (Lance-Williams updates for average linkage)
    let mut d = vec![0f32; n * n];
    for i in 0..n {
        for j in i + 1..n {
            let v = 1.0 - dot(&items[i], &items[j]);
            d[i * n + j] = v;
            d[j * n + i] = v;
        }
    }
    let mut size = vec![1usize; n];
    let mut alive = vec![true; n];
    let mut parent: Vec<usize> = (0..n).collect();
    loop {
        let mut best = (usize::MAX, usize::MAX, f32::INFINITY);
        for i in 0..n {
            if !alive[i] {
                continue;
            }
            for j in i + 1..n {
                if alive[j] && d[i * n + j] < best.2 {
                    best = (i, j, d[i * n + j]);
                }
            }
        }
        let (a, b, dist) = best;
        if a == usize::MAX || dist > threshold {
            break;
        }
        // merge b into a
        let (na, nb) = (size[a] as f32, size[b] as f32);
        for k in 0..n {
            if alive[k] && k != a && k != b {
                let v = (na * d[a * n + k] + nb * d[b * n + k]) / (na + nb);
                d[a * n + k] = v;
                d[k * n + a] = v;
            }
        }
        size[a] += size[b];
        alive[b] = false;
        parent[b] = a;
    }
    let root = |mut i: usize| {
        while parent[i] != i {
            i = parent[i];
        }
        i
    };
    let mut ids: Vec<usize> = vec![];
    (0..n)
        .map(|i| {
            let r = root(i);
            match ids.iter().position(|&x| x == r) {
                Some(p) => p,
                None => {
                    ids.push(r);
                    ids.len() - 1
                }
            }
        })
        .collect()
}

/// Window embeddings + clustering diarizer (see module docs).
pub struct FastDiarizer<E: Embedder> {
    pub embedder: E,
    pub cfg: FastConfig,
}

impl<E: Embedder> FastDiarizer<E> {
    /// Blocking. Turn times are on `x`'s timeline; speakers are "S1", "S2", ... by first
    /// appearance.
    pub fn diarize_sync(&self, x: &[f32]) -> Result<Vec<SpeakerTurn>> {
        let wins = windows(x, &self.cfg);
        if wins.is_empty() {
            return Ok(vec![]);
        }
        let min_len = ms_to_samples(self.cfg.min_window_ms);
        // embed in parallel (workers pull the next window index)
        let next = AtomicUsize::new(0);
        let mut embs: Vec<Option<Vec<f32>>> = vec![None; wins.len()];
        let results: Vec<Vec<(usize, Option<Vec<f32>>)>> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..self.cfg.workers.max(1))
                .map(|_| {
                    s.spawn(|| {
                        let mut out = vec![];
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(&(a, b)) = wins.get(i) else { break };
                            let e = if b - a >= min_len {
                                match self.embedder.embed(&x[a..b]) {
                                    Ok(mut e) => normalize(&mut e).then_some(e),
                                    Err(err) => {
                                        tracing::debug!("embedding window {i}: {err}");
                                        None
                                    }
                                }
                            } else {
                                None
                            };
                            out.push((i, e));
                        }
                        out
                    })
                })
                .collect();
            hs.into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });
        for (i, e) in results.into_iter().flatten() {
            embs[i] = e;
        }
        let have: Vec<usize> = (0..wins.len()).filter(|&i| embs[i].is_some()).collect();
        if have.is_empty() {
            // nothing long enough to embed: one speaker
            return Ok(merge_turns(
                wins.iter().map(|&(a, b)| (a, b, 0)).collect(),
                x.len(),
            ));
        }
        let vecs: Vec<Vec<f32>> = have
            .iter()
            .map(|&i| embs[i].clone().expect("filtered"))
            .collect();
        let mut lab = agglomerate(&vecs, self.cfg.threshold);
        fold_small_clusters(
            &mut lab,
            &vecs,
            &have
                .iter()
                .map(|&i| wins[i].1 - wins[i].0)
                .collect::<Vec<_>>(),
            ms_to_samples(self.cfg.min_speaker_ms),
        );
        // label every window: embedded ones directly, others from the nearest embedded window
        let mut labels = vec![0usize; wins.len()];
        for (k, &i) in have.iter().enumerate() {
            labels[i] = lab[k];
        }
        for i in 0..wins.len() {
            if embs[i].is_none() {
                let mid = (wins[i].0 + wins[i].1) / 2;
                let nearest = have
                    .iter()
                    .min_by_key(|&&j| {
                        let (a, b) = wins[j];
                        if mid < a {
                            a - mid
                        } else {
                            mid.saturating_sub(b)
                        }
                    })
                    .expect("non-empty");
                labels[i] = labels[*nearest];
            }
        }
        Ok(merge_turns(
            wins.iter()
                .zip(&labels)
                .map(|(&(a, b), &l)| (a, b, l))
                .collect(),
            x.len(),
        ))
    }
}

/// Re-label clusters with less than `min_len` samples of speech to the closest cluster (by
/// centroid) that has enough; then renumber by first appearance.
fn fold_small_clusters(lab: &mut [usize], vecs: &[Vec<f32>], lens: &[usize], min_len: usize) {
    let k = lab.iter().copied().max().map_or(0, |m| m + 1);
    let mut total = vec![0usize; k];
    for (i, &l) in lab.iter().enumerate() {
        total[l] += lens[i];
    }
    let big: Vec<usize> = (0..k).filter(|&c| total[c] >= min_len).collect();
    if big.is_empty() || big.len() == k {
        return;
    }
    let dim = vecs[0].len();
    let mut cent = vec![vec![0f32; dim]; k];
    for (i, &l) in lab.iter().enumerate() {
        for (c, v) in cent[l].iter_mut().zip(&vecs[i]) {
            *c += v;
        }
    }
    for c in cent.iter_mut() {
        normalize(c);
    }
    let target: Vec<usize> = (0..k)
        .map(|c| {
            if big.contains(&c) {
                c
            } else {
                *big.iter()
                    .max_by(|&&a, &&b| dot(&cent[c], &cent[a]).total_cmp(&dot(&cent[c], &cent[b])))
                    .expect("non-empty")
            }
        })
        .collect();
    let mut ids: Vec<usize> = vec![];
    for l in lab.iter_mut() {
        let t = target[*l];
        *l = match ids.iter().position(|&x| x == t) {
            Some(p) => p,
            None => {
                ids.push(t);
                ids.len() - 1
            }
        };
    }
}

/// Windows `(start, end, label)` in time order → turns; neighbours with the same label and a
/// gap < 1 s become one turn. Speakers are renumbered by first appearance.
fn merge_turns(mut w: Vec<(usize, usize, usize)>, total: usize) -> Vec<SpeakerTurn> {
    w.sort();
    let gap = ms_to_samples(1000);
    let mut order: Vec<usize> = vec![];
    let mut out: Vec<(usize, usize, usize)> = vec![];
    for (a, b, l) in w {
        let l = match order.iter().position(|&x| x == l) {
            Some(p) => p,
            None => {
                order.push(l);
                order.len() - 1
            }
        };
        match out.last_mut() {
            Some((_, e, pl)) if *pl == l && a <= *e + gap => *e = b.max(*e),
            _ => out.push((a, b.min(total), l)),
        }
    }
    out.into_iter()
        .map(|(a, b, l)| SpeakerTurn {
            t0_ms: samples_to_ms(a) as u32,
            t1_ms: samples_to_ms(b) as u32,
            speaker: SpeakerId(format!("S{}", l + 1)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burst(ms: u64, f: f32) -> Vec<f32> {
        (0..ms_to_samples(ms))
            .map(|i| {
                let t = i as f32 / 16_000.0;
                0.2 * (0.55 + 0.45 * (4.0 * t * 6.28).sin()) * (6.28 * f * t).sin()
            })
            .collect()
    }

    fn silence(ms: u64) -> Vec<f32> {
        vec![0.0; ms_to_samples(ms)]
    }

    /// "Speaker" = pitch: zero-crossing rate decides the embedding direction.
    struct ByPitch;
    impl Embedder for ByPitch {
        fn embed(&self, s: &[f32]) -> Result<Vec<f32>> {
            let zc = s
                .windows(2)
                .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
                .count() as f32
                / s.len().max(1) as f32;
            // 220 Hz -> ~0.0275, 440 Hz -> ~0.055
            Ok(if zc > 0.04 {
                vec![1.0, 0.1]
            } else {
                vec![0.1, 1.0]
            })
        }
    }

    #[test]
    fn clusters_by_embedding_and_keeps_time_order() {
        let mut x = vec![];
        for i in 0..6 {
            x.extend(burst(4000, if i % 2 == 0 { 220.0 } else { 440.0 }));
            x.extend(silence(1500));
        }
        x.extend(burst(500, 440.0)); // too short to embed: takes its neighbour's label
        let d = FastDiarizer {
            embedder: ByPitch,
            cfg: FastConfig {
                min_speaker_ms: 1000,
                ..Default::default()
            },
        };
        let turns = d.diarize_sync(&x).unwrap();
        let sp: Vec<&str> = turns.iter().map(|t| t.speaker.0.as_str()).collect();
        assert_eq!(sp, ["S1", "S2", "S1", "S2", "S1", "S2", "S2"], "{turns:?}");
        assert!(turns[0].t0_ms < 100);
        assert!((5300..5700).contains(&turns[1].t0_ms), "{turns:?}");
    }

    #[test]
    fn agglomerate_respects_threshold() {
        let mut v = vec![
            vec![1.0, 0.0],
            vec![0.99, 0.14],
            vec![0.0, 1.0],
            vec![0.1, 0.99],
        ];
        v.iter_mut().for_each(|x| {
            normalize(x);
        });
        assert_eq!(agglomerate(&v, 0.2), vec![0, 0, 1, 1]);
        assert_eq!(agglomerate(&v, 0.0001), vec![0, 1, 2, 3]);
        assert_eq!(agglomerate(&v, 2.0), vec![0, 0, 0, 0]);
    }

    #[test]
    fn small_clusters_fold_into_closest_big_one() {
        let vecs = vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![0.8, 0.6]];
        let mut lab = vec![0, 1, 2];
        fold_small_clusters(&mut lab, &vecs, &[100, 100, 10], 50);
        assert_eq!(lab, vec![0, 1, 0]);
    }
}
