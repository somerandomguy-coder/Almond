//! Parsing the user's plain-text / Markdown-ish notes into blocks. The text of every user block is
//! kept verbatim (only the list/heading marker is turned into `style`/`level`).

/// One user-written line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserLine {
    /// "heading" | "bullet" | "todo" | "paragraph"
    pub style: String,
    pub level: u8,
    pub text: String,
}

/// Split notes into lines/blocks. Empty lines are dropped.
pub fn parse_user_notes(s: &str) -> Vec<UserLine> {
    let mut out = vec![];
    for raw in s.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        let indent: usize = raw
            .chars()
            .take_while(|c| c.is_whitespace())
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum();
        let level = (indent / 2).min(4) as u8;
        let t = raw.trim();
        let (style, text) = if t.starts_with('#') {
            ("heading", t.trim_start_matches('#').trim())
        } else if let Some(rest) = strip_todo(t) {
            ("todo", rest)
        } else if let Some(rest) = ["- ", "* ", "• ", "+ ", "– "]
            .iter()
            .find_map(|p| t.strip_prefix(p))
        {
            ("bullet", rest.trim())
        } else {
            ("paragraph", t)
        };
        out.push(UserLine {
            style: style.into(),
            level: if style == "heading" { 0 } else { level },
            text: text.to_string(),
        });
    }
    out
}

fn strip_todo(t: &str) -> Option<&str> {
    for p in [
        "- [ ] ", "- [x] ", "- [X] ", "* [ ] ", "* [x] ", "[ ] ", "[x] ", "☐ ", "☑ ",
    ] {
        if let Some(r) = t.strip_prefix(p) {
            return Some(r.trim());
        }
    }
    None
}

/// `(n0) text` lines for the prompt, or `(no notes)`.
pub fn render_for_prompt(lines: &[UserLine]) -> String {
    if lines.is_empty() {
        return "(no notes)".into();
    }
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let marker = match l.style.as_str() {
                "heading" => "# ",
                "bullet" => "- ",
                "todo" => "[ ] ",
                _ => "",
            };
            format!("(n{i}) {}{marker}{}", "  ".repeat(l.level as usize), l.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_styles() {
        let n = parse_user_notes(
            "# Ngân sách\n- marketing 200tr\n  - hỏi lại Lan\n[ ] gửi plan\n\nghi chú tự do",
        );
        let s: Vec<(&str, u8, &str)> = n
            .iter()
            .map(|l| (l.style.as_str(), l.level, l.text.as_str()))
            .collect();
        assert_eq!(
            s,
            vec![
                ("heading", 0, "Ngân sách"),
                ("bullet", 0, "marketing 200tr"),
                ("bullet", 1, "hỏi lại Lan"),
                ("todo", 0, "gửi plan"),
                ("paragraph", 0, "ghi chú tự do"),
            ]
        );
        let r = render_for_prompt(&n);
        assert!(r.starts_with("(n0) # Ngân sách\n(n1) - marketing 200tr\n(n2)   - hỏi lại Lan"));
        assert_eq!(render_for_prompt(&[]), "(no notes)");
    }
}
