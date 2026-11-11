//! Conversation statistics computed in code from a transcript, so recipes like "How did I
//! communicate?" give the model exact numbers instead of asking it to count.
//!
//! - **Talk time**: sum of segment durations per speaker (`t1 - t0`), and its share of all speech.
//! - **Turns**: runs of consecutive segments by the same speaker (in start-time order).
//! - **Longest turn**: the longest such run (first start to last end).
//! - **Pace**: words per minute of talk time (words = whitespace-separated tokens; for
//!   Vietnamese that is syllables).
//! - **Interruptions**: a segment of B (at least [`MIN_INTERRUPT_MS`] long, so a quick "ừ" /
//!   "yeah" doesn't count) that starts while A's segment still has more than [`OVERLAP_MS`] to
//!   go. Counted as "made" for B and "received" for A.
//! - **Filler words**: keyword counts (rough: "à" or "actually" can carry meaning).
//!
//! "me" is the mic channel: speaker id `me` (any case), or no speaker on the mic channel.

use almond_llm::TranscriptView;
use almond_llm::transcript::fmt_ms;
use almond_types::artifact::body::Segment;
use almond_types::meeting::AudioChannel;
use serde::Serialize;
use std::collections::BTreeMap;

/// Overlap (ms) that counts as talking over someone rather than a turn-taking gap.
pub const OVERLAP_MS: u32 = 500;
/// Shortest segment (ms) that counts as an interruption (shorter ones are backchannels).
pub const MIN_INTERRUPT_MS: u32 = 1_000;

/// Filler words/phrases (lower case, matched on whole words).
const FILLERS: &[&str] = &[
    // English
    "um",
    "uh",
    "erm",
    "uhm",
    "hmm",
    "you know",
    "i mean",
    "sort of",
    "kind of",
    "basically",
    "actually",
    "literally",
    // Vietnamese
    "ờ",
    "ừm",
    "ơ",
    "à",
    "ừ",
    "kiểu như",
    "kiểu là",
    "nói chung là",
    "đại loại là",
    "thì là",
    "các thứ",
];

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SpeakerStats {
    /// Display label as in the prompt transcript ("me", "S1", a name).
    pub speaker: String,
    pub is_me: bool,
    pub talk_ms: u64,
    /// Share of all talk time, 0..=1.
    pub share: f64,
    pub segments: u32,
    pub turns: u32,
    pub longest_turn_ms: u64,
    pub words: u32,
    pub words_per_min: f64,
    pub interruptions_made: u32,
    pub interruptions_received: u32,
    pub fillers_total: u32,
    /// Filler -> count, most frequent first.
    pub fillers: Vec<(String, u32)>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ConversationStats {
    /// First speech start to last speech end.
    pub span_ms: u64,
    pub talk_ms: u64,
    /// Speakers by talk time, longest first ("me" included wherever it falls).
    pub speakers: Vec<SpeakerStats>,
}

fn is_me(view: &TranscriptView, s: &Segment) -> bool {
    match &s.speaker {
        Some(id) => id.0.eq_ignore_ascii_case("me") || view.speaker_label(s) == "Me",
        None => s.channel == AudioChannel::Mic,
    }
}

fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Count fillers in `words`, longest phrases first so "kiểu như" isn't also counted as parts.
fn count_fillers(words: &[String], into: &mut BTreeMap<String, u32>) {
    let mut phrases: Vec<Vec<&str>> = FILLERS
        .iter()
        .map(|f| f.split(' ').collect::<Vec<_>>())
        .collect();
    phrases.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let mut i = 0;
    'outer: while i < words.len() {
        for p in &phrases {
            if i + p.len() <= words.len() && p.iter().zip(&words[i..]).all(|(a, b)| a == b) {
                *into.entry(p.join(" ")).or_default() += 1;
                i += p.len();
                continue 'outer;
            }
        }
        i += 1;
    }
}

