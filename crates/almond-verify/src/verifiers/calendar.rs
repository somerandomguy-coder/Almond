//! `verify.calendar`: claims about the meeting itself — when it happened, who attended, how many
//! people — are checked against the linked `CalendarEvent` (and the transcript's speakers: someone
//! who spoke cannot have been absent).

use super::numbers::capitalize;
use super::{AtomStatus, VerifyConfig, label_for, verdict};
use crate::dates::{self, Qualifier};
use crate::evidence::{EvidenceIndex, mentions};
use crate::names;
use crate::text::{TokKind, fold, tokenize};
use almond_types::artifact::body::{Claim, Verdict};
use almond_types::calendar::CalendarEvent;
use almond_types::verify::{Verifier, VerifierDescriptor, VerifyContext};
use async_trait::async_trait;
use chrono::{FixedOffset, TimeZone, Timelike};
use serde_json::json;

pub const NAME: &str = "verify.calendar";

#[derive(Debug, Clone, Default)]
pub struct CalendarVerifier {
    pub config: VerifyConfig,
}

const MEETING_WORDS: &[&str] = &[
    "hop", "meeting", "call", "standup", "sync", "buoi", "cuoc", "session",
];
const NEG_ATTEND: &[&str] = &[
    "vang mat",
    "vang",
    "khong tham du",
    "khong tham gia",
    "khong co mat",
    "absent",
    "did not attend",
    "didn t attend",
    "missed the",
    "declined",
    "was not present",
    "wasn t present",
];
const POS_ATTEND: &[&str] = &[
    "tham du",
    "tham gia",
    "co mat",
    "attended",
    "joined",
    "was present",
    "were present",
    "participated",
];
const PEOPLE_WORDS: &[&str] = &[
    "nguoi",
    "attendees",
    "people",
    "participants",
    "thanh vien",
    "members",
];

fn norm(s: &str) -> String {
    let words: Vec<String> = tokenize(s)
        .into_iter()
        .filter(|t| t.kind != TokKind::Sym || t.raw == "'")
        .map(|t| if t.raw == "'" { " ".into() } else { t.folded })
        .collect();
    format!(
        " {} ",
        words
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn has_phrase(normed: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|p| normed.contains(&format!(" {p} ")))
}

/// "10h", "10h30", "10:30", "10 giờ", "3pm", "3 giờ chiều" -> (hour, minute) 24h.
pub fn times_of_day(text: &str) -> Vec<(u32, u32, String)> {
    let toks = tokenize(text);
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if t.kind != TokKind::Num {
            continue;
        }
        let Ok(h) = t.raw.parse::<u32>() else {
            continue;
        };
        let nx = |k: usize| toks.get(i + k);
        let lw = |k: usize| nx(k).map(|t| t.lower.as_str()).unwrap_or("");
        let (mut hour, mut minute) = (h, 0);
        let mut end;
        match lw(1) {
            "h" | "g" | "giờ" | "am" | "pm" => {
                end = i + 1;
                if let Some(n) = nx(2)
                    && n.kind == TokKind::Num
                    && n.attached
                    && let Ok(m) = n.raw.parse::<u32>()
                {
                    minute = m;
                    end = i + 2;
                }
            }
            ":" if nx(2).is_some_and(|n| n.kind == TokKind::Num) => {
                minute = nx(2).unwrap().raw.parse().unwrap_or(0);
                end = i + 2;
            }
            _ => continue,
        }
        let tail: Vec<&str> = (end + 1..end + 4)
            .filter_map(|k| toks.get(k).map(|t| t.lower.as_str()))
            .collect();
        let pm = lw(1) == "pm"
            || tail
                .iter()
                .take(2)
                .any(|w| matches!(*w, "pm" | "chiều" | "tối" | "trưa"));
        if pm && hour < 12 && !(hour == 11 && tail.contains(&"trưa")) {
            hour += 12;
        }
        if hour < 24 && minute < 60 {
            let surface = crate::text::char_slice(text, toks[i].span.0, toks[end].span.1);
            out.push((hour, minute, surface));
        }
    }
    out
}

