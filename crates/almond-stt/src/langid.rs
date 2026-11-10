//! Language decisions for mixed-language meetings: turns Whisper's per-utterance language
//! probabilities into a language per utterance that does not misroute the meeting's primary
//! language (eval/stt/RESULTS.md, "Language routing").
//!
//! Raw per-utterance language ID (argmax of Whisper's probabilities) fails on short utterances
//! and on accented / far-field Vietnamese: the user's first real test routed most of their
//! Vietnamese to English, where Whisper then hallucinated. [`decide`] is a small HMM over the
//! utterance sequence (Viterbi):
//!
//! * **prior** — the meeting's primary language (first of `allowed_languages`) is more likely;
//! * **evidence weight** — an utterance's log-probabilities count in proportion to its length
//!   (`full_evidence_ms`), and not at all below `min_evidence_ms`: very short utterances take
//!   their language from their neighbours / the prior;
//! * **probability floor** — Whisper is often over-confident on short or noisy audio, so a
//!   probability never counts as less than `p_floor`;
//! * **switch penalty** — changing language between neighbouring utterances costs
//!   `switch_penalty` nats, decaying with the silence between them (`context_ms`).
//!
//! The same policy runs online for live captions ([`OnlineLid`]): forward-only, starting from
//! the previous decision.

use almond_types::LangTag;

/// Tuning of [`decide`]. Defaults measured in eval/stt/RESULTS.md.
#[derive(Debug, Clone, PartialEq)]
pub struct LidPolicy {
    /// Prior probability of the primary language (the first allowed language). The rest is
    /// shared equally by the other allowed languages.
    pub primary_prior: f32,
    /// Below this, an utterance's LID probabilities are ignored.
    pub min_evidence_ms: u64,
    /// At and above this, an utterance's LID log-probabilities count fully (linear in between).
    pub full_evidence_ms: u64,
    /// Lower bound on a language probability (caps how much one utterance can say "not X").
    pub p_floor: f32,
    /// Cost (nats) of switching language between adjacent utterances with no gap.
    pub switch_penalty: f32,
    /// Silence time constant (ms) over which the switch penalty decays (e^-gap/context).
    pub context_ms: u64,
}

impl Default for LidPolicy {
    fn default() -> Self {
        LidPolicy {
            primary_prior: 0.75,
            min_evidence_ms: 400,
            full_evidence_ms: 2_000,
            p_floor: 0.02,
            switch_penalty: 1.0,
            context_ms: 3_000,
        }
    }
}

impl LidPolicy {
    /// No prior, no smoothing: the most probable language of each utterance (the behaviour
    /// before this policy existed; kept for comparisons).
    pub fn raw() -> Self {
        LidPolicy {
            primary_prior: 0.5,
            min_evidence_ms: 0,
            full_evidence_ms: 0,
            p_floor: 1e-9,
            switch_penalty: 0.0,
            context_ms: 0,
        }
    }
}

/// One utterance as seen by language ID.
#[derive(Debug, Clone, PartialEq)]
pub struct LidObs {
    /// Speech duration.
    pub dur_ms: u64,
    /// Silence since the previous utterance (ignored for the first).
    pub gap_ms: u64,
    /// Language probabilities (primary subtags), any order; missing languages count as 0.
    pub probs: Vec<(LangTag, f32)>,
}

impl LidPolicy {
    fn weight(&self, dur_ms: u64) -> f32 {
        if dur_ms < self.min_evidence_ms {
            0.0
        } else if dur_ms >= self.full_evidence_ms {
            1.0
        } else {
            dur_ms as f32 / self.full_evidence_ms as f32
        }
    }

    fn log_prior(&self, langs: &[LangTag]) -> Vec<f32> {
        let k = langs.len();
        if k <= 1 {
            return vec![0.0; k];
        }
        let p0 = self.primary_prior.clamp(0.01, 0.99);
        (0..k)
            .map(|i| {
                if i == 0 {
                    p0.ln()
                } else {
                    ((1.0 - p0) / (k - 1) as f32).ln()
                }
            })
            .collect()
    }

    /// Per-language emission score of one utterance (log prior + weighted log-likelihood).
    pub fn emission(&self, langs: &[LangTag], o: &LidObs) -> Vec<f32> {
        let w = self.weight(o.dur_ms);
        let prior = self.log_prior(langs);
        langs
            .iter()
            .zip(prior)
            .map(|(l, lp)| {
                let p = o
                    .probs
                    .iter()
                    .find(|(x, _)| x.primary() == l.primary())
                    .map(|x| x.1)
                    .unwrap_or(0.0);
                lp + w * p.max(self.p_floor).min(1.0).ln()
            })
            .collect()
    }

    fn switch_cost(&self, gap_ms: u64) -> f32 {
        if self.context_ms == 0 {
            return 0.0;
        }
        self.switch_penalty * (-(gap_ms as f32) / self.context_ms as f32).exp()
    }
}

