//! Quantity extraction for Vietnamese + English text: digits ("200", "1,5", "200.000.000"),
//! spelled-out numbers ("hai trăm triệu", "two hundred million", "hai mươi lăm"), scale suffixes
//! ("200tr", "1,5 tỷ", "$50k", "3 million"), money and percentages.
//!
//! Dates and times ("tháng 12", "thứ 6", "10h", "13/11", "Q4") are deliberately skipped: the date
//! verifier owns them.

use crate::text::{TokKind, Token, tokenize};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QKind {
    Plain,
    Money { currency: Option<String> },
    Percent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quantity {
    pub value: f64,
    #[serde(flatten)]
    pub kind: QKind,
    /// Surface text as written.
    pub surface: String,
    /// Char span in the source text.
    pub span: (usize, usize),
}

impl Quantity {
    /// Same magnitude (relative tolerance 1e-6).
    pub fn same_value(&self, other: &Quantity) -> bool {
        let (a, b) = (self.value, other.value);
        (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0)
    }

    /// Could these two quantities talk about the same thing? (percent vs money never does).
    pub fn compatible(&self, other: &Quantity) -> bool {
        match (&self.kind, &other.kind) {
            (QKind::Percent, QKind::Percent) => true,
            (QKind::Percent, _) | (_, QKind::Percent) => false,
            (QKind::Money { currency: Some(a) }, QKind::Money { currency: Some(b) }) => a == b,
            _ => true,
        }
    }

    pub fn matches(&self, other: &Quantity) -> bool {
        self.compatible(other) && self.same_value(other)
    }
}

/// Unit words 1..19 (VI with diacritics, EN). Context-only VI forms are handled separately.
fn unit_value(w: &str) -> Option<f64> {
    Some(match w {
        "một" | "one" => 1.0,
        "hai" | "two" => 2.0,
        "ba" | "three" => 3.0,
        "bốn" | "four" => 4.0,
        "năm" | "five" => 5.0,
        "sáu" | "six" => 6.0,
        "bảy" | "bẩy" | "seven" => 7.0,
        "tám" | "eight" => 8.0,
        "chín" | "nine" => 9.0,
        "ten" => 10.0,
        "eleven" => 11.0,
        "twelve" => 12.0,
        "thirteen" => 13.0,
        "fourteen" => 14.0,
        "fifteen" => 15.0,
        "sixteen" => 16.0,
        "seventeen" => 17.0,
        "eighteen" => 18.0,
        "nineteen" => 19.0,
        _ => return None,
    })
}

/// VI forms that are only numbers after "mươi"/"mười"/"trăm"/"lẻ": hai mươi **tư**, mười **lăm**.
fn inner_unit_value(w: &str) -> Option<f64> {
    Some(match w {
        "mốt" => 1.0,
        "tư" => 4.0,
        "lăm" | "nhăm" => 5.0,
        _ => return None,
    })
}

fn tens_value(w: &str) -> Option<f64> {
    Some(match w {
        "twenty" => 20.0,
        "thirty" => 30.0,
        "forty" => 40.0,
        "fifty" => 50.0,
        "sixty" => 60.0,
        "seventy" => 70.0,
        "eighty" => 80.0,
        "ninety" => 90.0,
        _ => return None,
    })
}

/// Scale words usable after both digits and number words.
fn scale_value(w: &str) -> Option<f64> {
    Some(match w {
        "nghìn" | "ngàn" | "thousand" | "thousands" | "k" => 1e3,
        "triệu" | "million" | "millions" | "tr" | "củ" | "mil" => 1e6,
        "tỷ" | "tỉ" | "billion" | "billions" | "bn" => 1e9,
        _ => return None,
    })
}

fn is_number_word(w: &str) -> bool {
    unit_value(w).is_some()
        || inner_unit_value(w).is_some()
        || tens_value(w).is_some()
        || scale_value(w).is_some()
        || matches!(
            w,
            "mười" | "mươi" | "trăm" | "hundred" | "linh" | "lẻ" | "rưỡi" | "and"
        )
}

/// Words after which a bare number is a date/time component, not a quantity.
fn is_date_prefix(w: &str) -> bool {
    matches!(
        w,
        "tháng"
            | "ngày"
            | "thứ"
            | "quý"
            | "lúc"
            | "tuần"
            | "q"
            | "t"
            | "january"
            | "february"
            | "march"
            | "april"
            | "may"
            | "june"
            | "july"
            | "august"
            | "september"
            | "october"
            | "november"
            | "december"
            | "jan"
            | "feb"
            | "mar"
            | "apr"
            | "jun"
            | "jul"
            | "aug"
            | "sep"
            | "sept"
            | "oct"
            | "nov"
            | "dec"
            | "week"
            | "quarter"
            | "at"
    ) || (w == "năm")
}

fn is_month_word(w: &str) -> bool {
    matches!(
        w,
        "january"
            | "february"
            | "march"
            | "april"
            | "june"
            | "july"
            | "august"
            | "september"
            | "october"
            | "november"
            | "december"
            | "jan"
            | "feb"
            | "mar"
            | "apr"
            | "jun"
            | "jul"
            | "aug"
            | "sep"
            | "sept"
            | "oct"
            | "nov"
            | "dec"
    )
}

/// Counting nouns that make an otherwise ambiguous VI word ("năm", "ba") a number.
fn is_classifier(w: &str) -> bool {
    matches!(
        w,
        "người"
            | "kỹ"
            | "bạn"
            | "ngày"
            | "tuần"
            | "tháng"
            | "năm"
            | "khách"
            | "lần"
            | "nhân"
            | "cái"
            | "chiếc"
            | "ứng"
            | "dự"
            | "đội"
            | "nhóm"
            | "bước"
            | "phương"
            | "sản"
            | "tính"
            | "mục"
            | "điểm"
            | "giờ"
            | "phút"
            | "vị"
            | "thành"
            | "khoản"
            | "buổi"
            | "trường"
            | "công"
            | "lập"
            | "tester"
            | "dev"
            | "developer"
            | "engineer"
            | "engineers"
    )
}

fn currency_after(t: &Token) -> Option<&'static str> {
    Some(match t.lower.as_str() {
        "đồng" | "đ" | "vnd" | "vnđ" => "VND",
        "usd" | "dollar" | "dollars" | "đô" => "USD",
        "eur" | "euro" | "euros" => "EUR",
        _ => return None,
    })
}

