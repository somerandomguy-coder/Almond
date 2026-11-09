//! Date expression extraction + normalization against a reference day (the meeting date), for
//! Vietnamese and English: "thứ Sáu", "thứ 6 tuần sau", "next Friday", "trước tháng 12",
//! "by December", "13/11", "2026-11-13", "ngày 13 tháng 11", "Nov 13th", "ngày mai", "tomorrow",
//! "tuần sau", "cuối tháng", ...

use crate::text::{TokKind, Token, tokenize};
use chrono::{Datelike, Days, Months, NaiveDate, TimeZone, Weekday};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Qualifier {
    /// "on Friday", "thứ Sáu"
    On,
    /// "by / before / until / trước / hạn"
    Deadline,
    /// "after / sau"
    After,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "granularity", rename_all = "snake_case")]
pub enum DateVal {
    Day {
        date: NaiveDate,
    },
    /// ISO week identified by its Monday.
    Week {
        monday: NaiveDate,
    },
    Month {
        year: i32,
        month: u32,
    },
}

impl DateVal {
    fn contains(&self, other: &DateVal) -> bool {
        match (self, other) {
            (DateVal::Month { year, month }, DateVal::Day { date }) => {
                date.year() == *year && date.month() == *month
            }
            (DateVal::Month { year, month }, DateVal::Week { monday }) => {
                let sunday = *monday + Days::new(6);
                (monday.year() == *year && monday.month() == *month)
                    || (sunday.year() == *year && sunday.month() == *month)
            }
            (DateVal::Week { monday }, DateVal::Day { date }) => {
                *date >= *monday && *date < *monday + Days::new(7)
            }
            _ => false,
        }
    }

    /// First day covered by this value.
    pub fn first_day(&self) -> NaiveDate {
        match *self {
            DateVal::Day { date } => date,
            DateVal::Week { monday } => monday,
            DateVal::Month { year, month } => NaiveDate::from_ymd_opt(year, month, 1).unwrap(),
        }
    }