/// Language per utterance (Viterbi). `langs` = allowed languages, primary first; empty -> empty.
pub fn decide(obs: &[LidObs], langs: &[LangTag], p: &LidPolicy) -> Vec<LangTag> {
    let k = langs.len();
    if k == 0 || obs.is_empty() {
        return vec![];
    }
    if k == 1 {
        return vec![langs[0].clone(); obs.len()];
    }
    let mut score: Vec<f32> = p.emission(langs, &obs[0]);
    let mut back: Vec<Vec<usize>> = vec![vec![0; k]];
    for o in &obs[1..] {
        let e = p.emission(langs, o);
        let sw = p.switch_cost(o.gap_ms);
        let mut next = vec![f32::NEG_INFINITY; k];
        let mut bp = vec![0; k];
        for j in 0..k {
            for (i, s) in score.iter().enumerate() {
                let v = s - if i == j { 0.0 } else { sw };
                if v > next[j] {
                    next[j] = v;
                    bp[j] = i;
                }
            }
            next[j] += e[j];
        }
        score = next;
        back.push(bp);
    }
    let mut best = (0..k).max_by(|a, b| score[*a].total_cmp(&score[*b])).unwrap_or(0);
    let mut path = vec![0; obs.len()];
    for t in (0..obs.len()).rev() {
        path[t] = best;
        best = back[t][best];
    }
    path.into_iter().map(|i| langs[i].clone()).collect()
}

/// Online (causal) version of [`decide`] for live captions: each call decides one utterance
/// given the previous decision (`LidObs::gap_ms` = silence since the previous utterance).
#[derive(Debug, Clone)]
pub struct OnlineLid {
    pub policy: LidPolicy,
    pub langs: Vec<LangTag>,
    prev: Option<usize>,
}

impl OnlineLid {
    pub fn new(langs: Vec<LangTag>, policy: LidPolicy) -> Self {
        OnlineLid {
            policy,
            langs,
            prev: None,
        }
    }

    /// Language of the last committed utterance.
    pub fn current(&self) -> Option<LangTag> {
        self.prev.map(|i| self.langs[i].clone())
    }

    /// Decide the language of an utterance without committing it (partials of a growing
    /// utterance).
    pub fn peek(&self, o: &LidObs) -> Option<LangTag> {
        self.choose(o).map(|i| self.langs[i].clone())
    }

    /// Decide and remember (the utterance is final).
    pub fn commit(&mut self, o: &LidObs) -> Option<LangTag> {
        let i = self.choose(o)?;
        self.prev = Some(i);
        Some(self.langs[i].clone())
    }

    fn choose(&self, o: &LidObs) -> Option<usize> {
        if self.langs.is_empty() {
            return None;
        }
        let e = self.policy.emission(&self.langs, o);
        let sw = self.policy.switch_cost(o.gap_ms);
        let s = |j: usize| e[j] - if self.prev.is_some_and(|p| p != j) { sw } else { 0.0 };
        (0..self.langs.len()).max_by(|a, b| s(*a).total_cmp(&s(*b)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(dur_ms: u64, gap_ms: u64, p_vi: f32) -> LidObs {
        LidObs {
            dur_ms,
            gap_ms,
            probs: vec![(LangTag::vi(), p_vi), (LangTag::en(), 1.0 - p_vi)],
        }
    }

    fn langs() -> Vec<LangTag> {
        vec![LangTag::vi(), LangTag::en()]
    }

    fn codes(v: &[LangTag]) -> Vec<&str> {
        v.iter().map(|l| l.0.as_str()).collect()
    }

    #[test]
    fn confident_long_utterances_keep_their_language() {
        let d = decide(
            &[o(4000, 0, 0.99), o(5000, 500, 0.01), o(3000, 500, 0.98)],
            &langs(),
            &LidPolicy::default(),
        );
        assert_eq!(codes(&d), ["vi", "en", "vi"]);
    }

    #[test]
    fn short_utterances_follow_context_and_prior() {
        // a 300 ms "ừ" that Whisper calls English, between Vietnamese turns
        let d = decide(
            &[o(3000, 0, 0.95), o(300, 300, 0.02), o(3000, 300, 0.9)],
            &langs(),
            &LidPolicy::default(),
        );
        assert_eq!(codes(&d), ["vi", "vi", "vi"]);
        // alone, it falls back to the primary language
        assert_eq!(
            codes(&decide(&[o(300, 0, 0.01)], &langs(), &LidPolicy::default())),
            ["vi"]
        );
    }

    #[test]
    fn weak_english_evidence_does_not_switch() {
        // 1.2 s utterance, Whisper says 70 % English: not enough against the prior
        let d = decide(&[o(1200, 0, 0.3)], &langs(), &LidPolicy::default());
        assert_eq!(codes(&d), ["vi"]);
        // a long confident English utterance does switch
        let d = decide(&[o(4000, 0, 0.03)], &langs(), &LidPolicy::default());
        assert_eq!(codes(&d), ["en"]);
    }

    #[test]
    fn online_matches_offline_on_a_simple_sequence() {
        let seq = [o(4000, 0, 0.99), o(300, 200, 0.01), o(5000, 800, 0.01)];
        let mut on = OnlineLid::new(langs(), LidPolicy::default());
        let got: Vec<LangTag> = seq.iter().map(|x| on.commit(x).unwrap()).collect();
        assert_eq!(on.current(), Some(LangTag::en()));
        assert_eq!(codes(&got), ["vi", "vi", "en"]);
        assert_eq!(codes(&decide(&seq, &langs(), &LidPolicy::default())), ["vi", "vi", "en"]);
    }
}