fn currency_before(t: &Token) -> Option<&'static str> {
    Some(match t.raw.as_str() {
        "$" => "USD",
        "€" => "EUR",
        "£" => "GBP",
        _ => return None,
    })
}

/// Parse "200", "1,5", "1.500", "200.000.000", "1,000,000", "2.5".
pub fn parse_digits(s: &str) -> Option<f64> {
    let seps: Vec<(usize, char)> = s
        .char_indices()
        .filter(|(_, c)| *c == '.' || *c == ',')
        .collect();
    if seps.is_empty() {
        return s.parse().ok();
    }
    let groups: Vec<&str> = s.split(['.', ',']).collect();
    let all_three = groups[1..].iter().all(|g| g.len() == 3);
    let same_sep = seps.iter().all(|(_, c)| *c == seps[0].1);
    if seps.len() > 1 && same_sep && all_three {
        return groups.concat().parse().ok();
    }
    if seps.len() == 1 {
        // "1.500" / "1,500" -> thousands; "1,5" / "2.75" -> decimal.
        return if all_three && groups[0] != "0" {
            groups.concat().parse().ok()
        } else {
            format!("{}.{}", groups[0], groups[1]).parse().ok()
        };
    }
    // Mixed: "1,234.56" or "1.234,56" -> last separator is decimal.
    let (last_i, _) = *seps.last().unwrap();
    let int: String = s[..last_i].chars().filter(|c| c.is_ascii_digit()).collect();
    format!("{}.{}", int, &s[last_i + 1..]).parse().ok()
}

/// Extract all quantities in `text`.
pub fn extract(text: &str) -> Vec<Quantity> {
    let toks = tokenize(text);
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if let Some((q, next)) = parse_at(&toks, i, &chars) {
            out.push(q);
            i = next;
        } else {
            i += 1;
        }
    }
    out
}

