//! Segment-level hallucination checks for Whisper-family decoders.
//!
//! Whisper invents text for silence, noise and speech in a language it was told not to expect
//! (the user's first real test: Vietnamese routed to English came out as "No, no, no, no, no.",
//! "A40.", "Hmm."). whisper.cpp's own thresholds (temperature fallback on entropy / logprob,
//! no-speech) catch some of it; [`reason`] adds checks per decoded segment, and
//! [`collapse_repeats`] cuts decoding loops inside otherwise good segments.

use crate::audio::{ms_to_samples, rms};
use crate::metrics::normalize;
use crate::streaming::HypWord;

/// Known silence/noise hallucinations of Whisper-family models (YouTube outros etc.).
pub const KNOWN_PHRASES: &[&str] = &[
    "hẹn gặp lại các bạn trong những video tiếp theo",
    "hãy subscribe cho kênh",
    "cảm ơn các bạn đã theo dõi",
    "ghiền mì gõ",
    "thanks for watching",
    "thank you for watching",
    "subtitles by the amara.org community",
    "please subscribe",
];

/// Short outputs Whisper produces for noise, breathing or speech in another language ("Hmm.",
/// "Thank you.", "You"): dropped unless decoded confidently over mostly voiced audio.
pub const SUSPECT_SHORT: &[&str] = &[
    "hmm",
    "hm",
    "mm",
    "um",
    "uh",
    "ah",
    "oh",
    "you",
    "thank you",
    "thanks",
    "bye",
    "bye bye",
    "so",
    "okay",
    "ok",
    "yeah",
    "the",
    "i",
    "a",
    "và",
    "ừ",
    "à",
    "ờ",
    "dạ",
    "cảm ơn",
];

/// What a decoder reported for one segment.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentScore<'a> {
    pub text: &'a str,
    /// Times relative to the decoded audio.
    pub t0_ms: u64,
    pub t1_ms: u64,
    /// Mean token probability (0 if unknown).
    pub mean_p: f32,
    /// Mean token log-probability (-inf if unknown).
    pub avg_logprob: f32,
    pub no_speech_p: f32,
}

/// Why a decoded segment should be dropped, if it looks like a hallucination. `audio` = the
/// decoded window.
pub fn reason(s: &SegmentScore, audio: &[f32]) -> Option<&'static str> {
    let n = normalize(s.text);
    let n = n.trim();
    if n.is_empty() {
        return Some("empty");
    }
    if KNOWN_PHRASES
        .iter()
        .any(|h| n.contains(normalize(h).trim()))
    {
        return Some("known phrase");
    }
    let lp = s.avg_logprob;
    // Whisper's rule: silence when no-speech is likely and the text is improbable.
    if s.no_speech_p > 0.6 && (s.mean_p < 0.5 || lp < -1.0) {
        return Some("no speech");
    }
    let words: Vec<&str> = n.split_whitespace().collect();
    // "No, no, no, no, no." / "A A A A": a few words repeated over and over.
    if words.len() >= 4 {
        let mut uniq = words.clone();
        uniq.sort_unstable();
        uniq.dedup();
        if uniq.len() * 3 <= words.len() {
            return Some("repetition");
        }
    }
    let voiced = voiced_fraction(audio, s.t0_ms, s.t1_ms);
    if voiced < 0.15 {
        return Some("mostly silence");
    }
    if words.len() <= 3 {
        let suspect = SUSPECT_SHORT.contains(&n);
        // A short, improbable decode ("A40.", "Hmm.") is Whisper guessing.
        if lp < -1.0 || (suspect && (lp < -0.5 || s.no_speech_p > 0.2 || voiced < 0.5)) {
            return Some("short and improbable");
        }
    }
    None
}

