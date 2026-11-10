//! Full-text search helpers: Vietnamese-friendly folding, query building, snippets.
//!
//! SQLite's `unicode61 remove_diacritics 2` tokenizer strips combining marks, so "họp", "hợp" and
//! "hop" all index as `hop`. It does *not* fold `đ` (a distinct letter, not d + mark), so we
//! pre-fold it ourselves. Folding is strictly char-for-char so highlight offsets in the folded
//! column map 1:1 back onto the original text.

const HL_START: char = '\u{1}';
const HL_END: char = '\u{2}';

/// Fold characters the tokenizer won't (char-for-char, length preserving).
pub fn fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'đ' => 'd',
            'Đ' => 'D',
            'ð' => 'd',
            'Ð' => 'D',
            _ => c,
        })
        .collect()
}

/// Build an FTS5 MATCH expression: every token must match (prefix match), quoted so user input
/// can never inject FTS syntax. `None` if the query has no searchable tokens.
pub fn match_expr(query: &str) -> Option<String> {
    let folded = fold(query);
    let toks: Vec<String> = folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"*", t.replace('"', "")))
        .collect();
    if toks.is_empty() {
        None
    } else {
        Some(toks.join(" "))
    }
}

pub fn highlight_markers() -> (String, String) {
    (HL_START.to_string(), HL_END.to_string())
}

/// Map an FTS `highlight()` of the folded column back onto the original text, wrap matches in
/// «», and cut a window of roughly `max_chars` around the first match.
pub fn snippet(original: &str, highlighted_folded: &str, max_chars: usize) -> String {
    let orig: Vec<char> = original.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(orig.len() + 8);
    let mut i = 0usize;
    let mut first_match: Option<usize> = None;
    for c in highlighted_folded.chars() {
        match c {
            HL_START => {
                first_match.get_or_insert(out.len());
                out.push('«');
            }
            HL_END => out.push('»'),
            _ => {
                out.push(orig.get(i).copied().unwrap_or(c));
                i += 1;
            }
        }
    }
    if out.len() <= max_chars {
        return out.into_iter().collect();
    }
    let center = first_match.unwrap_or(0);
    let mut start = center.saturating_sub(max_chars / 3);
    let mut end = (start + max_chars).min(out.len());
    if end == out.len() {
        start = end.saturating_sub(max_chars);
    }
    // snap to word boundaries, never splitting a «…» marker pair
    while start > 0
        && !out[start - 1].is_whitespace()
        && start > center.saturating_sub(max_chars / 2)
    {
        start -= 1;
    }
    while end < out.len() && !out[end].is_whitespace() && end < center + max_chars {
        end += 1;
    }
    let mut s: String = String::new();
    if start > 0 {
        s.push('…');
    }
    let body: String = out[start..end].iter().collect();
    s.push_str(body.trim());
    // balance markers if the window cut through a highlight
    let opens = s.matches('«').count();
    let closes = s.matches('»').count();
    if opens > closes {
        s.push('»');
    } else if closes > opens {
        s.insert(if start > 0 { '…'.len_utf8() } else { 0 }, '«');
    }
    if end < out.len() {
        s.push('…');
    }
    s
}

/// One FTS row to index.
pub struct Doc {
    pub text: String,
    pub kind: String,
    pub segment: Option<u32>,
}

/// Extract searchable text from an artifact body. Works on raw JSON (tolerant of schema bumps);
/// unknown kinds are not indexed.
pub fn artifact_docs(kind: &str, body: &serde_json::Value) -> Vec<Doc> {
    let mut docs = vec![];
    let mut push = |text: Option<&str>, segment: Option<u32>| {
        if let Some(t) = text
            && !t.trim().is_empty()
        {
            docs.push(Doc {
                text: t.to_string(),
                kind: kind.to_string(),
                segment,
            });
        }
    };
    let arr = |v: &serde_json::Value, k: &str| -> Vec<serde_json::Value> {
        v.get(k)
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default()
    };
    match kind {
        "transcript" => {
            for (i, s) in arr(body, "segments").iter().enumerate() {
                let idx = s
                    .get("idx")
                    .and_then(|x| x.as_u64())
                    .map(|x| x as u32)
                    .unwrap_or(i as u32);
                push(s.get("text").and_then(|x| x.as_str()), Some(idx));
            }
        }
        "summary" => {
            for sec in arr(body, "sections") {
                push(sec.get("heading").and_then(|x| x.as_str()), None);
                for b in arr(&sec, "bullets") {
                    push(b.get("text").and_then(|x| x.as_str()), None);
                }
            }
        }
        "enhanced_notes" => {
            for b in arr(body, "blocks") {
                push(b.get("text").and_then(|x| x.as_str()), None);
            }
        }
        "action_items" => {
            for b in arr(body, "items") {
                push(b.get("text").and_then(|x| x.as_str()), None);
            }
        }
        "title" => push(body.get("title").and_then(|x| x.as_str()), None),
        _ => {}
    }
    docs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_is_char_for_char() {
        let s = "Đồng ý, đi họp";
        assert_eq!(fold(s).chars().count(), s.chars().count());
        assert_eq!(fold(s), "Dồng ý, di họp");
    }

    #[test]
    fn match_expr_quotes_tokens() {
        assert_eq!(
            match_expr("hop  ngân-sách").unwrap(),
            "\"hop\"* \"ngân\"* \"sách\"*"
        );
        assert_eq!(match_expr("\" * ( :").as_deref(), None);
        // operators become quoted literals, never FTS syntax
        assert_eq!(match_expr("a OR b").unwrap(), "\"a\"* \"OR\"* \"b\"*");
        assert_eq!(match_expr("đồng").unwrap(), "\"dồng\"*");
    }

    #[test]
    fn snippet_maps_back_to_original() {
        let orig = "Ngân sách là 200 triệu đồng.";
        let hl = format!("Ngân sách là 200 triệu {HL_START}dồng{HL_END}.");
        assert_eq!(snippet(orig, &hl, 200), "Ngân sách là 200 triệu «đồng».");
    }

    #[test]
    fn snippet_windows_long_text() {
        let words: Vec<String> = (0..200).map(|i| format!("w{i}")).collect();
        let orig = words.join(" ");
        let hl = orig.replace("w150", &format!("{HL_START}w150{HL_END}"));
        let s = snippet(&orig, &hl, 60);
        assert!(s.contains("«w150»"), "{s}");
        assert!(s.starts_with('…') && s.ends_with('…'));
        assert!(s.chars().count() < 120);
    }
}
