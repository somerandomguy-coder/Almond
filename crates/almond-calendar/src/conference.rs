//! Video-call link detection (Google Meet, Zoom, Teams, Webex, …) in free text.

use regex::Regex;
use std::sync::OnceLock;

pub const GOOGLE_MEET: &str = "google_meet";
pub const ZOOM: &str = "zoom";
pub const TEAMS: &str = "teams";
pub const WEBEX: &str = "webex";

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"https?://[^\s<>"'()\[\]{}]+"#).expect("regex"))
}

/// Classify a URL by host. Returns `None` for non-conference links.
pub fn classify_url(url: &str) -> Option<&'static str> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let path = parsed.path();
    let is = |d: &str| host == d || host.ends_with(&format!(".{d}"));
    if host == "meet.google.com" && path.len() > 1 {
        Some(GOOGLE_MEET)
    } else if (is("zoom.us") || is("zoomgov.com"))
        && (path.starts_with("/j/") || path.starts_with("/my/") || path.starts_with("/w/"))
    {
        Some(ZOOM)
    } else if (host == "teams.microsoft.com" && path.contains("meetup-join"))
        || (host == "teams.live.com" && path.starts_with("/meet"))
        || (host == "teams.microsoft.com" && path.starts_with("/meet"))
    {
        Some(TEAMS)
    } else if is("webex.com")
        && (path.contains("/j.php") || path.starts_with("/meet") || path.contains("/join"))
    {
        Some(WEBEX)
    } else {
        None
    }
}

/// First conference link found in `text` (HTML or plain), with its app.
pub fn detect_in_text(text: &str) -> Option<(String, &'static str)> {
    for m in url_re().find_iter(text) {
        let raw = m
            .as_str()
            .replace("&amp;", "&")
            .trim_end_matches(['.', ',', ';', ':', '!', '?'])
            .to_string();
        if let Some(app) = classify_url(&raw) {
            return Some((raw, app));
        }
    }
    None
}

/// Search several optional texts in priority order.
pub fn detect_in<'a>(
    texts: impl IntoIterator<Item = Option<&'a str>>,
) -> Option<(String, &'static str)> {
    texts.into_iter().flatten().find_map(detect_in_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_links() {
        let cases = [
            ("Join: https://meet.google.com/abc-defg-hij", GOOGLE_MEET),
            (
                "<a href=\"https://us02web.zoom.us/j/123456789?pwd=xyz\">Zoom</a>",
                ZOOM,
            ),
            (
                "Click https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0?context=%7b%7d.",
                TEAMS,
            ),
            ("https://teams.live.com/meet/9312345", TEAMS),
            ("https://acme.webex.com/acme/j.php?MTID=m1", WEBEX),
        ];
        for (text, app) in cases {
            let (url, got) = detect_in_text(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(got, app, "{text}");
            assert!(!url.ends_with('.'));
        }
    }

    #[test]
    fn ignores_other_links() {
        assert!(
            detect_in_text("see https://zoom.us/pricing and https://example.com/j/1").is_none()
        );
        assert!(detect_in_text("Room 4, building B").is_none());
        let html = "x https://zoom.us/j/1?pwd=a&amp;b=2 y";
        assert_eq!(
            detect_in_text(html).unwrap().0,
            "https://zoom.us/j/1?pwd=a&b=2"
        );
    }

    #[test]
    fn priority_order() {
        let r = detect_in([
            None,
            Some("https://zoom.us/j/9"),
            Some("https://meet.google.com/x-y-z"),
        ]);
        assert_eq!(r.unwrap().1, ZOOM);
    }
}
