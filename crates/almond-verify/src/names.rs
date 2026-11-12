//! Heuristic named-entity (mostly person) detection for Vietnamese + English meeting text.
//!
//! A name is a run of capitalized words that is not a known common word. Sentence-initial runs
//! (where everything is capitalized) only count when they are a known person, follow an honorific
//! ("anh Minh", "chị Lan", "Mr Smith"), or are followed by a typical "person does X" verb.

use crate::text::{TokKind, Token, fold, tokenize};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameMention {
    pub surface: String,
    pub span: (usize, usize),
    /// True when the name matched a known person (speaker / attendee).
    pub known_person: bool,
}

const HONORIFICS: &[&str] = &[
    "anh", "chị", "em", "ông", "bà", "cô", "chú", "bạn", "sếp", "mr", "ms", "mrs", "dr",
];

/// Common words that are capitalized at sentence start or in headings (lowercase, diacritics kept).
const STOP: &[&str] = &[
    // vi
    "ngân",
    "tuyển",
    "nhóm",
    "cả",
    "họ",
    "chúng",
    "mọi",
    "quyết",
    "kế",
    "dự",
    "ngày",
    "thứ",
    "tháng",
    "hôm",
    "tôi",
    "mình",
    "khách",
    "công",
    "sản",
    "dữ",
    "hệ",
    "phòng",
    "ban",
    "đội",
    "bản",
    "báo",
    "hợp",
    "buổi",
    "cuộc",
    "việc",
    "các",
    "những",
    "một",
    "hai",
    "ba",
    "sau",
    "trước",
    "trong",
    "khi",
    "nếu",
    "vì",
    "do",
    "để",
    "đã",
    "sẽ",
    "cần",
    "phải",
    "không",
    "chưa",
    "có",
    "là",
    "và",
    "hoặc",
    "nhưng",
    "theo",
    "về",
    "với",
    "cho",
    "tại",
    "từ",
    "đến",
    "giám",
    "trưởng",
    "chi",
    "doanh",
    "lợi",
    "giá",
    "chiến",
    "mục",
    "tiêu",
    "đề",
    "xuất",
    "thống",
    "nhất",
    "đồng",
    "ý",
    "kỹ",
    "sư",
    "ứng",
    "viên",
    "nhân",
    "sự",
    "tài",
    "chính",
    "kết",
    "quả",
    "vấn",
    "giải",
    "pháp",
    "kiểm",
    "thử",
    "phát",
    "triển",
    "bảo",
    "mật",
    "khai",
    "ra",
    "mắt",
    "chào",
    "cảm",
    "ơn",
    "ok",
    "hạn",
    "chót",
    "lịch",
    "họp",
    "tổng",
    "kết",
    "hoàn",
    "thành",
    "gửi",
    "chuẩn",
    "bị",
    "tăng",
    "giảm",
    "chuyển",
    "sang",
    "dùng",
    "sử",
    "giữ",
    "nguyên",
    "hoãn",
    "lùi",
    "bắt",
    "đầu",
    "cuối",
    "tuần",
    "năm",
    "quý",
    "chủ",
    "nhật",
    "sáu",
    "bảy",
    "tư",
    "mười",
    "người",
    "tham",
    "dự",
    "vắng",
    "mặt",
    "thông",
    "qua",
    "phê",
    "duyệt",
    "chốt",
    "nhắc",
    "lại",
    "đánh",
    "giá",
    "rủi",
    "ro",
    "ưu",
    "tiên",
    "hiện",
    "tại",
    "tiếp",
    "tục",
    "thời",
    "gian",
    "điểm",
    "chính",
    "hành",
    "động",
    // en
    "the",
    "a",
    "an",
    "we",
    "they",
    "team",
    "our",
    "he",
    "she",
    "it",
    "i",
    "budget",
    "marketing",
    "sales",
    "hire",
    "hiring",
    "launch",
    "decision",
    "decided",
    "decisions",
    "action",
    "actions",
    "items",
    "next",
    "all",
    "everyone",
    "both",
    "each",
    "this",
    "that",
    "these",
    "there",
    "customer",
    "client",
    "product",
    "project",
    "meeting",
    "agreed",
    "approve",
    "approved",
    "plan",
    "ok",
    "yes",
    "no",
    "also",
    "and",
    "but",
    "if",
    "when",
    "after",
    "before",
    "by",
    "on",
    "in",
    "for",
    "to",
    "of",
    "with",
    "at",
    "from",
    "engineering",
    "design",
    "legal",
    "finance",
    "summary",
    "notes",
    "key",
    "points",
    "follow",
    "up",
    "send",
    "move",
    "keep",
    "use",
    "switch",
    "postpone",
    "start",
    "review",
    "attendees",
    "attended",
    "discussed",
    "discussion",
    "risks",
    "risk",
    "open",
    "questions",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "q",
    "backend",
    "frontend",
    "ceo",
    "cto",
    "cfo",
    "vp",
    "hr",
];

