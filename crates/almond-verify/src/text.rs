//! Text normalization shared by the symbolic verifiers: diacritic folding (Vietnamese aware) and a
//! small tokenizer that keeps character offsets so verdicts can point at exact spans.

use unicode_normalization::UnicodeNormalization;

/// Lowercase and strip diacritics: "Nguyễn Văn Đức" -> "nguyen van duc".
pub fn fold(s: &str) -> String {
    s.nfd()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
        .map(|c| match c {
            'đ' | 'Đ' => 'd',
            c => c,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    Word,
    Num,
    Sym,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Original surface form.
    pub raw: String,
    /// Lowercased surface (diacritics kept).
    pub lower: String,
    /// Lowercased + diacritics stripped.
    pub folded: String,
    pub kind: TokKind,
    /// Char offset (start, end) in the source text.
    pub span: (usize, usize),
    /// True when there was no whitespace between the previous token and this one.
    pub attached: bool,
}

impl Token {
    pub fn is_capitalized(&self) -> bool {
        self.kind == TokKind::Word && self.raw.chars().next().is_some_and(char::is_uppercase)
    }
}

/// Split into words (letters incl. Vietnamese), numbers (digits with inner `.`/`,`) and symbols.
pub fn tokenize(s: &str) -> Vec<Token> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut gap = true;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            gap = true;
            i += 1;
            continue;
        }
        let start = i;
        let kind = if c.is_alphabetic() {
            while i < chars.len() && (chars[i].is_alphabetic() || is_mark(chars[i])) {
                i += 1;
            }
            TokKind::Word
        } else if c.is_ascii_digit() {
            while i < chars.len() {
                let inner_sep = (chars[i] == '.' || chars[i] == ',')
                    && i + 1 < chars.len()
                    && chars[i + 1].is_ascii_digit();
                if chars[i].is_ascii_digit() || inner_sep {
                    i += 1;
                } else {
                    break;
                }
            }
            TokKind::Num
        } else {
            i += 1;
            TokKind::Sym
        };
        let raw: String = chars[start..i].iter().collect();
        let lower = raw.to_lowercase();
        out.push(Token {
            folded: fold(&raw),
            lower,
            raw,
            kind,
            span: (start, i),
            attached: !gap,
        });
        gap = false;
    }
    out
}

fn is_mark(c: char) -> bool {
    ('\u{0300}'..='\u{036f}').contains(&c)
}

/// Folded words of `s`, for bag-of-words style lookups.
pub fn folded_words(s: &str) -> Vec<String> {
    tokenize(s)
        .into_iter()
        .filter(|t| t.kind == TokKind::Word)
        .map(|t| t.folded)
        .collect()
}

/// Does the folded word sequence `needle` occur contiguously in `hay`?
pub fn contains_seq(hay: &[String], needle: &[String]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Char-index substring (end exclusive), clamped.
pub fn char_slice(s: &str, start: usize, end: usize) -> String {
    s.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_vietnamese() {
        assert_eq!(fold("Nguyễn Văn Đức"), "nguyen van duc");
        assert_eq!(fold("thứ Sáu"), "thu sau");
        assert_eq!(fold("Hai trăm triệu"), "hai tram trieu");
    }

    #[test]
    fn tokenizes_numbers_and_attachment() {
        let t = tokenize("200tr, 1,5 tỷ và Q4 15%");
        let raws: Vec<_> = t.iter().map(|t| t.raw.as_str()).collect();
        assert_eq!(
            raws,
            ["200", "tr", ",", "1,5", "tỷ", "và", "Q", "4", "15", "%"]
        );
        assert!(t[1].attached);
        assert!(t[7].attached);
        assert!(!t[8].attached);
    }
}
