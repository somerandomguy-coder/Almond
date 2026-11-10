//! Google Calendar + Gmail through the core, against a mock Google (wiremock): browser sign-in
//! (loopback + PKCE), polling into `calendar.upcoming`, all-day events in the calendar's zone,
//! the 410 full resync dropping stale events (E3), `mail.context`, and removing the account.
//! A real sign-in needs the user's browser and OAuth client; see docs/google-setup.md.

mod common;

use almond_core::Api;
use common::open_core;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use wiremock::matchers::{body_string_contains, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ME: &str = "me@example.com";

async fn call(api: &Api, m: &str, p: Value) -> Value {
    match api.call(m, p.clone()).await {
        Ok(v) => v,
        Err(e) => panic!("{m}({p}) failed: {e}"),
    }
}

fn rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn timed(id: &str, title: &str, start: i64, extra: Value) -> Value {
    let mut v = json!({
        "id": id, "status": "confirmed", "summary": title,
        "start": { "dateTime": rfc3339(start) }, "end": { "dateTime": rfc3339(start + 1_800_000) },
        "updated": "2026-10-20T00:00:00Z"
    });
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

/// Plays the browser: follow the consent URL straight to the loopback redirect with a code.
fn fake_browser(url: &str) -> almond_types::Result<()> {
    let u = url::Url::parse(url).unwrap();
    let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
    assert!(q["scope"].contains("gmail.readonly"), "gmail requested");
    assert_eq!(q["code_challenge_method"], "S256");
    let redirect = url::Url::parse(&q["redirect_uri"]).unwrap();
    let state = q["state"].clone();
    std::thread::spawn(move || {
        let addr = format!("127.0.0.1:{}", redirect.port().unwrap());
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        write!(
            s,
            "GET /?code=auth-code&state={state} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut buf = String::new();
        let _ = s.read_to_string(&mut buf);
    });
    Ok(())
}

async fn mount_auth(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code_verifier="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at-1", "expires_in": 3600, "refresh_token": "rt-1",
            "scope": "openid email https://www.googleapis.com/auth/calendar.readonly https://www.googleapis.com/auth/gmail.readonly"
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/userinfo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "email": ME })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
}

async fn mount_calendar_list(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/calendar/users/me/calendarList"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{ "id": ME, "summary": "Me", "primary": true, "timeZone": "Asia/Ho_Chi_Minh" }]
        })))
        .mount(server)
        .await;
}