/// Verbs that typically follow a person subject ("Lan sẽ gửi", "Minh will send").
const PERSON_VERBS: &[&str] = &[
    "sẽ",
    "gửi",
    "phụ",
    "chịu",
    "lo",
    "làm",
    "cần",
    "phải",
    "đã",
    "đề",
    "báo",
    "trình",
    "nói",
    "đồng",
    "thuyết",
    "chuẩn",
    "liên",
    "cập",
    "viết",
    "kiểm",
    "tổng",
    "xây",
    "hoàn",
    "nhận",
    "hỏi",
    "trả",
    "dẫn",
    "quản",
    "review",
    "will",
    "to",
    "sends",
    "owns",
    "is",
    "has",
    "needs",
    "said",
    "says",
    "agreed",
    "presented",
    "proposed",
    "asked",
    "handles",
    "takes",
    "leads",
    "wrote",
    "prepares",
    "schedules",
    "checks",
    "drafts",
    "follows",
    "updates",
    "confirmed",
    "and",
    "và",
    ":",
    "(",
    "attended",
    "joined",
    "tham",
    "vắng",
    "owner",
];

fn is_stop(t: &Token) -> bool {
    STOP.contains(&t.lower.as_str())
}

fn is_acronym(t: &Token) -> bool {
    let n = t.raw.chars().count();
    n == 1
        || (n >= 2
            && t.raw
                .chars()
                .all(|c| c.is_uppercase() || c.is_ascii_digit()))
}

fn sentence_start(toks: &[Token], i: usize) -> bool {
    i == 0
        || matches!(
            toks[i - 1].raw.as_str(),
            "." | "!" | "?" | ":" | "-" | "•" | "*" | "–" | "—" | "\"" | "(" | ";"
        )
}

/// Find name mentions in `text`. `known` are known people (folded comparison).
pub fn extract(text: &str, known: &[String]) -> Vec<NameMention> {
    let toks = tokenize(text);
    let chars: Vec<char> = text.chars().collect();
    let known_seqs: Vec<Vec<String>> = known
        .iter()
        .map(|k| crate::text::folded_words(k))
        .filter(|k| !k.is_empty())
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        let honorific_before = i > 0
            && toks[i - 1].kind == TokKind::Word
            && HONORIFICS.contains(&toks[i - 1].lower.as_str());
        if !t.is_capitalized() || is_acronym(t) {
            i += 1;
            continue;
        }
        // Capitalized honorific: "Anh Minh ..." -> skip the honorific itself.
        if HONORIFICS.contains(&t.lower.as_str())
            && toks
                .get(i + 1)
                .is_some_and(|n| n.is_capitalized() && !is_stop(n))
        {
            i += 1;
            continue;
        }
        // Collect a run of capitalized, non-acronym words (max 4).
        let mut j = i;
        while j < toks.len() && j - i < 4 && toks[j].is_capitalized() && !is_acronym(&toks[j]) {
            if j > i && is_stop(&toks[j]) {
                break;
            }
            j += 1;
        }
        let run: Vec<&Token> = toks[i..j].iter().collect();
        let folded: Vec<String> = run.iter().map(|t| t.folded.clone()).collect();
        let known_person = known_seqs.iter().any(|k| {
            crate::text::contains_seq(k, &folded) || crate::text::contains_seq(&folded, k)
        });
        let prev_date_word = i > 0 && matches!(toks[i - 1].lower.as_str(), "thứ" | "tháng");
        let all_stop = run.iter().all(|t| is_stop(t));
        let followed_by_verb = toks
            .get(j)
            .is_some_and(|n| PERSON_VERBS.contains(&n.lower.as_str()));
        let accept = if prev_date_word {
            false
        } else if known_person || honorific_before {
            true
        } else if all_stop {
            false
        } else if sentence_start(&toks, i) {
            followed_by_verb
        } else {
            true
        };
        if accept {
            let span = (run[0].span.0, run[run.len() - 1].span.1);
            out.push(NameMention {
                surface: chars[span.0..span.1].iter().collect(),
                span,
                known_person,
            });
        }
        i = j.max(i + 1);
    }
    out
}