fn attendee_status<'a>(
    ev: &'a CalendarEvent,
    name: &str,
) -> Option<&'a almond_types::calendar::Attendee> {
    ev.attendees.iter().find(|a| {
        a.name.as_deref().is_some_and(|n| mentions(n, name))
            || a.email.as_deref().is_some_and(|e| {
                let local = e.split('@').next().unwrap_or("").replace(['.', '_'], " ");
                mentions(&local, name) || fold(&local) == fold(name)
            })
    })
}

#[async_trait]
impl Verifier for CalendarVerifier {
    fn descriptor(&self) -> VerifierDescriptor {
        VerifierDescriptor {
            name: NAME.into(),
            version: "1".into(),
            family: "symbolic".into(),
            logical_forms: vec![],
            description: "Claims about the meeting's time, date and attendees are checked against \
                          the calendar event and the transcript's speakers."
                .into(),
        }
    }

    async fn verify(
        &self,
        claims: &[Claim],
        ctx: &VerifyContext,
    ) -> almond_types::Result<Vec<Verdict>> {
        let Some(ev) = &ctx.calendar_event else {
            return Ok(vec![]);
        };
        let idx = EvidenceIndex::new(ctx, self.config.tz_offset_min);
        let offset = match self.config.tz_offset_min {
            Some(m) => FixedOffset::east_opt(m * 60),
            None => chrono::Local
                .timestamp_millis_opt(ev.start)
                .single()
                .map(|d| *d.offset()),
        }
        .unwrap_or(FixedOffset::east_opt(0).unwrap());
        let start = offset.timestamp_millis_opt(ev.start).single();
        let start_day = dates::reference_day(ev.start, Some(offset.local_minus_utc() / 60));
        let speakers_who_spoke: Vec<String> = idx
            .all_segments()
            .into_iter()
            .filter_map(|s| s.speaker)
            .collect();
        let mut out = Vec::new();
        for c in claims {
            let n = norm(&c.text);
            let about_meeting = has_phrase(&n, MEETING_WORDS);
            let neg = has_phrase(&n, NEG_ATTEND);
            let pos = !neg && has_phrase(&n, POS_ATTEND);
            let mut statuses = Vec::new();
            let mut checks = Vec::new();
            let mut msgs = Vec::new();

            if neg || pos {
                for nm in names::extract(&c.text, &idx.people) {
                    let a = attendee_status(ev, &nm.surface);
                    let spoke = speakers_who_spoke.iter().any(|s| mentions(s, &nm.surface));
                    let declined = a.and_then(|a| a.response.as_deref()) == Some("declined");
                    let status = match (pos, a, declined, spoke) {
                        (true, _, _, true) => AtomStatus::Match,
                        (true, Some(_), false, _) => AtomStatus::Match,
                        (true, Some(_), true, _) => AtomStatus::Conflict,
                        (true, None, _, _) => AtomStatus::Absent,
                        (false, _, _, true) => AtomStatus::Conflict,
                        (false, _, true, _) | (false, None, _, _) => AtomStatus::Match,
                        (false, Some(_), false, false) => AtomStatus::Absent,
                    };
                    if status != AtomStatus::Match {
                        msgs.push(match (pos, status) {
                            (true, AtomStatus::Conflict) => {
                                format!("{} declined the calendar invite", nm.surface)
                            }
                            (true, _) => format!("{} is not on the invite", nm.surface),
                            (false, AtomStatus::Conflict) => {
                                format!("{} speaks in the transcript, so was present", nm.surface)
                            }
                            _ => format!(
                                "{} accepted the invite; absence cannot be confirmed",
                                nm.surface
                            ),
                        });
                    }
                    statuses.push(status);
                    checks.push(json!({"check": "attendance", "name": nm.surface,
                        "claim_present": pos, "on_invite": a.is_some(), "declined": declined,
                        "spoke": spoke, "status": status}));
                }
                // "5 người tham dự" / "five people attended"
                let toks = tokenize(&c.text);
                for q in crate::numbers::extract(&c.text) {
                    let after: Vec<&str> = toks
                        .iter()
                        .filter(|t| t.span.0 >= q.span.1)
                        .take(2)
                        .map(|t| t.folded.as_str())
                        .collect();
                    let one = after.first().copied().unwrap_or("");
                    let two = after.join(" ");
                    if !PEOPLE_WORDS.iter().any(|w| *w == one || *w == two) {
                        continue;
                    }
                    let expected = ev
                        .attendees
                        .iter()
                        .filter(|a| a.response.as_deref() != Some("declined"))
                        .count();
                    let ok = (q.value - expected as f64).abs() < 0.5;
                    let status = if ok {
                        AtomStatus::Match
                    } else {
                        AtomStatus::Conflict
                    };
                    if !ok {
                        msgs.push(format!(
                            "claim says {} attendees, calendar has {expected} (not declined)",
                            q.surface
                        ));
                    }
                    statuses.push(status);
                    checks.push(json!({"check": "attendee_count", "claim": q.value,
                        "calendar": expected, "status": status}));
                }
            }
            // "họp lại tuần sau lúc 3h" talks about a *future* meeting, not this one.
            let future = dates::extract(&c.text, idx.reference_day)
                .iter()
                .any(|d| d.value.first_day() > start_day);
            if about_meeting && !future {
                if let Some(start) = start {
                    for (h, m, surface) in times_of_day(&c.text) {
                        let ok = start.hour() == h && start.minute() == m;
                        let status = if ok {
                            AtomStatus::Match
                        } else {
                            AtomStatus::Conflict
                        };
                        if !ok {
                            msgs.push(format!(
                                "claim says the meeting was at {surface}, calendar says {:02}:{:02}",
                                start.hour(),
                                start.minute()
                            ));
                        }
                        statuses.push(status);
                        checks.push(json!({"check": "start_time", "claim": format!("{h:02}:{m:02}"),
                            "calendar": format!("{:02}:{:02}", start.hour(), start.minute()), "status": status}));
                    }
                }
                for d in dates::extract(&c.text, idx.reference_day) {
                    if d.qualifier != Qualifier::On {
                        continue;
                    }
                    let probe = dates::DateExpr {
                        value: dates::DateVal::Day { date: start_day },
                        ..d.clone()
                    };
                    let status = match d.compare(&probe) {
                        dates::Match::Exact => AtomStatus::Match,
                        dates::Match::Partial => AtomStatus::Partial,
                        dates::Match::Conflict => AtomStatus::Conflict,
                    };
                    if status == AtomStatus::Conflict {
                        msgs.push(format!(
                            "claim dates the meeting \"{}\", calendar says {start_day}",
                            d.surface
                        ));
                    }
                    statuses.push(status);
                    checks.push(json!({"check": "meeting_date", "claim": d, "calendar": start_day, "status": status}));
                }
            }
            if statuses.is_empty() {
                continue;
            }
            let (label, conf) = label_for(&statuses);
            let explanation = if msgs.is_empty() {
                "Consistent with the calendar event.".to_string()
            } else {
                capitalize(&msgs.join("; ")) + "."
            };
            out.push(verdict(
                ctx,
                c,
                NAME,
                label,
                conf,
                vec![],
                explanation,
                Some(json!({"rule": "calendar.v1",
                    "event": {"source": ev.source, "event_id": ev.event_id, "start": ev.start,
                              "attendees": ev.attendees.len()},
                    "checks": checks})),
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_times() {
        let t = |s: &str| {
            times_of_day(s)
                .into_iter()
                .map(|(h, m, _)| (h, m))
                .collect::<Vec<_>>()
        };
        assert_eq!(t("Cuộc họp bắt đầu lúc 10h"), [(10, 0)]);
        assert_eq!(t("họp lúc 10h30 sáng"), [(10, 30)]);
        assert_eq!(t("meeting at 3pm"), [(15, 0)]);
        assert_eq!(t("lúc 3 giờ chiều"), [(15, 0)]);
        assert_eq!(t("started at 14:30"), [(14, 30)]);
        assert!(t("200 triệu").is_empty());
    }
}
