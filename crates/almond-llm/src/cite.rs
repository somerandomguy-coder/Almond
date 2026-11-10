//! Citation handling: the model answers with transcript segment indices (`"cites": [3, 7]`); we
//! turn them into [`SpanRef`]s and **validate** them against the transcript. Indices that don't
//! exist are dropped and counted. Inline markers the model sometimes writes into the text
//! (`"... 200 triệu [1]"`, `"[3-5]"`) are stripped from the text and merged into the cites.

use crate::transcript::TranscriptView;
use almond_types::SpanRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Counters over everything resolved by one [`CiteResolver`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiteStats {
    /// Raw citation indices returned by the model (after de-duplication per item).
    pub cites_total: u32,
    /// ... of which point at an existing transcript segment.
    pub cites_valid: u32,
    /// ... of which were dropped (non-existent segment / unparsable).
    pub cites_invalid: u32,
    /// Cited items (bullets, blocks, action items).
    pub items_total: u32,
    /// Items left without any valid citation.
    pub items_uncited: u32,
}

impl CiteStats {
    /// Fraction of raw cites that were valid (1.0 when there were none).
    pub fn validity(&self) -> f64 {
        if self.cites_total == 0 {
            1.0
        } else {
            self.cites_valid as f64 / self.cites_total as f64
        }
    }
    /// Fraction of items with at least one valid cite (1.0 when there were none).
    pub fn grounded(&self) -> f64 {
        if self.items_total == 0 {
            1.0
        } else {
            (self.items_total - self.items_uncited) as f64 / self.items_total as f64
        }
    }
    pub fn merge(&mut self, o: &CiteStats) {
        self.cites_total += o.cites_total;
        self.cites_valid += o.cites_valid;
        self.cites_invalid += o.cites_invalid;
        self.items_total += o.items_total;
        self.items_uncited += o.items_uncited;
    }
}

/// Resolves raw indices against one transcript and accumulates [`CiteStats`].
pub struct CiteResolver<'a> {
    view: &'a TranscriptView,
    pub stats: CiteStats,
    /// Raw values that were dropped, for logging.
    pub dropped: Vec<String>,
}

impl<'a> CiteResolver<'a> {
    pub fn new(view: &'a TranscriptView) -> Self {
        CiteResolver {
            view,
            stats: CiteStats::default(),
            dropped: vec![],
        }
    }

    /// Resolve the cites of one item. `text` is cleaned of inline `[n]` markers in place.
    pub fn resolve(&mut self, raw: &[Value], text: &mut String) -> Vec<SpanRef> {
        let mut idx: Vec<i64> = vec![];
        let mut bad = 0u32;
        for v in raw {
            match parse_index(v) {
                Some(list) => idx.extend(list),
                None => {
                    bad += 1;
                    self.dropped.push(v.to_string());
                }
            }
        }
        let (clean, inline) = strip_inline_markers(text);
        *text = clean;
        idx.extend(inline);
        let mut seen = std::collections::BTreeSet::new();
        idx.retain(|i| seen.insert(*i));

        self.stats.items_total += 1;
        self.stats.cites_total += idx.len() as u32 + bad;
        self.stats.cites_invalid += bad;
        let mut out = vec![];
        for i in idx {
            match u32::try_from(i).ok().and_then(|u| self.view.segment(u)) {
                Some(seg) => {
                    self.stats.cites_valid += 1;
                    out.push(SpanRef {
                        artifact: self.view.artifact.clone(),
                        segment: seg.idx,
                        chars: None,
                        time_ms: Some((seg.t0_ms, seg.t1_ms)),
                    });
                }
                None => {
                    self.stats.cites_invalid += 1;
                    self.dropped.push(i.to_string());
                }
            }
        }
        out.sort_by_key(|s| s.segment);
        if out.is_empty() {
            self.stats.items_uncited += 1;
        }
        out
    }
}

/// Accepts `12`, `"12"`, `"[12]"`, `"12-14"`, `12.0`.
fn parse_index(v: &Value) -> Option<Vec<i64>> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
            .map(|i| vec![i]),
        Value::String(s) => parse_index_str(s),
        _ => None,
    }
}

fn parse_index_str(s: &str) -> Option<Vec<i64>> {
    let s = s
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    if let Some((a, b)) = s.split_once('-') {
        let (a, b) = (a.trim().parse::<i64>().ok()?, b.trim().parse::<i64>().ok()?);
        if a <= b && b - a <= 50 {
            return Some((a..=b).collect());
        }
        return None;
    }
    s.parse::<i64>().ok().map(|i| vec![i])
}

/// Remove `[3]`, `[3, 4]`, `[3-5]` markers from `text`; returns (clean text, indices).
pub fn strip_inline_markers(text: &str) -> (String, Vec<i64>) {
    let mut out = String::with_capacity(text.len());
    let mut idx = vec![];
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(close_rel) = rest[open..].find(']') else {
            break;
        };
        let inner = &rest[open + 1..open + close_rel];
        let parts: Option<Vec<Vec<i64>>> = if inner.trim().is_empty() {
            None
        } else {
            inner.split([',', ';']).map(parse_index_str).collect()
        };
        out.push_str(&rest[..open]);
        match parts {
            Some(p) => idx.extend(p.into_iter().flatten()),
            None => out.push_str(&rest[open..=open + close_rel]),
        }
        rest = &rest[open + close_rel + 1..];
    }
    out.push_str(rest);
    // Tidy whitespace left behind ("200 triệu [1]." -> "200 triệu.").
    let mut clean = out.split_whitespace().collect::<Vec<_>>().join(" ");
    for p in [" .", " ,", " ;", " :", " )"] {
        clean = clean.replace(p, &p[1..]);
    }
    (clean, idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::ids::DeviceId;
    use almond_types::mock::fixtures;
    use serde_json::json;

    fn view() -> TranscriptView {
        let m = fixtures::meeting(0);
        TranscriptView::from_artifact(&fixtures::transcript(&m.id, &DeviceId("d".into()))).unwrap()
    }

    #[test]
    fn valid_invalid_and_inline() {
        let v = view();
        let mut r = CiteResolver::new(&v);
        let mut text = "Ngân sách 200 triệu [1].".to_string();
        let cites = r.resolve(&[json!(1), json!(9), json!("2"), json!("x")], &mut text);
        assert_eq!(text, "Ngân sách 200 triệu.");
        assert_eq!(
            cites.iter().map(|c| c.segment).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(cites[0].time_ms, Some((5000, 9500)));
        assert_eq!(cites[0].artifact, v.artifact);
        assert_eq!(r.stats.cites_total, 4); // 1, 9, 2 (+ dup inline 1 removed) + "x"
        assert_eq!(r.stats.cites_valid, 2);
        assert_eq!(r.stats.cites_invalid, 2);
        let mut t2 = "No support".to_string();
        assert!(r.resolve(&[json!(-1)], &mut t2).is_empty());
        assert_eq!(r.stats.items_uncited, 1);
        assert_eq!(r.stats.items_total, 2);
    }

    #[test]
    fn markers() {
        assert_eq!(
            strip_inline_markers("a [1, 2] b [3-4] c [note]"),
            ("a b c [note]".to_string(), vec![1, 2, 3, 4])
        );
    }
}