pub fn compute(view: &TranscriptView) -> ConversationStats {
    let mut segs: Vec<&Segment> = view
        .segments()
        .iter()
        .filter(|s| !s.text.trim().is_empty() && s.t1_ms >= s.t0_ms)
        .collect();
    segs.sort_by_key(|s| (s.t0_ms, s.t1_ms));
    if segs.is_empty() {
        return ConversationStats::default();
    }
    let label = |s: &Segment| {
        if is_me(view, s) {
            "me".to_string()
        } else {
            view.speaker_label(s)
        }
    };

    #[derive(Default)]
    struct Acc {
        is_me: bool,
        talk_ms: u64,
        segments: u32,
        turns: u32,
        longest_turn_ms: u64,
        words: u32,
        made: u32,
        received: u32,
        fillers: BTreeMap<String, u32>,
    }
    let mut acc: BTreeMap<String, Acc> = BTreeMap::new();
    // Current turn: (speaker, start, end).
    let mut turn: Option<(String, u32, u32)> = None;
    let close_turn = |acc: &mut BTreeMap<String, Acc>, t: &(String, u32, u32)| {
        let a = acc.entry(t.0.clone()).or_default();
        a.turns += 1;
        a.longest_turn_ms = a.longest_turn_ms.max(u64::from(t.2.saturating_sub(t.1)));
    };
    for (i, s) in segs.iter().enumerate() {
        let who = label(s);
        let words = tokens(&s.text);
        {
            let a = acc.entry(who.clone()).or_default();
            a.is_me = who == "me";
            a.talk_ms += u64::from(s.t1_ms - s.t0_ms);
            a.segments += 1;
            a.words += words.len() as u32;
            count_fillers(&words, &mut a.fillers);
        }
        turn = match turn.take() {
            Some((w, t0, t1)) if w == who => Some((w, t0, t1.max(s.t1_ms))),
            Some(prev) => {
                close_turn(&mut acc, &prev);
                Some((who.clone(), s.t0_ms, s.t1_ms))
            }
            None => Some((who.clone(), s.t0_ms, s.t1_ms)),
        };
        // Interruption: someone else's earlier segment is still running well past our start.
        if s.t1_ms - s.t0_ms >= MIN_INTERRUPT_MS
            && let Some(victim) =
                segs[..i].iter().rev().take(64).find(|p| {
                    p.t1_ms > s.t0_ms + OVERLAP_MS && p.t0_ms < s.t0_ms && label(p) != who
                })
        {
            let v = label(victim);
            acc.entry(who.clone()).or_default().made += 1;
            acc.entry(v).or_default().received += 1;
        }
    }
    if let Some(t) = &turn {
        close_turn(&mut acc, t);
    }

    let talk_ms: u64 = acc.values().map(|a| a.talk_ms).sum();
    let span_ms = u64::from(
        segs.iter().map(|s| s.t1_ms).max().unwrap_or(0) - segs.first().map_or(0, |s| s.t0_ms),
    );
    let mut speakers: Vec<SpeakerStats> = acc
        .into_iter()
        .map(|(speaker, a)| {
            let mut fillers: Vec<(String, u32)> = a.fillers.into_iter().collect();
            fillers.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
            SpeakerStats {
                speaker,
                is_me: a.is_me,
                talk_ms: a.talk_ms,
                share: if talk_ms == 0 {
                    0.0
                } else {
                    a.talk_ms as f64 / talk_ms as f64
                },
                segments: a.segments,
                turns: a.turns,
                longest_turn_ms: a.longest_turn_ms,
                words: a.words,
                words_per_min: if a.talk_ms == 0 {
                    0.0
                } else {
                    a.words as f64 / (a.talk_ms as f64 / 60_000.0)
                },
                interruptions_made: a.made,
                interruptions_received: a.received,
                fillers_total: fillers.iter().map(|f| f.1).sum(),
                fillers,
            }
        })
        .collect();
    speakers.sort_by(|a, b| b.talk_ms.cmp(&a.talk_ms).then(a.speaker.cmp(&b.speaker)));
    ConversationStats {
        span_ms,
        talk_ms,
        speakers,
    }
}

fn mmss(ms: u64) -> String {
    fmt_ms(ms.min(u64::from(u32::MAX)) as u32)
}