fn titles(up: &Value) -> Vec<String> {
    up.as_array()
        .unwrap()
        .iter()
        .map(|u| u["event"]["title"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn google_signin_poll_resync_mail_remove() {
    let (api, dir) = open_core("lib-google").await;
    let core = api.core.clone();
    let server = MockServer::start().await;
    let base = server.uri();
    let now = almond_types::time::now_ms();
    let (t1, t2) = (now + 2 * 3_600_000, now + 4 * 3_600_000);

    mount_auth(&server).await;
    mount_calendar_list(&server).await;
    let all_day_date = chrono::DateTime::from_timestamp_millis(now + 30 * 3_600_000)
        .unwrap()
        .with_timezone(&chrono_tz_vn())
        .date_naive();
    let full = |items: Vec<Value>, token: &str| {
        ResponseTemplate::new(200).set_body_json(json!({
            "items": items, "nextSyncToken": token, "timeZone": "Asia/Ho_Chi_Minh"
        }))
    };
    Mock::given(method("GET"))
        .and(path(format!("/calendar/calendars/{ME}/events")))
        .and(query_param_is_missing("syncToken"))
        .respond_with(full(
            vec![
                timed(
                    "ev1",
                    "Budget sync",
                    t1,
                    json!({
                        "hangoutLink": "https://meet.google.com/aaa-bbbb-ccc",
                        "attendees": [
                            { "email": ME, "self": true, "responseStatus": "accepted" },
                            { "email": "bob@example.com", "displayName": "Bob" }
                        ]
                    }),
                ),
                timed("ev2", "Hiring panel", t2, json!({})),
                json!({ "id": "holiday", "status": "confirmed", "summary": "Holiday",
                        "start": { "date": all_day_date.to_string() },
                        "end": { "date": all_day_date.succ_opt().unwrap().to_string() } }),
            ],
            "s1",
        ))
        .mount(&server)
        .await;
    // nothing changed since s1 (the background poll may already have used the token)
    Mock::given(method("GET"))
        .and(path(format!("/calendar/calendars/{ME}/events")))
        .and(query_param("syncToken", "s1"))
        .respond_with(full(vec![], "s1"))
        .mount(&server)
        .await;

    // ---- sign in (browser → loopback redirect → token exchange)
    api.call(
        "settings.set",
        json!({ "key": "google.oauth_client", "value": {
            "client_id": "cid.apps.googleusercontent.com", "client_secret": "not-secret",
            "auth_url": format!("{base}/o/oauth2/v2/auth"), "token_url": format!("{base}/token"),
            "revoke_url": format!("{base}/revoke"), "userinfo_url": format!("{base}/v1/userinfo"),
            "calendar_api_base": format!("{base}/calendar"), "gmail_api_base": format!("{base}/gmail")
        } }),
    )
    .await
    .unwrap();
    almond_core::library::set_url_opener(&core, Arc::new(fake_browser));
    let info = call(&api, "calendar.addGoogle", json!({ "gmail": true })).await;
    assert_eq!(info["id"], format!("google:{ME}"));
    assert_eq!(info["provider"], "google");
    let sources = call(&api, "calendar.sources", json!({})).await;
    assert_eq!(sources["sources"][0]["label"], ME);
    // refresh tokens stay out of the database
    let db = std::fs::read(dir.join("almond.db")).unwrap();
    assert!(!db.windows(4).any(|w| w == b"rt-1"));

    // ---- poll → upcoming
    call(&api, "calendar.refresh", json!({})).await;
    let up = call(&api, "calendar.upcoming", json!({ "hours": 48 })).await;
    assert_eq!(titles(&up), vec!["Budget sync", "Hiring panel", "Holiday"]);
    let holiday = &up[2]["event"];
    assert_eq!(holiday["all_day"], true);
    // all-day: midnight in the calendar's zone (UTC+7), not UTC midnight
    let midnight_vn = all_day_date
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis()
        - 7 * 3_600_000;
    assert_eq!(holiday["start"].as_i64().unwrap(), midnight_vn);
    assert_eq!(up[0]["event"]["conference_app"], "google_meet");

    // ---- sync token expired (410) while ev2 was deleted: the full resync drops it (E3)
    server.reset().await;
    mount_auth(&server).await;
    mount_calendar_list(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("/calendar/calendars/{ME}/events")))
        .and(query_param("syncToken", "s1"))
        .respond_with(ResponseTemplate::new(410).set_body_json(json!({
            "error": { "code": 410, "message": "Sync token is no longer valid" }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/calendar/calendars/{ME}/events")))
        .and(query_param_is_missing("syncToken"))
        .respond_with(full(
            vec![timed(
                "ev1",
                "Budget sync",
                t1,
                json!({ "attendees": [
                { "email": ME, "self": true }, { "email": "bob@example.com" }
            ] }),
            )],
            "s2",
        ))
        .mount(&server)
        .await;
    call(&api, "calendar.refresh", json!({})).await;
    let up = call(&api, "calendar.upcoming", json!({ "hours": 48 })).await;
    assert_eq!(titles(&up), vec!["Budget sync"]);

    // ---- mail context: threads with the attendees (not me), via the event's account
    Mock::given(method("GET"))
        .and(path("/gmail/users/me/threads"))
        .and(query_param(
            "q",
            "{from:bob@example.com to:bob@example.com cc:bob@example.com}",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "threads": [{ "id": "th1", "snippet": "Numbers for Q4 attached" }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gmail/users/me/threads/th1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "th1",
            "messages": [{ "internalDate": "1793000000000", "payload": { "headers": [
                { "name": "Subject", "value": "Q4 budget" },
                { "name": "From", "value": "Bob <bob@example.com>" },
                { "name": "To", "value": ME }
            ] } }]
        })))
        .mount(&server)
        .await;
    let mail = call(
        &api,
        "mail.context",
        json!({ "event": { "source": format!("google:{ME}"), "event_id": "ev1" } }),
    )
    .await;
    assert_eq!(mail.as_array().unwrap().len(), 1, "{mail}");
    assert_eq!(mail[0]["subject"], "Q4 budget");
    assert_eq!(mail[0]["participants"], json!(["bob@example.com"]));
    let e = api
        .call(
            "mail.context",
            json!({ "event": { "source": format!("google:{ME}"), "event_id": "nope" } }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, -32004);

    // ---- remove the account: token revoked, events gone
    call(
        &api,
        "calendar.remove",
        json!({ "source": format!("google:{ME}") }),
    )
    .await;
    assert!(
        call(&api, "calendar.upcoming", json!({ "hours": 48 }))
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    let revoked = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|r| r.url.path() == "/revoke");
    assert!(revoked, "refresh token revoked at Google");

    api.core.shutdown().await;
    drop((api, core));
    let _ = std::fs::remove_dir_all(dir);
}

/// UTC+7 without pulling chrono-tz into the test: Vietnam has no DST.
fn chrono_tz_vn() -> chrono::FixedOffset {
    chrono::FixedOffset::east_opt(7 * 3600).unwrap()
}
