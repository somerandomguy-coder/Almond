//! WER / CER with text normalization suitable for Vietnamese (and English / Chinese).
//!
//! Normalization (applied to both reference and hypothesis):
//! 1. Unicode NFC — Vietnamese diacritics can arrive precomposed (`ế`) or decomposed
//!    (`e` + U+0302 + U+0301); NFC makes them compare equal. Diacritics are **kept**: dropping
//!    them would hide real errors (`ma`/`má`/`mà`/`mả`/`mã`/`mạ` are different words).
//! 2. Lowercase (Unicode-aware).
//! 3. Punctuation and symbols -> space (keeps letters, digits, combining marks; apostrophes
//!    inside English words are kept: "don't").
//! 4. Collapse whitespace.
//!
//! Optional (off by default): map numbers spelled as digits — not done; references in VIVOS /
//! Common Voice are already spelled out, and Whisper is prompted not to use digits where
//! possible. Report this as a known source of error rather than hiding it.
//!
//! CER is computed over characters of the normalized text **without spaces**, which is the usual
//! convention for Chinese and is also reported for Vietnamese (syllable-level errors on
//! diacritics show up as 1-char substitutions).

use unicode_normalization::UnicodeNormalization;

/// Normalize text for scoring (see module docs).
pub fn normalize(text: &str) -> String {
    let nfc: String = text.nfc().collect();
    let lower = nfc.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let mut out = String::with_capacity(lower.len());
    for (i, &c) in chars.iter().enumerate() {
        let keep = c.is_alphanumeric()
            || is_combining_mark(c)
            || (c == '\''
                && i > 0
                && i + 1 < chars.len()
                && chars[i - 1].is_alphabetic()
                && chars[i + 1].is_alphabetic());
        if keep {
            out.push(c);
        } else {
            out.push(' ');
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_combining_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF)
}

/// True for CJK ideographs (scored per character even in "WER").
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
}

/// Tokens for WER: whitespace-separated words, with CJK ideographs split into single chars.
pub fn words(normalized: &str) -> Vec<String> {
    let mut out = vec![];
    for w in normalized.split_whitespace() {
        if w.chars().any(is_cjk) {
            let mut buf = String::new();
            for c in w.chars() {
                if is_cjk(c) {
                    if !buf.is_empty() {
                        out.push(std::mem::take(&mut buf));
                    }
                    out.push(c.to_string());
                } else {
                    buf.push(c);
                }
            }
            if !buf.is_empty() {
                out.push(buf);
            }
        } else {
            out.push(w.to_string());
        }
    }
    out
}

/// Edit-distance breakdown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EditCounts {
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
    /// Reference length.
    pub ref_len: usize,
}

impl EditCounts {
    pub fn errors(&self) -> usize {
        self.substitutions + self.deletions + self.insertions
    }
    /// Error rate; 0 for an empty reference with empty hypothesis, 1.0-per-insertion otherwise.
    pub fn rate(&self) -> f64 {
        if self.ref_len == 0 {
            return if self.insertions == 0 { 0.0 } else { 1.0 };
        }
        self.errors() as f64 / self.ref_len as f64
    }
    pub fn add(&mut self, o: EditCounts) {
        self.substitutions += o.substitutions;
        self.deletions += o.deletions;
        self.insertions += o.insertions;
        self.ref_len += o.ref_len;
    }
}

/// Levenshtein alignment with S/D/I breakdown.
pub fn align<T: PartialEq>(r: &[T], h: &[T]) -> EditCounts {
    let (n, m) = (r.len(), h.len());
    // dp[j] = (cost, s, d, i) for current row
    let mut prev: Vec<(usize, usize, usize, usize)> = (0..=m).map(|j| (j, 0, 0, j)).collect();
    for i in 1..=n {
        let mut cur = vec![(i, 0, i, 0); m + 1];
        for j in 1..=m {
            let sub = prev[j - 1];
            let cand_sub = if r[i - 1] == h[j - 1] {
                sub
            } else {
                (sub.0 + 1, sub.1 + 1, sub.2, sub.3)
            };
            let del = prev[j];
            let cand_del = (del.0 + 1, del.1, del.2 + 1, del.3);
            let ins = cur[j - 1];
            let cand_ins = (ins.0 + 1, ins.1, ins.2, ins.3 + 1);
            cur[j] = [cand_sub, cand_del, cand_ins]
                .into_iter()
                .min_by_key(|c| c.0)
                .unwrap();
        }
        prev = cur;
    }
    let (_, s, d, ins) = prev[m];
    EditCounts {
        substitutions: s,
        deletions: d,
        insertions: ins,
        ref_len: n,
    }
}

/// Word-level edit counts after normalization.
pub fn wer_counts(reference: &str, hypothesis: &str) -> EditCounts {
    align(
        &words(&normalize(reference)),
        &words(&normalize(hypothesis)),
    )
}

/// Character-level edit counts after normalization (spaces removed).
pub fn cer_counts(reference: &str, hypothesis: &str) -> EditCounts {
    let r: Vec<char> = normalize(reference).chars().filter(|c| *c != ' ').collect();
    let h: Vec<char> = normalize(hypothesis)
        .chars()
        .filter(|c| *c != ' ')
        .collect();
    align(&r, &h)
}

pub fn wer(reference: &str, hypothesis: &str) -> f64 {
    wer_counts(reference, hypothesis).rate()
}

pub fn cer(reference: &str, hypothesis: &str) -> f64 {
    cer_counts(reference, hypothesis).rate()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfc_makes_decomposed_vietnamese_equal() {
        let pre = "Tiếng Việt";
        let dec: String = pre.nfd().collect();
        assert_ne!(pre, dec);
        assert_eq!(normalize(pre), normalize(&dec));
        assert_eq!(wer(pre, &dec), 0.0);
    }

    #[test]
    fn case_and_punctuation_ignored() {
        assert_eq!(
            normalize("Xin chào, các BẠN!  Hôm nay..."),
            "xin chào các bạn hôm nay"
        );
        assert_eq!(wer("Hello, world.", "hello world"), 0.0);
        assert_eq!(normalize("don't stop"), "don't stop");
    }

    #[test]
    fn diacritics_matter() {
        // 'ma' vs 'má' is a substitution
        let c = wer_counts("con ma", "con má");
        assert_eq!(c.substitutions, 1);
        assert_eq!(c.rate(), 0.5);
    }

    #[test]
    fn sdi_breakdown() {
        let c = wer_counts("a b c d", "a x c d e");
        assert_eq!((c.substitutions, c.deletions, c.insertions), (1, 0, 1));
        let c = wer_counts("a b c d", "a d");
        assert_eq!((c.substitutions, c.deletions, c.insertions), (0, 2, 0));
        assert_eq!(c.ref_len, 4);
    }

    #[test]
    fn cer_ignores_spaces() {
        assert_eq!(cer("xin chào", "xinchào"), 0.0);
        assert!((cer("abcd", "abce") - 0.25).abs() < 1e-9);
    }

    #[test]
    fn chinese_is_split_per_char() {
        assert_eq!(words(&normalize("我们开会")), vec!["我", "们", "开", "会"]);
        assert!((wer("我们开会", "我们开车") - 0.25).abs() < 1e-9);
    }

    #[test]
    fn empty_cases() {
        assert_eq!(wer("", ""), 0.0);
        assert_eq!(wer("", "x"), 1.0);
        assert_eq!(wer("a", ""), 1.0);
    }
}