/// Share of 30 ms frames between `t0_ms` and `t1_ms` (relative to `audio`) whose energy is
/// above a voice threshold derived from the whole window (1 if the span is empty).
pub fn voiced_fraction(audio: &[f32], t0_ms: u64, t1_ms: u64) -> f32 {
    let all: Vec<f32> = audio.chunks(ms_to_samples(30).max(1)).map(rms).collect();
    if all.is_empty() {
        return 0.0;
    }
    let mut sorted = all.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let floor = sorted[sorted.len() / 10];
    let p90 = sorted[sorted.len() * 9 / 10];
    // Above the noise floor but below the loud frames (uniformly loud audio is all voiced);
    // never below an absolute minimum.
    let thr = (floor * 2.5).min(p90 * 0.5).max(3e-4);
    let a = (t0_ms as usize / 30).min(all.len());
    let b = (t1_ms.max(t0_ms + 1) as usize).div_ceil(30).min(all.len());
    if a >= b {
        return 1.0;
    }
    all[a..b].iter().filter(|e| **e >= thr).count() as f32 / (b - a) as f32
}

/// Cut a word loop ("we can we can we can we can ...") down to two repetitions.
pub fn collapse_repeats(words: &mut Vec<HypWord>) {
    let keys: Vec<String> = words.iter().map(|w| normalize(&w.text)).collect();
    let mut keep = vec![true; words.len()];
    for n in 1..=6usize {
        let mut i = 0;
        while i + 2 * n <= keys.len() {
            let mut reps = 1;
            while i + (reps + 1) * n <= keys.len()
                && keys[i + reps * n..i + (reps + 1) * n] == keys[i..i + n]
            {
                reps += 1;
            }
            if reps > 2 && keys[i..i + n].iter().all(|k| !k.trim().is_empty()) {
                for k in keep.iter_mut().take(i + reps * n).skip(i + 2 * n) {
                    *k = false;
                }
                i += reps * n;
            } else {
                i += 1;
            }
        }
    }
    let mut it = keep.iter();
    words.retain(|_| *it.next().unwrap_or(&true));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(ms: u64) -> Vec<f32> {
        (0..ms_to_samples(ms))
            .map(|i| 0.2 * ((i as f32) * 0.07).sin())
            .collect()
    }

    fn score(text: &str, t0: u64, t1: u64, lp: f32) -> SegmentScore<'_> {
        SegmentScore {
            text,
            t0_ms: t0,
            t1_ms: t1,
            mean_p: lp.exp(),
            avg_logprob: lp,
            no_speech_p: 0.1,
        }
    }

    #[test]
    fn real_test_hallucinations_are_dropped() {
        let a = voice(3000);
        assert_eq!(
            reason(&score("No, no, no, no, no.", 0, 3000, -0.3), &a),
            Some("repetition")
        );
        assert_eq!(
            reason(&score("A40.", 0, 1000, -1.6), &a),
            Some("short and improbable")
        );
        assert_eq!(
            reason(&score("Hmm.", 0, 1000, -0.7), &a),
            Some("short and improbable")
        );
        assert_eq!(
            reason(&score("Thanks for watching!", 0, 2000, -0.2), &a),
            Some("known phrase")
        );
    }

    #[test]
    fn real_speech_is_kept() {
        let a = voice(3000);
        assert_eq!(
            reason(
                &score("So what if I do with English right now?", 0, 3000, -0.3),
                &a
            ),
            None
        );
        assert_eq!(reason(&score("Okay.", 0, 600, -0.1), &a), None);
        assert_eq!(reason(&score("xin chào các bạn", 0, 3000, -0.4), &a), None);
    }

    #[test]
    fn text_over_silence_is_dropped() {
        let mut a = voice(1000);
        a.extend(vec![0.0; ms_to_samples(3000)]);
        assert_eq!(
            reason(&score("and then we went home", 1000, 4000, -0.3), &a),
            Some("mostly silence")
        );
        assert_eq!(voiced_fraction(&a, 0, 1000), 1.0);
    }

    #[test]
    fn loops_are_cut_to_two_repetitions() {
        let mk = |s: &str| -> Vec<HypWord> {
            s.split(' ')
                .enumerate()
                .map(|(i, w)| HypWord {
                    t0_ms: i as u64 * 100,
                    t1_ms: i as u64 * 100 + 90,
                    text: format!(" {w}"),
                    p: None,
                })
                .collect()
        };
        let mut w = mk("we can do it we can do it we can do it we can do it today");
        collapse_repeats(&mut w);
        let t: String = w.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(t.trim(), "we can do it we can do it today");
        let mut w = mk("no no that is fine");
        collapse_repeats(&mut w);
        assert_eq!(w.len(), 5);
    }
}
