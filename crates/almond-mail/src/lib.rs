//! # almond-mail — Gmail thread context (WS-E5), read-only
//!
//! [`GmailSource`] implements [`MailSource`]: "recent threads with these people", used as
//! meeting context. It needs the optional `gmail.readonly` scope, granted per account at sign-in
//! (`AddAccountOptions { gmail: true, .. }` in `almond_calendar::oauth`). Drafting follow-ups
//! is a later `Action`, not part of this crate.
//!
//! Query: Gmail search `{from:a to:a cc:a from:b …}` (braces = OR), newest first, then
//! `threads.get?format=metadata` per thread for subject / participants / last message time.

pub use almond_types as types;

use almond_calendar::oauth::{AccessTokenProvider, GoogleAuth, authorized_get, json_or_error};
use almond_types::mail::{MailSource, MailThreadSummary};
use almond_types::{Error, Result};
use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub const DEFAULT_API_BASE: &str = "https://gmail.googleapis.com/gmail/v1";
/// Upper bound on threads fetched per call (each costs one `threads.get`).
pub const MAX_THREADS: u32 = 25;

pub struct GmailSource {
    account: String,
    auth: Arc<dyn AccessTokenProvider>,
    http: reqwest::Client,
    api_base: String,
}

impl GmailSource {
    pub fn new(account: impl Into<String>, auth: Arc<dyn AccessTokenProvider>) -> Self {
        GmailSource {
            account: account.into().to_ascii_lowercase(),
            auth,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            api_base: DEFAULT_API_BASE.into(),
        }
    }

    /// Build for a connected account, checking that Gmail access was granted.
    pub fn for_account(auth: Arc<GoogleAuth>, email: &str) -> Result<Self> {
        let acct = auth
            .account(email)?
            .ok_or_else(|| Error::NotFound(format!("Google account {email} is not connected")))?;
        if !acct.gmail_enabled() {
            return Err(Error::PermissionDenied(format!(
                "Gmail is not enabled for {email}; re-add the account with Gmail ticked"
            )));
        }
        Ok(Self::new(acct.email, auth))
    }

    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_http(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    fn url(&self, segments: &[&str]) -> Result<url::Url> {
        let mut u = url::Url::parse(&self.api_base)
            .map_err(|e| Error::Invalid(format!("gmail url: {e}")))?;
        u.path_segments_mut()
            .map_err(|_| Error::Invalid("gmail api base cannot be a base".into()))?
            .pop_if_empty()
            .extend(segments);
        Ok(u)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, url: url::Url) -> Result<T> {
        let resp = authorized_get(&self.http, self.auth.as_ref(), &self.account, url).await?;
        json_or_error(resp).await.map_err(|e| match e {
            Error::PermissionDenied(m) => Error::PermissionDenied(format!(
                "{m} (is the Gmail API enabled and gmail.readonly granted?)"
            )),
            other => other,
        })
    }

    /// Browser deep link to a thread for this account.
    pub fn thread_url(&self, thread_id: &str) -> String {
        format!(
            "https://mail.google.com/mail/u/{}/#all/{}",
            self.account, thread_id
        )
    }
}

/// Gmail search query for threads involving any of `emails`; `None` if no valid address.
pub fn build_query(emails: &[String]) -> Option<String> {
    let addrs: BTreeSet<String> = emails
        .iter()
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| is_plain_email(e))
        .collect();
    if addrs.is_empty() {
        return None;
    }
    let terms: Vec<String> = addrs
        .iter()
        .flat_map(|a| [format!("from:{a}"), format!("to:{a}"), format!("cc:{a}")])
        .collect();
    Some(format!("{{{}}}", terms.join(" ")))
}

/// Only plain addresses: no spaces, quotes, braces or other search operators can sneak in.
fn is_plain_email(e: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9._%+\-]+@[a-z0-9\-]+(\.[a-z0-9\-]+)+$").expect("regex"))
        .is_match(e)
}

fn email_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(\.[A-Za-z0-9\-]+)+").expect("regex")
    })
}

