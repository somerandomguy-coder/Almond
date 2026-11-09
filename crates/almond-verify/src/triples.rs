//! `logical_form_kind = "triples.v1"`: a claim's logical form as (subject, predicate, object)
//! triples with typed literal objects.
//!
//! ```json
//! {"triples": [
//!   {"s": "ngân sách marketing Q4", "p": "amount",
//!    "o": {"type": "money", "value": 300000000, "currency": "VND", "text": "300 triệu đồng"}},
//!   {"s": "gửi kế hoạch", "p": "owner", "o": {"type": "person", "name": "Lan"}},
//!   {"s": "gửi kế hoạch", "p": "due",
//!    "o": {"type": "date", "granularity": "day", "date": "2026-11-13", "qualifier": "deadline", "text": "trước thứ Sáu"}}
//! ]}
//! ```
//!
//! Predicates are normalized to a small vocabulary where possible (see [`canonical_predicate`]);
//! unknown predicates are kept (snake_case) so future verifiers can use them.

use crate::dates::{self, DateVal, Qualifier};
use crate::names;
use crate::numbers::{self, QKind};
use crate::text::{TokKind, fold, tokenize};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

pub const KIND: &str = "triples.v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TriplesV1 {
    pub triples: Vec<Triple>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Triple {
    pub s: String,
    pub p: String,
    pub o: Literal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Literal {
    Person {
        name: String,
    },
    Entity {
        name: String,
    },
    Number {
        value: f64,
        #[serde(default)]
        text: Option<String>,
    },
    Money {
        value: f64,
        #[serde(default)]
        currency: Option<String>,
        #[serde(default)]
        text: Option<String>,
    },
    Percent {
        value: f64,
        #[serde(default)]
        text: Option<String>,
    },
    Date {
        #[serde(flatten)]
        value: DateVal,
        qualifier: Qualifier,
        #[serde(default)]
        text: Option<String>,
    },
    Text {
        value: String,
    },
}

impl Literal {
    /// (type tag, canonical value) used as Datalog constants.
    pub fn canonical(&self) -> (String, String) {
        match self {
            Literal::Person { name } => ("person".into(), names::key(name)),
            Literal::Entity { name } => ("entity".into(), names::key(name)),
            Literal::Number { value, .. } => ("number".into(), fmt_num(*value)),
            Literal::Money {
                value, currency, ..
            } => (
                "money".into(),
                format!("{}:{}", currency.as_deref().unwrap_or("?"), fmt_num(*value)),
            ),
            Literal::Percent { value, .. } => ("percent".into(), fmt_num(*value)),
            Literal::Date { value, .. } => ("date".into(), date_key(value)),
            Literal::Text { value } => ("text".into(), subject_key(value)),
        }
    }
}

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

pub fn date_key(v: &DateVal) -> String {
    match v {
        DateVal::Day { date } => date.to_string(),
        DateVal::Week { monday } => format!("week:{monday}"),
        DateVal::Month { year, month } => format!("{year}-{month:02}"),
    }
}

const SUBJECT_STOP: &[&str] = &[
    "la", "is", "are", "was", "the", "a", "an", "of", "to", "se", "da", "can", "phai", "will",
    "khoang", "about", "around", "duoc", "be", "va", "and", "cho", "for", "voi", "with", "by",
    "truoc", "before", "until", "vao", "on", "at", "luc", "cua", "cac", "nhung", "mot",
];

/// Canonical subject key: folded words minus function words.
pub fn subject_key(s: &str) -> String {
    tokenize(s)
        .into_iter()
        .filter(|t| t.kind != TokKind::Sym)
        .map(|t| t.folded)
        .filter(|w| !SUBJECT_STOP.contains(&w.as_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Map free-form predicates (from an LLM, VI or EN) to the small canonical vocabulary.
pub fn canonical_predicate(p: &str) -> String {
    let f = fold(p).replace([' ', '-'], "_");
    match f.as_str() {
        "owner" | "assignee" | "assigned_to" | "responsible" | "phu_trach" | "nguoi_phu_trach"
        | "who" | "by_whom" | "actor" | "agent" => "owner".into(),
        "due" | "deadline" | "due_date" | "han" | "han_chot" | "by" | "before" | "when" => {
            "due".into()
        }
        "amount" | "budget" | "cost" | "value" | "ngan_sach" | "price" | "gia" | "so_tien" => {
            "amount".into()
        }
        "count" | "number" | "quantity" | "headcount" | "so_luong" => "count".into(),
        "percent" | "percentage" | "rate" | "ti_le" | "ty_le" => "percent".into(),
        "date" | "on" | "ngay" | "time" => "date".into(),
        other => other.to_string(),
    }
}

/// Predicates that admit only one value per subject (used by the consistency rules).
pub const FUNCTIONAL: &[&str] = &[
    "owner", "due", "amount", "count", "percent", "date", "status",
];

/// Build typed literal from an LLM-provided object string + type hint, re-parsing numbers and
/// dates with our own normalizers (LLMs are bad at date arithmetic and VI number words).
pub fn literal_from(o: &str, o_type: &str, reference: NaiveDate) -> Literal {
    let t = o_type.to_ascii_lowercase();
    match t.as_str() {
        "money" | "number" | "percent" | "count" | "amount" => {
            if let Some(q) = numbers::extract(o).into_iter().next() {
                return match (q.kind, t.as_str()) {
                    (QKind::Percent, _) | (_, "percent") => Literal::Percent {
                        value: q.value,
                        text: Some(o.into()),
                    },
                    (QKind::Money { currency }, _) => Literal::Money {
                        value: q.value,
                        currency,
                        text: Some(o.into()),
                    },
                    (_, "money" | "amount") => Literal::Money {
                        value: q.value,
                        currency: None,
                        text: Some(o.into()),
                    },
                    _ => Literal::Number {
                        value: q.value,
                        text: Some(o.into()),
                    },
                };
            }
            if let Ok(v) = o.trim().parse::<f64>() {
                return Literal::Number {
                    value: v,
                    text: Some(o.into()),
                };
            }
            Literal::Text { value: o.into() }
        }
        "date" => match dates::extract(o, reference).into_iter().next() {
            Some(d) => Literal::Date {
                value: d.value,
                qualifier: d.qualifier,
                text: Some(o.into()),
            },
            None => Literal::Text { value: o.into() },
        },
        "person" => Literal::Person { name: o.into() },
        "entity" => Literal::Entity { name: o.into() },
        _ => Literal::Text { value: o.into() },
    }
}

/// Deterministic triples for a claim (used when no LLM is available, and for action items):
/// owner (sentence-initial person + verb), amounts/counts/percentages, due dates.
pub fn heuristic(
    text: &str,
    owner: Option<&str>,
    due: Option<&str>,
    known_people: &[String],
    reference: NaiveDate,
) -> TriplesV1 {
    let toks = tokenize(text);
    let qs = numbers::extract(text);
    let ds = dates::extract(text, reference);
    let people = names::extract(text, known_people);
    // "action" subject = text minus people, quantities, dates and qualifiers.
    let mut masked: Vec<(usize, usize)> = Vec::new();
    masked.extend(people.iter().map(|p| p.span));
    masked.extend(ds.iter().map(|d| d.span));
    let action: String = toks
        .iter()
        .filter(|t| !masked.iter().any(|(a, b)| t.span.0 >= *a && t.span.1 <= *b))
        .filter(|t| t.kind == TokKind::Word)
        .filter(|t| {
            !matches!(
                t.folded.as_str(),
                "truoc" | "by" | "before" | "han" | "until"
            )
        })
        .map(|t| t.raw.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = TriplesV1::default();
    let owner_name = owner.map(str::to_string).or_else(|| {
        people
            .iter()
            .find(|p| p.span.0 == toks.first().map(|t| t.span.0).unwrap_or(usize::MAX))
            .or_else(|| people.first())
            .map(|p| p.surface.clone())
    });
    if let Some(o) = owner_name {
        out.triples.push(Triple {
            s: action.clone(),
            p: "owner".into(),
            o: Literal::Person { name: o },
        });
    }
    let due_expr = due
        .and_then(|d| dates::extract(d, reference).into_iter().next())
        .or_else(|| {
            ds.iter()
                .find(|d| d.qualifier == Qualifier::Deadline)
                .or(ds.first())
                .cloned()
        });
    if let Some(d) = due_expr {
        out.triples.push(Triple {
            s: action.clone(),
            p: "due".into(),
            o: Literal::Date {
                value: d.value,
                qualifier: d.qualifier,
                text: Some(d.surface.clone()),
            },
        });
    }
    for q in qs {
        // Subject: up to 5 content tokens before the quantity, else 3 words after it.
        let before: Vec<&str> = toks
            .iter()
            .filter(|t| t.span.1 <= q.span.0 && t.kind != TokKind::Sym)
            .map(|t| t.raw.as_str())
            .filter(|w| !SUBJECT_STOP.contains(&fold(w).as_str()))
            .collect();
        let subj = if !before.is_empty() {
            before[before.len().saturating_sub(5)..].join(" ")
        } else {
            toks.iter()
                .filter(|t| t.span.0 >= q.span.1 && t.kind == TokKind::Word)
                .take(3)
                .map(|t| t.raw.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let (p, o) = match q.kind {
            QKind::Percent => (
                "percent",
                Literal::Percent {
                    value: q.value,
                    text: Some(q.surface.clone()),
                },
            ),
            QKind::Money { currency } => (
                "amount",
                Literal::Money {
                    value: q.value,
                    currency,
                    text: Some(q.surface.clone()),
                },
            ),
            QKind::Plain if q.value >= 1000.0 => (
                "amount",
                Literal::Money {
                    value: q.value,
                    currency: None,
                    text: Some(q.surface.clone()),
                },
            ),
            QKind::Plain => (
                "count",
                Literal::Number {
                    value: q.value,
                    text: Some(q.surface.clone()),
                },
            ),
        };
        out.triples.push(Triple {
            s: subj,
            p: p.into(),
            o,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_triples() {
        let r = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let t = heuristic("Lan gửi kế hoạch trước thứ Sáu.", None, None, &[], r);
        assert_eq!(t.triples.len(), 2);
        assert_eq!(t.triples[0].p, "owner");
        assert_eq!(subject_key(&t.triples[0].s), "gui ke hoach");
        assert_eq!(
            t.triples[1].o.canonical(),
            ("date".into(), "2026-11-13".into())
        );

        let t = heuristic(
            "Ngân sách marketing Q4 là 300 triệu đồng.",
            None,
            None,
            &[],
            r,
        );
        assert_eq!(t.triples.len(), 1);
        assert_eq!(subject_key(&t.triples[0].s), "ngan sach marketing q 4");
        assert_eq!(
            t.triples[0].o.canonical(),
            ("money".into(), "VND:300000000".into())
        );
    }

    #[test]
    fn llm_literals_are_renormalized() {
        let r = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        assert_eq!(
            literal_from("hai trăm triệu đồng", "money", r).canonical(),
            ("money".into(), "VND:200000000".into())
        );
        assert_eq!(
            literal_from("thứ Sáu", "date", r).canonical(),
            ("date".into(), "2026-11-13".into())
        );
        assert_eq!(canonical_predicate("Ngân sách"), "amount");
        assert_eq!(canonical_predicate("assigned to"), "owner");
    }
}
