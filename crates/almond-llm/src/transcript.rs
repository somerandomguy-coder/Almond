//! Rendering transcripts for prompts (`[12] 03:21 Lan: ...`), chunking under a token budget, and
//! picking the output language.

use crate::tokens::estimate_tokens;
use almond_types::artifact::body::{Segment, Transcript};
use almond_types::meeting::AudioChannel;
use almond_types::{Artifact, Id, LangTag, Result};
use std::collections::HashMap;

/// A transcript artifact prepared for prompting: segments indexed by `idx`.
#[derive(Debug, Clone)]
pub struct TranscriptView {
    pub artifact: Id,
    pub body: Transcript,
    names: HashMap<String, String>,
    by_idx: HashMap<u32, usize>,
}

impl TranscriptView {
    pub fn from_artifact(a: &Artifact) -> Result<Self> {
        let body: Transcript = a.decode()?;
        Ok(Self::new(a.id.clone(), body))
    }

    pub fn new(artifact: Id, body: Transcript) -> Self {
        let names = body
            .speakers
            .iter()
            .filter_map(|s| s.name.clone().map(|n| (s.id.0.clone(), n)))
            .collect();
        let by_idx = body
            .segments
            .iter()
            .enumerate()
            .map(|(i, s)| (s.idx, i))
            .collect();
        TranscriptView {
            artifact,
            body,
            names,
            by_idx,
        }
    }

    pub fn segments(&self) -> &[Segment] {
        &self.body.segments
    }

    pub fn segment(&self, idx: u32) -> Option<&Segment> {
        self.by_idx.get(&idx).map(|i| &self.body.segments[*i])
    }

    pub fn speaker_label(&self, s: &Segment) -> String {
        if let Some(id) = &s.speaker {
            return self
                .names
                .get(&id.0)
                .cloned()
                .unwrap_or_else(|| id.0.clone());
        }
        match s.channel {
            AudioChannel::Mic => "Me".into(),
            AudioChannel::System => "Remote".into(),
            AudioChannel::Mixed => "Speaker".into(),
        }
    }

    /// One prompt line per segment: `[idx] mm:ss Speaker: text`.
    pub fn render_line(&self, s: &Segment) -> String {
        format!(
            "[{}] {} {}: {}",
            s.idx,
            fmt_ms(s.t0_ms),
            self.speaker_label(s),
            s.text.trim()
        )
    }

    pub fn render(&self, segs: &[Segment]) -> String {
        let mut out = String::new();
        for s in segs {
            if s.text.trim().is_empty() {
                continue;
            }
            out.push_str(&self.render_line(s));
            out.push('\n');
        }
        out
    }

    pub fn render_all(&self) -> String {
        self.render(&self.body.segments)
    }

    /// Split into consecutive chunks whose rendering fits `budget_tokens` (a single oversized
    /// segment becomes its own chunk). Indices are preserved, so citations stay global.
    pub fn chunks(&self, budget_tokens: usize) -> Vec<Vec<Segment>> {
        let mut out = vec![];
        let mut cur: Vec<Segment> = vec![];
        let mut cur_tok = 0usize;
        for s in &self.body.segments {
            if s.text.trim().is_empty() {
                continue;
            }
            let t = estimate_tokens(&self.render_line(s)) + 1;
            if !cur.is_empty() && cur_tok + t > budget_tokens {
                out.push(std::mem::take(&mut cur));
                cur_tok = 0;
            }
            cur.push(s.clone());
            cur_tok += t;
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }

    /// Language with the most transcript characters (segment `lang`), if any segment has one.
    pub fn dominant_lang(&self) -> Option<LangTag> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for s in &self.body.segments {
            if let Some(l) = &s.lang {
                *counts.entry(l.primary()).or_default() += s.text.chars().count();
            }
        }
        counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
            .map(|(l, _)| LangTag(l))
    }
}

/// `mm:ss` or `h:mm:ss`.
pub fn fmt_ms(ms: u32) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Output language: explicit request > dominant transcript language > meeting's first language
/// > English.
pub fn resolve_output_lang(
    requested: Option<&str>,
    transcript: &TranscriptView,
    meeting_langs: &[LangTag],
) -> LangTag {
    if let Some(r) = requested.filter(|r| !r.trim().is_empty() && *r != "auto") {
        return LangTag(r.trim().to_string());
    }
    transcript
        .dominant_lang()
        .or_else(|| meeting_langs.first().cloned())
        .unwrap_or_else(LangTag::en)
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::ids::DeviceId;
    use almond_types::mock::fixtures;

    #[test]
    fn render_and_chunk() {
        let m = fixtures::meeting(0);
        let t = fixtures::transcript(&m.id, &DeviceId("d".into()));
        let v = TranscriptView::from_artifact(&t).unwrap();
        let r = v.render_all();
        assert!(r.starts_with("[0] 00:00 me: Chào mọi người"));
        assert!(r.contains("[2] 00:10 S2: We should hire"));
        assert_eq!(v.chunks(10_000).len(), 1);
        let small = v.chunks(20);
        assert!(small.len() >= 2);
        let flat: Vec<u32> = small.iter().flatten().map(|s| s.idx).collect();
        assert_eq!(flat, vec![0, 1, 2, 3]);
        assert_eq!(v.dominant_lang(), Some(LangTag::vi()));
        assert_eq!(resolve_output_lang(Some("en"), &v, &[]).0, "en");
        assert_eq!(resolve_output_lang(Some("auto"), &v, &[]).0, "vi");
    }

    #[test]
    fn time_format() {
        assert_eq!(fmt_ms(61_000), "01:01");
        assert_eq!(fmt_ms(3_723_000), "1:02:03");
    }
}