fn decode_entities(s: &str) -> String {
    s.replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[async_trait]
impl MailSource for GmailSource {
    fn account(&self) -> &str {
        &self.account
    }

    async fn threads_with(&self, emails: &[String], limit: u32) -> Result<Vec<MailThreadSummary>> {
        let Some(q) = build_query(emails) else {
            return Ok(Vec::new());
        };
        let limit = limit.clamp(1, MAX_THREADS);
        let mut u = self.url(&["users", "me", "threads"])?;
        u.query_pairs_mut()
            .append_pair("q", &q)
            .append_pair("maxResults", &limit.to_string());
        let list: ThreadList = self.get(u).await?;

        let mut out = Vec::new();
        for t in list.threads.into_iter().take(limit as usize) {
            let mut u = self.url(&["users", "me", "threads", &t.id])?;
            {
                let mut qp = u.query_pairs_mut();
                qp.append_pair("format", "metadata");
                for h in ["From", "To", "Cc", "Subject"] {
                    qp.append_pair("metadataHeaders", h);
                }
            }
            let th: Thread = match self.get(u).await {
                Ok(t) => t,
                // Thread deleted between list and get.
                Err(Error::NotFound(_)) => continue,
                Err(e) => return Err(e),
            };
            out.push(summarize(self, &t, th));
        }
        out.sort_by_key(|t| std::cmp::Reverse(t.last_message_at));
        Ok(out)
    }
}

fn summarize(src: &GmailSource, listed: &ThreadRef, th: Thread) -> MailThreadSummary {
    let mut participants: BTreeSet<String> = BTreeSet::new();
    let mut subject = None;
    let mut last = 0i64;
    for m in &th.messages {
        last = last.max(
            m.internal_date
                .as_deref()
                .and_then(|d| d.parse().ok())
                .unwrap_or(0),
        );
        for h in m.payload.iter().flat_map(|p| p.headers.iter()) {
            match h.name.to_ascii_lowercase().as_str() {
                "subject" if subject.is_none() => subject = Some(h.value.clone()),
                "from" | "to" | "cc" => {
                    for m in email_re().find_iter(&h.value) {
                        participants.insert(m.as_str().to_ascii_lowercase());
                    }
                }
                _ => {}
            }
        }
    }
    participants.remove(&src.account);
    let snippet = listed
        .snippet
        .clone()
        .or_else(|| th.messages.last().and_then(|m| m.snippet.clone()))
        .unwrap_or_default();
    MailThreadSummary {
        account: src.account.clone(),
        thread_id: th.id.clone(),
        subject: subject
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "(no subject)".into()),
        participants: participants.into_iter().collect(),
        snippet: decode_entities(&snippet),
        last_message_at: last,
        url: Some(src.thread_url(&th.id)),
    }
}

// ---------------------------------------------------------------- wire types

#[derive(Debug, Deserialize)]
struct ThreadList {
    #[serde(default)]
    threads: Vec<ThreadRef>,
}

#[derive(Debug, Deserialize)]
struct ThreadRef {
    id: String,
    snippet: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Thread {
    id: String,
    #[serde(default)]
    messages: Vec<Message>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Message {
    internal_date: Option<String>,
    snippet: Option<String>,
    payload: Option<Payload>,
}

#[derive(Debug, Deserialize)]
struct Payload {
    #[serde(default)]
    headers: Vec<Header>,
}

#[derive(Debug, Deserialize)]
struct Header {
    name: String,
    value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_is_sanitized() {
        let q = build_query(&[
            "Bob@Example.com".into(),
            "evil@x.com OR subject:secret".into(),
            "{x}@y.com".into(),
            "bob@example.com".into(),
        ])
        .unwrap();
        assert_eq!(
            q,
            "{from:bob@example.com to:bob@example.com cc:bob@example.com}"
        );
        assert!(build_query(&["not an email".into()]).is_none());
        assert!(build_query(&[]).is_none());
    }

    #[test]
    fn entities() {
        assert_eq!(decode_entities("it&#39;s &lt;ok&gt; &amp;"), "it's <ok> &");
    }
}
