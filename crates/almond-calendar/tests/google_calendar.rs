//! GoogleCalendar against a mock Calendar API (wiremock).

use almond_calendar::GoogleCalendar;
use almond_calendar::oauth::{AccessTokenProvider, StaticTokenProvider};
use almond_types::Result;
use almond_types::calendar::CalendarSource;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NOW: i64 = 1_793_000_000_000; // 2026-10-26T07:33:20Z

fn cal(server: &MockServer) -> GoogleCalendar {
    GoogleCalendar::new(
        "Me@Example.com",
        Arc::new(StaticTokenProvider("tok".into())),
    )
    .with_api_base(server.uri())
    .with_clock(Arc::new(|| NOW))
}

async fn mount_calendar_list(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/users/me/calendarList"))
        .and(query_param_is_missing("pageToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [
                {"id": "me@example.com", "summary": "Me", "primary": true},
                {"id": "hidden@group.calendar.google.com", "summary": "Hidden", "selected": false}
            ],
            "nextPageToken": "p2"
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/me/calendarList"))
        .and(query_param("pageToken", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"id": "team@group.calendar.google.com", "summary": "Team", "summaryOverride": "My team", "selected": true}]
        })))
        .mount(server)
        .await;
}

fn event(id: &str, start: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut v = serde_json::json!({
        "id": id, "status": "confirmed", "summary": format!("Event {id}"),
        "start": {"dateTime": start}, "end": {"dateTime": start},
        "updated": "2026-10-20T00:00:00Z"
    });
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in e {
            o.insert(k.clone(), val.clone());
        }
    }
    v
}

#[tokio::test]
async fn full_then_incremental_then_410() {
    let server = MockServer::start().await;
    mount_calendar_list(&server).await;

    // --- full sync, primary calendar: two pages
    Mock::given(method("GET"))
        .and(path("/calendars/me@example.com/events"))
        .and(header("authorization", "Bearer tok"))
        .and(query_param("singleEvents", "true"))
        .and(query_param("timeMin", "2026-09-26T07:33:20Z"))
        .and(query_param_is_missing("syncToken"))
        .and(query_param_is_missing("pageToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [event("a", "2026-10-27T09:00:00Z", serde_json::json!({"hangoutLink": "https://meet.google.com/aaa-bbbb-ccc"}))],
            "nextPageToken": "e2"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/calendars/me@example.com/events"))
        .and(query_param("pageToken", "e2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [event("b", "2026-10-28T09:00:00Z", serde_json::json!({}))],
            "nextSyncToken": "sync-me-1"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/calendars/team@group.calendar.google.com/events"))
        .and(query_param_is_missing("syncToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [event("t1", "2026-10-29T09:00:00Z", serde_json::json!({}))],
            "nextSyncToken": "sync-team-1"
        })))
        .mount(&server)
        .await;

    let src = cal(&server);
    assert_eq!(src.info().id, "google:me@example.com");
    let d1 = src.sync(None).await.unwrap();
    let ids: Vec<_> = d1.upserts.iter().map(|e| e.event_id.as_str()).collect();
    assert_eq!(ids, vec!["a", "b", "t1"]);
    assert!(d1.deletes.is_empty());
    assert_eq!(d1.upserts[0].conference_app.as_deref(), Some("google_meet"));
    assert_eq!(d1.upserts[2].calendar_name.as_deref(), Some("My team"));
    assert!(
        d1.upserts
            .iter()
            .all(|e| e.source == "google:me@example.com")
    );
    let c1 = d1.next_cursor.clone().unwrap();
    assert!(c1.contains("sync-me-1") && c1.contains("sync-team-1"));

    // --- incremental: primary has a change + a cancellation; team's token expired (410)
    Mock::given(method("GET"))
        .and(path("/calendars/me@example.com/events"))
        .and(query_param("syncToken", "sync-me-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [
                event("a", "2026-10-27T10:00:00Z", serde_json::json!({})),
                {"id": "b", "status": "cancelled"}
            ],
            "nextSyncToken": "sync-me-2"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/calendars/team@group.calendar.google.com/events"))
        .and(query_param("syncToken", "sync-team-1"))
        .respond_with(ResponseTemplate::new(410).set_body_json(serde_json::json!({
            "error": {"code": 410, "message": "Sync token is no longer valid, a full sync is required."}
        })))
        .mount(&server)
        .await;

    let d2 = src.sync(Some(c1)).await.unwrap();
    let ids: Vec<_> = d2.upserts.iter().map(|e| e.event_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["a", "t1"],
        "a changed; t1 from full resync after 410"
    );
    assert_eq!(d2.deletes, vec!["b".to_string()]);
    // E3: the 410'd calendar is reset so events deleted meanwhile don't linger
    assert_eq!(
        d2.reset_calendars,
        vec!["team@group.calendar.google.com".to_string()]
    );
    let c2 = d2.next_cursor.unwrap();
    assert!(c2.contains("sync-me-2") && c2.contains("sync-team-1"));
}

/// Token provider that hands out a stale token first.
struct Rotating(AtomicU32);

#[async_trait]
impl AccessTokenProvider for Rotating {
    async fn access_token(&self, _a: &str) -> Result<String> {
        Ok(format!("t{}", self.0.load(Ordering::SeqCst)))
    }
    fn invalidate(&self, _a: &str) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn retries_once_on_401_and_maps_auth_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/me/calendarList"))
        .and(header("authorization", "Bearer t0"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/me/calendarList"))
        .and(header("authorization", "Bearer t1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": []})))
        .mount(&server)
        .await;
    let src = GoogleCalendar::new("me@example.com", Arc::new(Rotating(AtomicU32::new(0))))
        .with_api_base(server.uri());
    let d = src.sync(None).await.unwrap();
    assert!(d.upserts.is_empty());

    let server2 = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server2)
        .await;
    let src2 = GoogleCalendar::new("me@example.com", Arc::new(StaticTokenProvider("x".into())))
        .with_api_base(server2.uri());
    let e = src2.sync(None).await.unwrap_err();
    assert!(matches!(e, almond_types::Error::AuthRequired(_)), "{e:?}");
}

/// Real account. Needs a short-lived access token with calendar.readonly (e.g. from
/// https://developers.google.com/oauthplayground); run with
/// `ALMOND_GOOGLE_ACCESS_TOKEN=... ALMOND_GOOGLE_EMAIL=... cargo test -p almond-calendar --test google_calendar -- --ignored --nocapture`
#[tokio::test]
#[ignore = "needs a real Google account access token"]
async fn real_google_calendar() {
    let token = std::env::var("ALMOND_GOOGLE_ACCESS_TOKEN").expect("ALMOND_GOOGLE_ACCESS_TOKEN");
    let email = std::env::var("ALMOND_GOOGLE_EMAIL").expect("ALMOND_GOOGLE_EMAIL");
    let src = GoogleCalendar::new(email, Arc::new(StaticTokenProvider(token)));
    let d = src.sync(None).await.unwrap();
    println!("{} events", d.upserts.len());
    for e in d.upserts.iter().take(10) {
        println!("{} {} {:?}", e.start, e.title, e.conference_app);
    }
}