    /// Last day covered by this value.
    pub fn last_day(&self) -> NaiveDate {
        match *self {
            DateVal::Day { date } => date,
            DateVal::Week { monday } => monday + Days::new(6),
            DateVal::Month { year, month } => {
                let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
                first + Months::new(1) - Days::new(1)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DateExpr {
    pub surface: String,
    pub span: (usize, usize),
    pub qualifier: Qualifier,
    pub value: DateVal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Match {
    Exact,
    /// One side is coarser (a day inside a month / week).
    Partial,
    Conflict,
}

impl DateExpr {
    pub fn compare(&self, other: &DateExpr) -> Match {
        use Qualifier::*;
        if matches!(
            (self.qualifier, other.qualifier),
            (After, Deadline) | (Deadline, After)
        ) {
            return Match::Conflict;
        }
        if self.value == other.value {
            Match::Exact
        } else if self.value.contains(&other.value) || other.value.contains(&self.value) {
            Match::Partial
        } else {
            Match::Conflict
        }
    }
}

/// Meeting start -> local calendar day. `tz_offset_min = None` uses this machine's local zone.
pub fn reference_day(started_at_ms: i64, tz_offset_min: Option<i32>) -> NaiveDate {
    match tz_offset_min.and_then(|m| chrono::FixedOffset::east_opt(m * 60)) {
        Some(off) => off
            .timestamp_millis_opt(started_at_ms)
            .single()
            .map(|d| d.date_naive()),
        None => chrono::Local
            .timestamp_millis_opt(started_at_ms)
            .single()
            .map(|d| d.date_naive()),
    }
    .unwrap_or_default()
}

fn weekday_from_vi(w: &str) -> Option<Weekday> {
    Some(match w {
        "hai" | "2" => Weekday::Mon,
        "ba" | "3" => Weekday::Tue,
        "tư" | "4" => Weekday::Wed,
        "năm" | "5" => Weekday::Thu,
        "sáu" | "6" => Weekday::Fri,
        "bảy" | "bẩy" | "7" => Weekday::Sat,
        _ => return None,
    })
}

fn weekday_en(w: &str) -> Option<Weekday> {
    Some(match w {
        "monday" => Weekday::Mon,
        "tuesday" => Weekday::Tue,
        "wednesday" => Weekday::Wed,
        "thursday" => Weekday::Thu,
        "friday" => Weekday::Fri,
        "saturday" => Weekday::Sat,
        "sunday" => Weekday::Sun,
        _ => return None,
    })
}

fn month_en(t: &Token) -> Option<u32> {
    Some(match t.lower.as_str() {
        "january" | "jan" => 1,
        "february" | "feb" => 2,
        "march" | "mar" => 3,
        "april" | "apr" => 4,
        "may" if t.raw == "May" => 5,
        "june" | "jun" => 6,
        "july" | "jul" => 7,
        "august" | "aug" => 8,
        "september" | "sep" | "sept" => 9,
        "october" | "oct" => 10,
        "november" | "nov" => 11,
        "december" | "dec" => 12,
        _ => return None,
    })
}

fn vi_month_word(toks: &[Token], j: usize) -> Option<(u32, usize)> {
    let w = toks.get(j)?.lower.as_str();
    let n = |k: usize| toks.get(j + k).map(|t| t.lower.as_str());
    let v = match w {
        "giêng" => 1,
        "hai" => 2,
        "ba" => 3,
        "tư" => 4,
        "năm" => 5,
        "sáu" => 6,
        "bảy" => 7,
        "tám" => 8,
        "chín" => 9,
        "chạp" => 12,
        "mười" => match n(1) {
            Some("một") => return Some((11, 2)),
            Some("hai") => return Some((12, 2)),
            _ => 10,
        },
        _ => return None,
    };
    Some((v, 1))
}

fn num(t: Option<&Token>) -> Option<u32> {
    let t = t?;
    if t.kind == TokKind::Num && t.raw.chars().all(|c| c.is_ascii_digit()) {
        t.raw.parse().ok()
    } else {
        None
    }
}

fn monday_of(d: NaiveDate) -> NaiveDate {
    d - Days::new(d.weekday().num_days_from_monday() as u64)
}

fn nearest_year(r: NaiveDate, month: u32, day: u32) -> Option<NaiveDate> {
    [r.year(), r.year() + 1, r.year() - 1]
        .iter()
        .filter_map(|y| NaiveDate::from_ymd_opt(*y, month, day.max(1)))
        .min_by_key(|d| (*d - r).num_days().abs())
}

fn qualifier_before(toks: &[Token], i: usize) -> Qualifier {
    let w = |k: usize| i.checked_sub(k).map(|p| toks[p].lower.as_str());
    for k in 1..=3 {
        match w(k) {
            Some("trước" | "by" | "before" | "until" | "till" | "hạn" | "deadline" | "muộn") =>
            {
                return Qualifier::Deadline;
            }
            Some("sau" | "after") => return Qualifier::After,
            Some(
                "vào" | "on" | "the" | "ngày" | "this" | "next" | "cuối" | "end" | "of" | "đầu"
                | "hôm" | "lúc" | "nhất" | "chậm" | "là" | "later" | "than" | "no",
            ) => {
                continue;
            }
            _ => return Qualifier::On,
        }
    }
    Qualifier::On
}

/// Extract and resolve date expressions in `text` relative to `reference`.
pub fn extract(text: &str, reference: NaiveDate) -> Vec<DateExpr> {
    let toks = tokenize(text);
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if let Some((value, end)) = parse_at(&toks, i, reference) {
            let span = (toks[i].span.0, toks[end - 1].span.1);
            out.push(DateExpr {
                surface: chars[span.0..span.1].iter().collect(),
                span,
                qualifier: qualifier_before(&toks, i),
                value,
            });
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// Week shift words right after an expression: "tuần sau" (+1), "tuần trước" (-1), "này" (0).
fn week_shift_after(toks: &[Token], j: usize) -> Option<(i64, usize)> {
    let a = toks.get(j).map(|t| t.lower.as_str());
    let b = toks.get(j + 1).map(|t| t.lower.as_str());
    match (a, b) {
        (Some("tuần"), Some("sau" | "tới" | "nữa")) => Some((1, 2)),
        (Some("tuần"), Some("trước")) => Some((-1, 2)),
        (Some("tuần"), Some("này")) => Some((0, 2)),
        (Some("này"), _) => Some((0, 1)),
        _ => None,
    }
}

fn resolve_weekday(reference: NaiveDate, wd: Weekday, shift: Option<i64>) -> NaiveDate {
    match shift {
        Some(s) => {
            let monday = monday_of(reference);
            let base = if s >= 0 {
                monday + Days::new(7 * s as u64)
            } else {
                monday - Days::new(7 * (-s) as u64)
            };
            base + Days::new(wd.num_days_from_monday() as u64)
        }
        None => {
            let delta = (7 + wd.num_days_from_monday() as i64
                - reference.weekday().num_days_from_monday() as i64)
                % 7;
            reference + Days::new(delta as u64)
        }
    }
}

fn parse_at(toks: &[Token], i: usize, r: NaiveDate) -> Option<(DateVal, usize)> {
    let t = &toks[i];
    let lw = |k: usize| toks.get(i + k).map(|t| t.lower.as_str());
    let prev = i.checked_sub(1).map(|p| toks[p].lower.as_str());

    // --- ISO yyyy-mm-dd
    if t.kind == TokKind::Num
        && t.raw.len() == 4
        && lw(1) == Some("-")
        && let (Some(y), Some(m), Some(d)) =
            (num(Some(t)), num(toks.get(i + 2)), num(toks.get(i + 4)))
        && lw(3) == Some("-")
        && let Some(date) = NaiveDate::from_ymd_opt(y as i32, m, d)
    {
        return Some((DateVal::Day { date }, i + 5));
    }
    // --- d/m(/y)
    if t.kind == TokKind::Num
        && lw(1) == Some("/")
        && let (Some(a), Some(b)) = (num(Some(t)), num(toks.get(i + 2)))
    {
        let (d, m) = if b > 12 && a <= 12 { (b, a) } else { (a, b) };
        let (date, end) = if lw(3) == Some("/")
            && let Some(y) = num(toks.get(i + 4))
        {
            let y = if y < 100 { 2000 + y } else { y };
            (NaiveDate::from_ymd_opt(y as i32, m, d), i + 5)
        } else {
            (nearest_year(r, m, d).filter(|x| x.day() == d), i + 3)
        };
        return date.map(|date| (DateVal::Day { date }, end));
    }
    // --- relative days
    match (t.lower.as_str(), lw(1)) {
        ("hôm", Some("nay")) => return Some((DateVal::Day { date: r }, i + 2)),
        ("today", _) => return Some((DateVal::Day { date: r }, i + 1)),
        ("ngày", Some("mai")) => {
            return Some((
                DateVal::Day {
                    date: r + Days::new(1),
                },
                i + 2,
            ));
        }
        ("tomorrow", _) => {
            return Some((
                DateVal::Day {
                    date: r + Days::new(1),
                },
                i + 1,
            ));
        }
        ("ngày", Some("mốt" | "kia")) => {
            return Some((
                DateVal::Day {
                    date: r + Days::new(2),
                },
                i + 2,
            ));
        }
        _ => {}
    }
    // --- VI weekday: "thứ Sáu", "thứ 6", "chủ nhật", "T6" (+ "tuần sau")
    let vi_wd = match (t.lower.as_str(), lw(1)) {
        ("thứ", Some(w)) if prev != Some("lần") => weekday_from_vi(w).map(|d| (d, 2)),
        ("chủ", Some("nhật")) => Some((Weekday::Sun, 2)),
        ("t", Some(w)) if toks.get(i + 1).is_some_and(|n| n.attached) && t.raw == "T" => {
            weekday_from_vi(w).map(|d| (d, 2))
        }
        ("cn", _) if t.raw == "CN" => Some((Weekday::Sun, 1)),
        _ => None,
    };
    let en_wd = weekday_en(&t.lower).map(|d| (d, 1));
    if let Some((wd, len)) = vi_wd.or(en_wd) {
        let mut end = i + len;
        let mut shift = match prev {
            Some("next") => Some(1),
            Some("last") => Some(-1),
            Some("this") => Some(0),
            _ => None,
        };
        if let Some((s, l)) = week_shift_after(toks, end) {
            shift = Some(s);
            end += l;
        } else if toks.get(end).map(|t| t.lower.as_str()) == Some("next")
            && toks.get(end + 1).map(|t| t.lower.as_str()) == Some("week")
        {
            shift = Some(1);
            end += 2;
        }
        return Some((
            DateVal::Day {
                date: resolve_weekday(r, wd, shift),
            },
            end,
        ));
    }
    // --- weeks / months relative
    match (t.lower.as_str(), lw(1), lw(2)) {
        ("tuần", Some("sau" | "tới"), _) | ("next", Some("week"), _) => {
            return Some((
                DateVal::Week {
                    monday: monday_of(r) + Days::new(7),
                },
                i + 2,
            ));
        }
        ("tuần", Some("này"), _) | ("this", Some("week"), _) => {
            return Some((
                DateVal::Week {
                    monday: monday_of(r),
                },
                i + 2,
            ));
        }
        ("cuối", Some("tuần"), _) => {
            return Some((
                DateVal::Week {
                    monday: monday_of(r),
                },
                i + 2,
            ));
        }
        ("end", Some("of"), Some("week")) => {
            return Some((
                DateVal::Week {
                    monday: monday_of(r),
                },
                i + 3,
            ));
        }
        ("tháng", Some("sau" | "tới"), _) | ("next", Some("month"), _) => {
            let d = r + Months::new(1);
            return Some((
                DateVal::Month {
                    year: d.year(),
                    month: d.month(),
                },
                i + 2,
            ));
        }
        ("cuối", Some("tháng"), n)
            if num(toks.get(i + 2)).is_none() && vi_month_word(toks, i + 2).is_none() =>
        {
            return Some((
                DateVal::Month {
                    year: r.year(),
                    month: r.month(),
                },
                i + 2 + usize::from(n == Some("này")),
            ));
        }
        ("end", Some("of"), Some("month")) => {
            return Some((
                DateVal::Month {
                    year: r.year(),
                    month: r.month(),
                },
                i + 3,
            ));
        }
        _ => {}
    }
    // --- "ngày 13 tháng 11 (năm 2026)", "13 tháng 11"
    let (day_first, k0) = if t.lower == "ngày" && num(toks.get(i + 1)).is_some() {
        (num(toks.get(i + 1)), i + 2)
    } else if t.kind == TokKind::Num && lw(1) == Some("tháng") {
        (num(Some(t)), i + 1)
    } else {
        (None, i)
    };
    if let Some(d) = day_first
        && toks.get(k0).map(|t| t.lower.as_str()) == Some("tháng")
        && let Some(m) = num(toks.get(k0 + 1))
        && (1..=12).contains(&m)
    {
        let (date, end) = vi_year(toks, k0 + 2)
            .map(|(y, e)| (NaiveDate::from_ymd_opt(y, m, d), e))
            .unwrap_or_else(|| (nearest_year(r, m, d).filter(|x| x.day() == d), k0 + 2));
        return date.map(|date| (DateVal::Day { date }, end));
    }
    // --- "tháng 12", "tháng mười hai" (+ "năm 2026")
    if t.lower == "tháng" {
        let m = num(toks.get(i + 1))
            .filter(|m| (1..=12).contains(m))
            .map(|m| (m, 1))
            .or_else(|| vi_month_word(toks, i + 1));
        if let Some((m, len)) = m {
            let end = i + 1 + len;
            let (year, end) = vi_year(toks, end).unwrap_or_else(|| {
                (
                    nearest_year(r, m, 1).map(|d| d.year()).unwrap_or(r.year()),
                    end,
                )
            });
            return Some((DateVal::Month { year, month: m }, end));
        }
    }
    // --- EN "November 13(th)", "13 November", "December"
    if let Some(m) = month_en(t) {
        let mut end = i + 1;
        let day = num(toks.get(i + 1)).filter(|d| (1..=31).contains(d));
        if day.is_some() {
            end += 1;
            if matches!(lw(2), Some("th" | "st" | "nd" | "rd")) {
                end += 1;
            }
        }
        let year = num(toks.get(end))
            .filter(|y| (1900..=2100).contains(y))
            .or_else(|| {
                (toks.get(end).map(|t| t.raw.as_str()) == Some(","))
                    .then(|| num(toks.get(end + 1)).filter(|y| (1900..=2100).contains(y)))
                    .flatten()
            });
        if let Some(y) = year {
            end += if toks.get(end).map(|t| t.raw.as_str()) == Some(",") {
                2
            } else {
                1
            };
            let v = match day {
                Some(d) => DateVal::Day {
                    date: NaiveDate::from_ymd_opt(y as i32, m, d)?,
                },
                None => DateVal::Month {
                    year: y as i32,
                    month: m,
                },
            };
            return Some((v, end));
        }
        return Some(match day {
            Some(d) => (
                DateVal::Day {
                    date: nearest_year(r, m, d).filter(|x| x.day() == d)?,
                },
                end,
            ),
            None => (
                DateVal::Month {
                    year: nearest_year(r, m, 1)?.year(),
                    month: m,
                },
                end,
            ),
        });
    }
    if t.kind == TokKind::Num
        && let Some(d) = num(Some(t)).filter(|d| (1..=31).contains(d))
    {
        let mut k = i + 1;
        if matches!(lw(1), Some("th" | "st" | "nd" | "rd")) {
            k += 1;
        }
        if toks.get(k).map(|t| t.lower.as_str()) == Some("of") {
            k += 1;
        }
        if let Some(m) = toks.get(k).and_then(month_en) {
            return Some((
                DateVal::Day {
                    date: nearest_year(r, m, d).filter(|x| x.day() == d)?,
                },
                k + 1,
            ));
        }
    }
    None
}

fn vi_year(toks: &[Token], j: usize) -> Option<(i32, usize)> {
    if toks.get(j).map(|t| t.lower.as_str()) == Some("năm")
        && let Some(y) = num(toks.get(j + 1)).filter(|y| (1900..=2100).contains(y))
    {
        return Some((y as i32, j + 2));
    }
    if toks.get(j).map(|t| t.raw.as_str()) == Some("/")
        && let Some(y) = num(toks.get(j + 1)).filter(|y| (1900..=2100).contains(y))
    {
        return Some((y as i32, j + 2));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Monday 2026-11-09
    fn r() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 9).unwrap()
    }
    fn day(y: i32, m: u32, d: u32) -> DateVal {
        DateVal::Day {
            date: NaiveDate::from_ymd_opt(y, m, d).unwrap(),
        }
    }
    fn one(s: &str) -> DateExpr {
        let v = extract(s, r());
        assert_eq!(v.len(), 1, "{s:?} -> {v:?}");
        v.into_iter().next().unwrap()
    }

    #[test]
    fn weekdays() {
        assert_eq!(
            one("Lan gửi kế hoạch trước thứ Sáu.").value,
            day(2026, 11, 13)
        );
        assert_eq!(one("trước thứ Sáu").qualifier, Qualifier::Deadline);
        assert_eq!(one("thứ 6 tuần sau").value, day(2026, 11, 20));
        assert_eq!(one("by Friday").value, day(2026, 11, 13));
        assert_eq!(one("next Friday").value, day(2026, 11, 20));
        assert_eq!(one("họp lại vào thứ Hai").value, day(2026, 11, 9));
        assert_eq!(one("chủ nhật").value, day(2026, 11, 15));
        assert_eq!(one("gặp T5").value, day(2026, 11, 12));
    }

    #[test]
    fn months_and_days() {
        let m = DateVal::Month {
            year: 2026,
            month: 12,
        };
        assert_eq!(one("trước tháng 12").value, m);
        assert_eq!(one("by December").value, m);
        assert_eq!(one("trước tháng mười hai").value, m);
        assert_eq!(one("ngày 20/11").value, day(2026, 11, 20));
        assert_eq!(one("2026-11-20").value, day(2026, 11, 20));
        assert_eq!(one("ngày 20 tháng 11 năm 2026").value, day(2026, 11, 20));
        assert_eq!(one("Nov 20th").value, day(2026, 11, 20));
        assert_eq!(one("20 November 2026").value, day(2026, 11, 20));
        assert_eq!(
            one("tháng 1").value,
            DateVal::Month {
                year: 2027,
                month: 1
            }
        );
        assert_eq!(one("ngày mai").value, day(2026, 11, 10));
        assert_eq!(
            one("tuần sau").value,
            DateVal::Week {
                monday: NaiveDate::from_ymd_opt(2026, 11, 16).unwrap()
            }
        );
    }

    #[test]
    fn comparisons() {
        let a = one("trước tháng 12");
        let b = one("by December");
        assert_eq!(a.compare(&b), Match::Exact);
        let c = one("by December 5");
        assert_eq!(a.compare(&c), Match::Partial);
        let d = one("trước thứ Năm");
        let e = one("before Friday");
        assert_eq!(d.compare(&e), Match::Conflict);
        let f = one("after Friday");
        assert_eq!(e.compare(&f), Match::Conflict);
    }

    #[test]
    fn ignores_non_dates() {
        assert!(extract("lần thứ hai", r()).is_empty());
        assert!(extract("We may need more time", r()).is_empty());
        assert!(extract("200 triệu đồng", r()).is_empty());
    }
}