fn parse_at(toks: &[Token], i: usize, chars: &[char]) -> Option<(Quantity, usize)> {
    let t = &toks[i];
    let prev = i.checked_sub(1).map(|p| &toks[p]);
    let start_tok = i;
    let mut j = i;
    let mut value;
    let mut had_scale = false;
    let mut word_count = 0;

    match t.kind {
        TokKind::Num => {
            // Q4, H1, T6, v2 ... : digit glued to a letter.
            if t.attached && prev.is_some_and(|p| p.kind == TokKind::Word) {
                return None;
            }
            if prev.is_some_and(|p| is_date_prefix(&p.lower)) {
                return None;
            }
            // 13/11, 2026-11-13, 10:30
            if prev.is_some_and(|p| t.attached && matches!(p.raw.as_str(), "/" | "-" | ":")) {
                return None;
            }
            if let Some(n) = toks.get(i + 1)
                && n.attached
                && matches!(n.raw.as_str(), "/" | ":")
            {
                return None;
            }
            value = parse_digits(&t.raw)?;
            j += 1;
        }
        TokKind::Word => {
            let w = t.lower.as_str();
            if inner_unit_value(w).is_some()
                || matches!(w, "linh" | "lẻ" | "rưỡi" | "and" | "mươi")
                || scale_value(w).is_some()
                || prev.is_some_and(|p| is_date_prefix(&p.lower))
            {
                return None;
            }
            if !(unit_value(w).is_some()
                || tens_value(w).is_some()
                || matches!(w, "mười" | "trăm" | "hundred"))
            {
                return None;
            }
            // Accumulate spelled-out number.
            let (mut total, mut cur, mut last_scale) = (0.0f64, 0.0f64, 1.0f64);
            let mut after_tens = false;
            while j < toks.len() && toks[j].kind == TokKind::Word && is_number_word(&toks[j].lower)
            {
                let w = toks[j].lower.as_str();
                if w == "and" {
                    // only inside a number: "two hundred and five"
                    if !toks.get(j + 1).is_some_and(|n| {
                        unit_value(&n.lower).is_some() || tens_value(&n.lower).is_some()
                    }) || word_count == 0
                    {
                        break;
                    }
                    j += 1;
                    continue;
                }
                if let Some(u) = unit_value(w) {
                    cur += u;
                    after_tens = false;
                } else if let Some(u) = inner_unit_value(w) {
                    if !after_tens && !matches!(toks[j - 1].lower.as_str(), "linh" | "lẻ") {
                        break;
                    }
                    cur += u;
                    after_tens = false;
                } else if let Some(v) = tens_value(w) {
                    cur += v;
                } else if w == "mười" {
                    cur += 10.0;
                    after_tens = true;
                } else if w == "mươi" {
                    cur = if cur == 0.0 { 1.0 } else { cur } * 10.0;
                    after_tens = true;
                } else if w == "trăm" || w == "hundred" {
                    cur = if cur == 0.0 { 1.0 } else { cur } * 100.0;
                } else if let Some(s) = scale_value(w) {
                    if w == "k" || w == "tr" || w == "bn" || w == "mil" || w == "củ" {
                        break;
                    }
                    total += if cur == 0.0 { 1.0 } else { cur } * s;
                    cur = 0.0;
                    last_scale = s;
                    had_scale = true;
                } else if w == "rưỡi" {
                    if last_scale > 1.0 && cur == 0.0 {
                        total += last_scale / 2.0;
                    } else {
                        cur += 0.5;
                    }
                } else if matches!(w, "linh" | "lẻ") {
                    // filler: "một trăm linh năm"
                }
                word_count += 1;
                j += 1;
            }
            value = total + cur;
            if word_count == 0 {
                return None;
            }
            // Bare ambiguous single words need a counting noun after them.
            if word_count == 1 && !had_scale {
                if value == 1.0 {
                    return None; // "một"/"one" is mostly an article
                }
                if matches!(w, "năm" | "ba")
                    && !toks.get(j).is_some_and(|n| is_classifier(&n.lower))
                {
                    return None;
                }
            }
        }
        TokKind::Sym => {
            // "$50k" handled from the digit token; symbols themselves are not numbers.
            return None;
        }
    }

    // Scale suffixes after digits: "200 triệu", "200tr", "1,5 tỷ", "50k", "3 million".
    if t.kind == TokKind::Num {
        let money_prefix = prev.and_then(currency_before).is_some();
        while let Some(n) = toks.get(j) {
            let w = n.lower.as_str();
            let s = scale_value(w).or(match w {
                "m" | "mn" if money_prefix => Some(1e6),
                "b" if money_prefix => Some(1e9),
                "trăm" | "hundred" => Some(100.0),
                _ => None,
            });
            match s {
                Some(s) => {
                    value *= s;
                    had_scale = true;
                    j += 1;
                }
                None => break,
            }
        }
        if toks.get(j).is_some_and(|n| n.lower == "rưỡi") {
            value *= 1.5;
            j += 1;
        }
        let next = toks.get(j);
        // times: "10h", "10 giờ", "3pm"; ordinals "5th"; 4-digit years.
        if !had_scale
            && next.is_some_and(|n| {
                matches!(
                    n.lower.as_str(),
                    "h" | "giờ" | "g" | "am" | "pm" | "th" | "st" | "nd" | "rd" | "tháng"
                ) || is_month_word(&n.lower)
            })
        {
            return None;
        }
        if !had_scale
            && !t.raw.contains(['.', ','])
            && (1900.0..=2100.0).contains(&value)
            && prev.and_then(currency_before).is_none()
            && next.and_then(currency_after).is_none()
        {
            return None;
        }
    }

    // Kind: percent / money.
    let mut kind = QKind::Plain;
    let mut end = j;
    if let Some(n) = toks.get(j) {
        if n.raw == "%" {
            kind = QKind::Percent;
            end = j + 1;
        } else if n.lower == "phần" && toks.get(j + 1).is_some_and(|m| m.lower == "trăm") {
            kind = QKind::Percent;
            end = j + 2;
        } else if matches!(n.lower.as_str(), "percent" | "pct") {
            kind = QKind::Percent;
            end = j + 1;
        } else if n.lower == "per" && toks.get(j + 1).is_some_and(|m| m.lower == "cent") {
            kind = QKind::Percent;
            end = j + 2;
        } else if let Some(c) = currency_after(n) {
            kind = QKind::Money {
                currency: Some(c.into()),
            };
            end = j + 1;
        }
    }
    let mut begin = start_tok;
    if kind == QKind::Plain
        && let Some(p) = prev
        && let Some(c) = currency_before(p)
    {
        kind = QKind::Money {
            currency: Some(c.into()),
        };
        begin = start_tok - 1;
    }
    let span = (toks[begin].span.0, toks[end - 1].span.1);
    let surface: String = chars[span.0..span.1].iter().collect();
    Some((
        Quantity {
            value,
            kind,
            surface,
            span,
        },
        end,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals(s: &str) -> Vec<f64> {
        extract(s).into_iter().map(|q| q.value).collect()
    }

    #[test]
    fn digits() {
        assert_eq!(parse_digits("200"), Some(200.0));
        assert_eq!(parse_digits("1,5"), Some(1.5));
        assert_eq!(parse_digits("1.500"), Some(1500.0));
        assert_eq!(parse_digits("200.000.000"), Some(2e8));
        assert_eq!(parse_digits("1,000,000"), Some(1e6));
        assert_eq!(parse_digits("2.75"), Some(2.75));
        assert_eq!(parse_digits("1.234,5"), Some(1234.5));
    }

    #[test]
    fn vietnamese_money() {
        assert_eq!(vals("Ngân sách marketing là 200 triệu đồng."), [2e8]);
        assert_eq!(vals("ngân sách 200tr"), [2e8]);
        assert_eq!(vals("khoảng hai trăm triệu"), [2e8]);
        assert_eq!(vals("1,5 tỷ"), [1.5e9]);
        assert_eq!(vals("hai triệu rưỡi"), [2.5e6]);
        assert_eq!(vals("hai mươi lăm người"), [25.0]);
        assert_eq!(vals("200.000.000 đ"), [2e8]);
        let q = &extract("300 triệu đồng")[0];
        assert_eq!(
            q.kind,
            QKind::Money {
                currency: Some("VND".into())
            }
        );
    }

    #[test]
    fn english_numbers() {
        assert_eq!(vals("hire two backend engineers"), [2.0]);
        assert_eq!(vals("two hundred and fifty users"), [250.0]);
        assert_eq!(vals("$50k budget"), [50_000.0]);
        assert_eq!(vals("a $3m round"), [3e6]);
        assert_eq!(vals("grew 15% this quarter"), [15.0]);
        assert_eq!(vals("tăng 15 phần trăm"), [15.0]);
        assert_eq!(extract("15%")[0].kind, QKind::Percent);
    }

    #[test]
    fn skips_dates_times_and_articles() {
        assert!(vals("trước tháng 12").is_empty());
        assert!(vals("by December 5").is_empty());
        assert!(vals("thứ 6 lúc 10h").is_empty());
        assert!(vals("ngày 13/11/2026").is_empty());
        assert!(vals("kế hoạch Q4 năm 2026").is_empty());
        assert!(vals("gửi một bản kế hoạch, sẽ đầu tư thêm").is_empty());
        assert!(vals("năm nay và năm sau").is_empty());
        assert!(vals("thứ Sáu").is_empty());
        assert!(vals("send it at 3pm").is_empty());
        assert_eq!(vals("năm người tham dự"), [5.0]);
    }
}