/// The statistics as a prompt block (Markdown table + definitions).
pub fn render(st: &ConversationStats) -> String {
    if st.speakers.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "Meeting statistics (computed exactly from the transcript; use these numbers as they are):\n",
    );
    s.push_str(&format!(
        "- Conversation length: {} ({} of speech), {} speaker(s).\n",
        mmss(st.span_ms),
        mmss(st.talk_ms),
        st.speakers.len()
    ));
    if !st.speakers.iter().any(|x| x.is_me) {
        s.push_str("- There are no lines from \"me\" (the user's microphone).\n");
    }
    s.push_str(
        "\n| Speaker | Talk time | Share | Turns | Longest turn | Words/min | Interruptions made | Interrupted by others | Filler words |\n|---|---|---|---|---|---|---|---|---|\n",
    );
    for x in &st.speakers {
        let fillers = if x.fillers.is_empty() {
            "0".to_string()
        } else {
            let top: Vec<String> = x
                .fillers
                .iter()
                .take(4)
                .map(|(w, n)| format!("\"{w}\" ×{n}"))
                .collect();
            format!("{} ({})", x.fillers_total, top.join(", "))
        };
        s.push_str(&format!(
            "| {}{} | {} | {:.0}% | {} | {} | {:.0} | {} | {} | {} |\n",
            x.speaker,
            if x.is_me { " (the user)" } else { "" },
            mmss(x.talk_ms),
            x.share * 100.0,
            x.turns,
            mmss(x.longest_turn_ms),
            x.words_per_min,
            x.interruptions_made,
            x.interruptions_received,
            fillers
        ));
    }
    s.push_str(&format!(
        "\nDefinitions: a turn is a run of consecutive lines by one speaker; words are space-separated \
         (Vietnamese: syllables); an interruption is starting to speak (for at least {} s) while \
         another speaker still had more than {} ms to go; filler words are a rough keyword count.\n",
        MIN_INTERRUPT_MS / 1000,
        OVERLAP_MS
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::Id;
    use almond_types::artifact::body::Transcript;
    use almond_types::ids::SpeakerId;

    fn seg(idx: u32, t0: u32, t1: u32, spk: &str, text: &str) -> Segment {
        Segment {
            idx,
            t0_ms: t0,
            t1_ms: t1,
            channel: if spk == "me" {
                AudioChannel::Mic
            } else {
                AudioChannel::System
            },
            speaker: Some(SpeakerId(spk.into())),
            lang: None,
            text: text.into(),
            confidence: None,
            words: None,
        }
    }

    #[test]
    fn talk_time_turns_interruptions_fillers() {
        let view = TranscriptView::new(
            Id::new(),
            Transcript {
                segments: vec![
                    seg(0, 0, 10_000, "me", "UM SO WE NEED THE BUDGET YOU KNOW"),
                    seg(1, 10_500, 14_000, "me", "ACTUALLY TWO HUNDRED"),
                    // S1 starts at 12 s while "me" runs until 14 s: interruption (2 s overlap).
                    seg(
                        2,
                        12_000,
                        20_000,
                        "S1",
                        "Ờ KIỂU NHƯ CHÚNG TA CẦN THÊM THỜI GIAN",
                    ),
                    // Short backchannel during S1: not an interruption.
                    seg(3, 15_000, 15_400, "me", "ừ"),
                    seg(4, 21_000, 30_000, "S2", "OK"),
                    seg(5, 31_000, 31_000, "S1", "   "),
                ],
                speakers: vec![],
            },
        );
        let st = compute(&view);
        assert_eq!(st.talk_ms, 10_000 + 3_500 + 8_000 + 400 + 9_000);
        assert_eq!(st.span_ms, 30_000);
        let get = |n: &str| st.speakers.iter().find(|s| s.speaker == n).unwrap();
        let me = get("me");
        assert!(me.is_me);
        assert_eq!(me.talk_ms, 13_900);
        assert_eq!(me.segments, 3);
        // me(0,1) | S1(2) | me(3) | S2(4)
        assert_eq!(me.turns, 2);
        assert_eq!(me.longest_turn_ms, 14_000);
        assert_eq!(me.interruptions_received, 1);
        assert_eq!(me.interruptions_made, 0);
        assert_eq!(me.fillers_total, 4, "{:?}", me.fillers); // um, you know, actually, ừ
        let s1 = get("S1");
        assert_eq!(s1.interruptions_made, 1);
        assert_eq!(s1.fillers_total, 2, "{:?}", s1.fillers); // ờ, kiểu như (not "kiểu")
        assert_eq!(s1.words, 9);
        assert_eq!(st.speakers[0].speaker, "me", "sorted by talk time");
        assert!((st.speakers.iter().map(|s| s.share).sum::<f64>() - 1.0).abs() < 1e-9);
        let r = render(&st);
        assert!(
            r.contains("| me (the user) | 00:13 | 45% | 2 | 00:14 |"),
            "{r}"
        );
        assert!(r.contains("\"ờ\" ×1"), "{r}");
    }

    #[test]
    fn empty_and_no_me() {
        let v = TranscriptView::new(Id::new(), Transcript::default());
        assert_eq!(render(&compute(&v)), "");
        let v = TranscriptView::new(
            Id::new(),
            Transcript {
                segments: vec![seg(0, 0, 1000, "S1", "hello")],
                speakers: vec![],
            },
        );
        assert!(render(&compute(&v)).contains("no lines from \"me\""));
    }
}
