//! `mail.context`: recent Gmail threads with a calendar event's attendees, through the Google
//! accounts that granted `gmail.readonly` (optional per account, `calendar.addGoogle { gmail }`).

use super::add;
use super::calendar::{gmail_source, google_accounts};
use crate::Core;
use crate::error::ApiResult;
use crate::router::Router;
use almond_types::mail::{MailSource, MailThreadSummary};
use almond_types::meeting::EventRef;
use almond_types::{Error, Result};
use serde::Deserialize;
use std::collections::BTreeSet;

/// Threads asked from each account, and returned in total.
const PER_ACCOUNT: u32 = 10;
const MAX_TOTAL: usize = 20;

#[derive(Deserialize)]
struct ContextParams {
    event: EventRef,
}

/// Empty when the event has no other attendees or no account has Gmail enabled.
async fn context(core: &Core, r: &EventRef) -> Result<Vec<MailThreadSummary>> {
    let ev = core
        .store()
        .calendar_event(&r.source, &r.event_id)?
        .ok_or_else(|| Error::NotFound(format!("calendar event {}/{}", r.source, r.event_id)))?;
    let accounts = google_accounts(core)?;
    let mine: BTreeSet<String> = accounts
        .iter()
        .map(|a| a.email.to_ascii_lowercase())
        .chain(
            ev.attendees
                .iter()
                .filter(|a| a.is_self)
                .filter_map(|a| a.email.clone()),
        )
        .collect();
    let emails: Vec<String> = ev
        .attendees
        .iter()
        .filter(|a| !a.is_self)
        .filter_map(|a| a.email.clone())
        .chain(ev.organizer.clone())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| !mine.contains(e))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if emails.is_empty() {
        return Ok(vec![]);
    }
    let with_gmail: Vec<String> = accounts
        .into_iter()
        .filter(|a| a.gmail_enabled())
        .map(|a| a.email)
        .collect();
    // The event's own account first; otherwise every account with Gmail.
    let owner = r
        .source
        .strip_prefix("google:")
        .map(str::to_ascii_lowercase);
    let use_accounts: Vec<String> = match owner {
        Some(o) if with_gmail.iter().any(|a| a.eq_ignore_ascii_case(&o)) => vec![o],
        _ => with_gmail,
    };
    let mut out = vec![];
    let mut first_err = None;
    let n = use_accounts.len();
    for acct in use_accounts {
        let res = match gmail_source(core, &acct) {
            Ok(src) => src.threads_with(&emails, PER_ACCOUNT).await,
            Err(e) => Err(e),
        };
        match res {
            Ok(t) => out.extend(t),
            Err(e) => {
                tracing::warn!("mail.context via {acct}: {e}");
                first_err.get_or_insert(e);
            }
        }
    }
    if let Some(e) = first_err
        && out.is_empty()
        && n > 0
    {
        return Err(e);
    }
    out.sort_by_key(|t| std::cmp::Reverse(t.last_message_at));
    out.truncate(MAX_TOTAL);
    Ok(out)
}

pub(super) fn register(r: &mut Router) {
    add(r, "mail.context", |core, p: ContextParams| async move {
        ApiResult::Ok(context(&core, &p.event).await?)
    });
}
