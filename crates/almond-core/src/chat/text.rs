//! Text helpers for chat: diacritic folding, question keywords, answer language, and `[n]`
//! citation markers in the model's answer.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Lower-case, strip diacritics (`Ngân sách` -> `ngan sach`, `đồng` -> `dong`).
pub(crate) fn fold(s: &str) -> String {
    s.nfd()
        .filter(|c| !is_combining_mark(*c))
        .map(|c| match c {
            'đ' | 'Đ' | 'ð' | 'Ð' => 'd',
            c => c,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

const STOP_EN: &[&str] = &[
    "a",
    "about",
    "after",
    "all",
    "am",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "been",
    "before",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "for",
    "from",
    "get",
    "had",
    "has",
    "have",
    "how",
    "i",
    "if",
    "in",
    "is",
    "it",
    "its",
    "last",
    "me",
    "my",
    "next",
    "of",
    "on",
    "or",
    "our",
    "say",
    "said",
    "should",
    "so",
    "tell",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "this",
    "to",
    "today",
    "tomorrow",
    "us",
    "was",
    "we",
    "were",
    "what",
    "whats",
    "when",
    "where",
    "which",
    "who",
    "why",
    "will",
    "with",
    "would",
    "you",
    "your",
    "yesterday",
    "meeting",
    "meetings",
    "week",
    "please",
    "prep",
    "up",
    "coming",
    "decide",
    "decided",
    "discuss",
    "discussed",
    "talk",
    "talked",
];

/// Folded Vietnamese function words (too common, or too ambiguous once folded, to search for).
const STOP_VI: &[&str] = &[
    "a", "ai", "anh", "ay", "ba", "bao", "ban", "bi", "cac", "cai", "chi", "cho", "chua", "co",
    "con", "cua", "da", "dang", "day", "de", "di", "do", "duoc", "em", "gi", "hay", "hoac", "hom",
    "hop", "khi", "khong", "kia", "la", "lai", "luc", "ma", "minh", "mot", "nao", "nay", "nhe",
    "nha", "nhieu", "nhu", "nhung", "noi", "o", "oi", "qua", "ra", "roi", "sao", "se", "tai",
    "the", "thi", "toi", "tren", "trong", "tu", "va", "vay", "ve", "voi", "vua", "chung", "ngay",
    "mai", "tuan", "sap", "toi", "quyet", "dinh", "ban", "luan",
];

/// Search keywords of a question: folded words that are not stop words, longest first (at most
/// `max`).
pub(crate) fn keywords(question: &str, max: usize) -> Vec<String> {
    let folded = fold(question);
    let mut out: Vec<String> = vec![];
    for w in folded.split(|c: char| !c.is_alphanumeric()) {
        if w.is_empty() || STOP_EN.contains(&w) || STOP_VI.contains(&w) {
            continue;
        }
        // One letter is noise; numbers ("200", "4") are kept when they have 2+ digits.
        if w.chars().count() < 2 {
            continue;
        }
        if !out.iter().any(|o| o == w) {
            out.push(w.to_string());
        }
    }
    out.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
    out.truncate(max);
    out
}

/// "vi" or "en": the question's language when it is clear, else the UI language.
pub(crate) fn answer_lang(question: &str, ui_lang: Option<&str>) -> &'static str {
    let has_vi_marks = question.chars().any(|c| {
        matches!(c, 'đ' | 'Đ') || (!c.is_ascii() && c.is_alphabetic() && c.nfd().count() > 1)
    });
    if has_vi_marks {
        return "vi";
    }
    let words: Vec<String> = fold(question)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect();
    const EN_HINTS: &[&str] = &[
        "what", "when", "who", "how", "why", "did", "do", "does", "the", "is", "are", "my", "me",
        "about", "have", "should", "prep", "tell", "which", "where",
    ];
    const VI_HINTS: &[&str] = &[
        "gi", "nao", "khong", "minh", "toi", "hom", "nay", "cua", "nhung", "chua", "sao",
    ];
    let en = words
        .iter()
        .filter(|w| EN_HINTS.contains(&w.as_str()))
        .count();
    let vi = words
        .iter()
        .filter(|w| VI_HINTS.contains(&w.as_str()))
        .count();
    if en > vi {
        "en"
    } else if vi > en {
        "vi"
    } else if ui_lang.is_some_and(|l| l.starts_with("vi")) {
        "vi"
    } else {
        "en"
    }
}

/// Numbers of a citation marker's inside (`3`, `1, 2`, `2-4`, `Source 3`, `nguồn 1`), or `None`
/// when it isn't one (a Markdown link text, a checkbox, ...).
fn marker_numbers(inner: &str) -> Option<Vec<u32>> {
    let mut s = inner.trim();
    let lower = fold(s);
    for prefix in ["sources", "source", "src", "nguon", "ref"] {
        if lower.starts_with(prefix) {
            // `fold` keeps the char count of these ASCII-foldable prefixes.
            s = s[s
                .char_indices()
                .nth(prefix.len())
                .map_or(s.len(), |(i, _)| i)..]
                .trim_start_matches([':', ' ', '#']);
            break;
        }
    }
    if s.is_empty() {
        return None;
    }
    let mut out = vec![];
    for part in s.split([',', ';']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let range: Vec<&str> = part.split(['-', '–']).map(str::trim).collect();
        match range.as_slice() {
            [one] => out.push(one.trim_start_matches('#').parse().ok()?),
            [a, b] => {
                let (a, b): (u32, u32) = (a.parse().ok()?, b.parse().ok()?);
                if a == 0 || b < a || b - a > 20 {
                    return None;
                }
                out.extend(a..=b);
            }
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Normalise the answer's citation markers: `[1, 2]` / `[1-2]` / `[Source 1]` become `[1][2]`,
/// numbers outside `1..=n_sources` are dropped (a marker left empty is removed). Returns the
/// cleaned text and the cited numbers (sorted, unique).
pub(crate) fn resolve_markers(answer: &str, n_sources: usize) -> (String, Vec<u32>) {
    let mut out = String::with_capacity(answer.len());
    let mut cited: Vec<u32> = vec![];
    let mut rest = answer;
    while let Some(open) = rest.find(['[', '【']) {
        let open_ch = rest[open..].chars().next().unwrap();
        let close_ch = if open_ch == '[' { ']' } else { '】' };
        out.push_str(&rest[..open]);
        let after = &rest[open + open_ch.len_utf8()..];
        let parsed = after.find(close_ch).filter(|&c| c <= 40).and_then(|c| {
            // `[3](http://...)` is a Markdown link, not a citation.
            let next = after[c + close_ch.len_utf8()..].chars().next();
            if next == Some('(') {
                return None;
            }
            marker_numbers(&after[..c]).map(|nums| (c, nums))
        });
        match parsed {
            Some((c, nums)) => {
                let valid: Vec<u32> = nums
                    .into_iter()
                    .filter(|n| *n >= 1 && (*n as usize) <= n_sources)
                    .collect();
                if valid.is_empty() {
                    // Drop the marker and the space before it ("text [9]." -> "text.").
                    while out.ends_with(' ') {
                        out.pop();
                    }
                } else {
                    let mut seen = vec![];
                    for n in valid {
                        if !seen.contains(&n) {
                            seen.push(n);
                            out.push_str(&format!("[{n}]"));
                            cited.push(n);
                        }
                    }
                }
                rest = &after[c + close_ch.len_utf8()..];
            }
            None => {
                out.push(open_ch);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    cited.sort_unstable();
    cited.dedup();
    (out, cited)
}

/// Remove every citation marker (for earlier answers sent back as history: their numbers referred
/// to that turn's sources).
pub(crate) fn strip_markers(text: &str) -> String {
    resolve_markers(text, 0).0
}

/// At most `max_chars` characters, cut at a word boundary, with "…" when cut.
pub(crate) fn clip(s: &str, max_chars: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let cut: String = s.chars().take(max_chars).collect();
    let cut = match cut.rfind(char::is_whitespace) {
        Some(i) if i > cut.len() / 2 => &cut[..i],
        _ => &cut[..],
    };
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding() {
        assert_eq!(fold("Ngân sách ĐỒNG Họp"), "ngan sach dong hop");
    }

    #[test]
    fn question_keywords() {
        let k = keywords("What did we decide about the marketing budget?", 8);
        assert_eq!(k, vec!["marketing", "budget"]);
        let k = keywords("Mình đã quyết định gì về ngân sách Q4?", 8);
        assert_eq!(k, vec!["ngan", "sach", "q4"]);
        assert!(keywords("what is it?", 8).is_empty());
    }

    #[test]
    fn language() {
        assert_eq!(answer_lang("Hôm nay có gì?", Some("en")), "vi");
        assert_eq!(answer_lang("What do I have today?", Some("vi")), "en");
        assert_eq!(answer_lang("hom nay co gi", Some("en")), "vi");
        assert_eq!(answer_lang("Q4 budget", Some("vi")), "vi");
        assert_eq!(answer_lang("Q4 budget", None), "en");
    }

    #[test]
    fn markers() {
        let (t, c) = resolve_markers(
            "Budget is 200 [1, 3]. Hiring [2-3] [9]. Done [Source 2].",
            3,
        );
        assert_eq!(t, "Budget is 200 [1][3]. Hiring [2][3]. Done [2].");
        assert_eq!(c, vec![1, 2, 3]);
        let (t, c) = resolve_markers("See [the doc](http://x) and [ ] box [x] 【2】", 2);
        assert_eq!(t, "See [the doc](http://x) and [ ] box [x] [2]");
        assert_eq!(c, vec![2]);
        assert_eq!(strip_markers("A [1]. B [2][3]."), "A. B.");
        let (t, c) = resolve_markers("Theo nguồn [Nguồn 1] thì", 1);
        assert_eq!(t, "Theo nguồn [1] thì");
        assert_eq!(c, vec![1]);
    }

    #[test]
    fn clipping() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("one two three four", 12), "one two…");
    }
}