/// Folded key for comparing names.
pub fn key(name: &str) -> String {
    fold(name).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Titles dropped when comparing two names ("Cr Okafor" vs "Cr David Okafor", "anh Minh" vs
/// "Minh"); folded.
const TITLES: &[&str] = &[
    "anh",
    "chi",
    "em",
    "ong",
    "ba",
    "co",
    "chu",
    "ban",
    "sep",
    "mr",
    "ms",
    "mrs",
    "dr",
    "cr",
    "cllr",
    "councillor",
    "mayor",
    "prof",
    "sir",
    "madam",
    "mx",
];

/// Folded name words without titles.
fn core_words(name: &str) -> Vec<String> {
    crate::text::folded_words(name)
        .into_iter()
        .filter(|w| !TITLES.contains(&w.as_str()))
        .collect()
}

/// Same person, allowing titles and partial names: every non-title word of the shorter name is
/// in the longer one ("Cr Okafor" ~ "Cr David Okafor", "Minh" ~ "Nguyễn Văn Minh").
pub fn same_person(a: &str, b: &str) -> bool {
    let (a, b) = (core_words(a), core_words(b));
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    !short.is_empty() && short.iter().all(|w| long.contains(w))
}

/// Folded group words used as owners: they are not people, so "not an attendee" doesn't apply.
const GROUPS: &[&str] = &[
    "staff",
    "team",
    "everyone",
    "all",
    "officers",
    "officer",
    "council",
    "management",
    "committee",
    "board",
    "we",
    "us",
    "they",
    "nhom",
    "moi nguoi",
    "ca nhom",
    "doi",
    "ban",
    "phong",
    "cong ty",
    "chung ta",
    "chung toi",
    "ho",
    "tat ca",
];

/// "Staff", "the team", "nhóm", "mọi người": a group, not a named person.
pub fn is_group(name: &str) -> bool {
    let w: Vec<String> = crate::text::folded_words(name)
        .into_iter()
        .filter(|w| !matches!(w.as_str(), "the" | "our" | "cac" | "nhung"))
        .collect();
    let k = w.join(" ");
    !k.is_empty()
        && (GROUPS.contains(&k.as_str())
            || w.first().is_some_and(|f| {
                matches!(f.as_str(), "nhom" | "doi" | "phong" | "ban") && w.len() <= 3
            })
            || w.last()
                .is_some_and(|l| matches!(l.as_str(), "team" | "staff" | "committee")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(s: &str, known: &[&str]) -> Vec<String> {
        let k: Vec<String> = known.iter().map(|s| s.to_string()).collect();
        extract(s, &k).into_iter().map(|n| n.surface).collect()
    }

    #[test]
    fn same_person_ignores_titles_and_partial_names() {
        assert!(same_person("Cr Okafor", "Cr David Okafor"));
        assert!(same_person("anh Minh", "Nguyễn Văn Minh"));
        assert!(same_person("MINH", "Minh"));
        assert!(!same_person("Cr Okafor", "Mayor Helen Nguyen"));
        assert!(!same_person("Mr", "Mr Smith"));
    }

    #[test]
    fn groups() {
        for g in [
            "Staff",
            "the team",
            "Nhóm",
            "mọi người",
            "Nhóm marketing",
            "Sales team",
        ] {
            assert!(is_group(g), "{g}");
        }
        for p in ["Lan", "Cr Okafor", "Minh"] {
            assert!(!is_group(p), "{p}");
        }
    }

    #[test]
    fn vietnamese_names() {
        assert_eq!(names("Lan gửi kế hoạch trước thứ Sáu.", &[]), ["Lan"]);
        assert_eq!(
            names("Ngân sách marketing Q4 là 300 triệu đồng.", &[]),
            Vec::<String>::new()
        );
        assert_eq!(names("Giao cho anh Minh chuẩn bị demo.", &[]), ["Minh"]);
        assert_eq!(names("Anh Minh chuẩn bị demo.", &[]), ["Minh"]);
        assert_eq!(
            names("Tuyển 2 kỹ sư backend trước tháng 12.", &[]),
            Vec::<String>::new()
        );
        assert_eq!(
            names("Chị Nguyễn Thị Hoa phụ trách", &[]),
            ["Nguyễn Thị Hoa"]
        );
        assert_eq!(names("Hoa phê duyệt", &["Hoa"]), ["Hoa"]);
    }

    #[test]
    fn english_names() {
        assert_eq!(names("Sarah will send the deck by Friday.", &[]), ["Sarah"]);
        assert_eq!(
            names("The team agreed to move to Postgres.", &[]),
            ["Postgres"]
        );
        assert_eq!(names("Budget is approved.", &[]), Vec::<String>::new());
        assert_eq!(
            names("Action items: follow up with David Chen", &[]),
            ["David Chen"]
        );
    }
}
