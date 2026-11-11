//! Citations in a recipe's Markdown answer.
//!
//! The model cites transcript lines by their index (`[12]`, `[12, 15]`, `[12-14]`, `[12][15]`).
//! [`renumber`] validates every index against the transcript (like the summary's cite handling
//! in `almond_llm::cite`), drops the ones that don't exist, and rewrites the valid ones as
//! `[n]` markers numbered in order of first appearance, where `n` refers to
//! `RecipeOutput::citations[n - 1]`. Markdown links (`[text](url)`), task boxes (`[ ]`, `[x]`)
//! and code are left alone.

use almond_llm::TranscriptView;
use almond_llm::cite::CiteStats;
use almond_types::SpanRef;
use std::collections::HashMap;

/// Ranges longer than this cite only their first and last line.
const MAX_RANGE_EXPAND: i64 = 5;

/// Parse the inside of `[...]` as citation indices: `12`, `12, 15`, `12-14`, `12–14; 20`.
fn parse_group(inner: &str) -> Option<Vec<i64>> {
    let inner = inner.trim();
    if inner.is_empty() || !inner.chars().next()?.is_ascii_digit() {
        return None;
    }
    let mut out = vec![];
    for part in inner.split([',', ';']) {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        let range = part
            .split_once('-')
            .or_else(|| part.split_once('–'))
            .or_else(|| part.split_once('—'));
        match range {
            Some((a, b)) => {
                let a: i64 = a.trim().parse().ok()?;
                let b: i64 = b.trim().parse().ok()?;
                if b < a || b - a > 200 {
                    return None;
                }
                if b - a < MAX_RANGE_EXPAND {
                    out.extend(a..=b);
                } else {
                    out.extend([a, b]);
                }
            }
            None => out.push(part.parse().ok()?),
        }
    }
    Some(out)
}

/// Strip things models wrap answers in: a reasoning block, a ```markdown fence.
pub fn clean_answer(s: &str) -> String {
    let s = match s.rfind("</think>") {
        Some(i) => &s[i + "</think>".len()..],
        None => s,
    };
    let mut s = s.trim();
    if s.starts_with("```") {
        let first_nl = s.find('\n').unwrap_or(s.len());
        let body = &s[first_nl..];
        s = body.trim_end().strip_suffix("```").unwrap_or(body).trim();
    }
    s.to_string()
}

/// Rewrite transcript-index citations as numbered markers; returns the new Markdown, the
/// citation list and validation stats (each citation group counts as one "item").
pub fn renumber(md: &str, view: &TranscriptView) -> (String, Vec<SpanRef>, CiteStats) {
    let mut out = String::with_capacity(md.len());
    let mut cites: Vec<SpanRef> = vec![];
    let mut by_seg: HashMap<u32, usize> = HashMap::new();
    let mut stats = CiteStats::default();
    let mut in_code = false; // inside a ``` block
    for (li, line) in md.split('\n').enumerate() {
        if li > 0 {
            out.push('\n');
        }
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            out.push_str(line);
            continue;
        }
        if in_code {
            out.push_str(line);
            continue;
        }
        let mut rest = line;
        let mut in_tick = false; // inside `inline code`
        while !rest.is_empty() {
            let next = rest.find(['[', '`']);
            let Some(pos) = next else {
                out.push_str(rest);
                break;
            };
            out.push_str(&rest[..pos]);
            let c = rest.as_bytes()[pos];
            if c == b'`' {
                in_tick = !in_tick;
                out.push('`');
                rest = &rest[pos + 1..];
                continue;
            }
            let after = &rest[pos + 1..];
            let Some(close) = after.find(']') else {
                out.push_str(&rest[pos..]);
                break;
            };
            let inner = &after[..close];
            let tail = &after[close + 1..];
            let group = if in_tick || tail.starts_with('(') {
                None
            } else {
                parse_group(inner)
            };
            let Some(idx) = group else {
                out.push('[');
                rest = after;
                continue;
            };
            stats.items_total += 1;
            let mut markers = String::new();
            let mut any = false;
            let mut seen = std::collections::HashSet::new();
            for i in idx {
                if !seen.insert(i) {
                    continue;
                }
                stats.cites_total += 1;
                let seg = u32::try_from(i).ok().and_then(|u| view.segment(u));
                let Some(seg) = seg else {
                    stats.cites_invalid += 1;
                    continue;
                };
                stats.cites_valid += 1;
                any = true;
                let n = *by_seg.entry(seg.idx).or_insert_with(|| {
                    cites.push(SpanRef {
                        artifact: view.artifact.clone(),
                        segment: seg.idx,
                        chars: None,
                        time_ms: Some((seg.t0_ms, seg.t1_ms)),
                    });
                    cites.len()
                });
                markers.push_str(&format!("[{n}]"));
            }
            if !any {
                stats.items_uncited += 1;
                // Drop the marker and the space before it ("200 triệu [99]." -> "200 triệu.").
                while out.ends_with(' ') {
                    out.pop();
                }
                if tail.starts_with(|c: char| c.is_alphanumeric()) && !out.is_empty() {
                    out.push(' ');
                }
            } else {
                out.push_str(&markers);
            }
            rest = tail;
        }
    }
    (out, cites, stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::ids::DeviceId;
    use almond_types::mock::fixtures;

    fn view() -> TranscriptView {
        let m = fixtures::meeting(0);
        TranscriptView::from_artifact(&fixtures::transcript(&m.id, &DeviceId("d".into()))).unwrap()
    }

    #[test]
    fn renumbers_validates_and_skips_links() {
        let v = view(); // segments 0..=3
        let md = "## Budget\n- Marketing is 200 triệu [1].\n- Hiring [2, 3] and [3][99].\n\
                  - Bogus [42].\n- [x] done, see [docs](http://x) and `a[0]`\n\
                  ```\nlet a = b[2];\n```\n| Decision | Source |\n|---|---|\n| Hire | [2-3] |";
        let (out, cites, st) = renumber(md, &v);
        assert_eq!(
            out,
            "## Budget\n- Marketing is 200 triệu [1].\n- Hiring [2][3] and [3].\n\
             - Bogus.\n- [x] done, see [docs](http://x) and `a[0]`\n\
             ```\nlet a = b[2];\n```\n| Decision | Source |\n|---|---|\n| Hire | [2][3] |"
        );
        assert_eq!(
            cites.iter().map(|c| c.segment).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(cites[0].artifact, v.artifact);
        assert_eq!(cites[0].time_ms, Some((5000, 9500)));
        assert_eq!(st.items_total, 6);
        assert_eq!(st.items_uncited, 2);
        assert_eq!(st.cites_invalid, 2);
        assert_eq!(st.cites_valid, 6);
    }

    #[test]
    fn groups_and_cleanup() {
        assert_eq!(parse_group("1, 2;3"), Some(vec![1, 2, 3]));
        assert_eq!(parse_group("4-6"), Some(vec![4, 5, 6]));
        assert_eq!(parse_group("4–40"), Some(vec![4, 40]));
        assert_eq!(parse_group(" "), None);
        assert_eq!(parse_group("x"), None);
        assert_eq!(parse_group("1, x"), None);
        assert_eq!(
            clean_answer("<think>hm</think>\n```markdown\n# Hi\n```"),
            "# Hi"
        );
        assert_eq!(clean_answer("  plain  "), "plain");
    }
}
