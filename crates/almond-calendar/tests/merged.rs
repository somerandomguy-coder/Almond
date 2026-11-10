//! "Done when": two Google accounts + an ICS feed → merged upcoming events with reminders.
//! Everything mocked: Google + ICS via wiremock, the event log via `almond_types::mock::MemStore`.

use almond_calendar::oauth::AccessTokenProvider;
use almond_calendar::{GoogleCalendar, IcsSubscription, ReminderScheduler, delta_to_event_bodies};
use almond_types::calendar::CalendarSource;
use almond_types::mock::MemStore;
use almond_types::store::Store;
use almond_types::{DeviceId, Result};
use async_trait::async_trait;
use std::sync::Arc;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// 2026-11-09T08:00:00Z
const NOW: i64 = 1_794_211_200_000;

/// Token = "tok-<account>".
struct PerAccount;

#[async_trait]
impl AccessTokenProvider for PerAccount {
    async fn access_token(&self, a: &str) -> Result<String> {
        Ok(format!("tok-{a}"))
    }
    fn invalidate(&self, _: &str) {}
}

async fn mount_account(server: &MockServer, email: &str, events: serde_json::Value) {
    let auth = format!("Bearer tok-{email}");
    Mock::given(method("GET"))
        .and(path("/users/me/calendarList"))
        .and(header("authorization", auth.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"id": email, "summary": email, "primary": true}]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/calendars/{email}/events")))
        .and(header("authorization", auth.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": events, "nextSyncToken": "s1"
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn two_google_accounts_and_ics_merged_with_reminders() {
    let server = MockServer::start().await;
    mount_account(
        &server,
        "work@example.com",
        serde_json::json!([{
            "id": "w1", "summary": "Design review",
            "start": {"dateTime": "2026-11-09T08:05:00Z"}, "end": {"dateTime": "2026-11-09T09:00:00Z"},
            "hangoutLink": "https://meet.google.com/abc-defg-hij",
            "attendees": [{"email": "work@example.com", "self": true, "responseStatus": "accepted"},
                          {"email": "ann@example.com"}]
        }]),
    )
    .await;
    mount_account(
        &server,
        "home@example.com",
        serde_json::json!([
            {"id": "h1", "summary": "Dentist",
             "start": {"dateTime": "2026-11-09T08:04:00Z"}, "end": {"dateTime": "2026-11-09T09:00:00Z"}},
            {"id": "h2", "summary": "Call with mom (declined)",
             "start": {"dateTime": "2026-11-09T08:03:00Z"}, "end": {"dateTime": "2026-11-09T09:00:00Z"},
             "attendees": [{"email": "home@example.com", "self": true, "responseStatus": "declined"},
                           {"email": "mom@example.com"}]}
        ]),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/team.ics"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:ics1\r\n\
DTSTART:20261109T080200Z\r\nDTEND:20261109T083000Z\r\nSUMMARY:Team sync\r\n\
LOCATION:https://zoom.us/j/555\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        ))
        .mount(&server)
        .await;

    let auth: Arc<dyn AccessTokenProvider> = Arc::new(PerAccount);
    let clock = Arc::new(|| NOW);
    let sources: Vec<Box<dyn CalendarSource>> = vec![
        Box::new(
            GoogleCalendar::new("work@example.com", auth.clone())
                .with_api_base(server.uri())
                .with_clock(clock.clone()),
        ),
        Box::new(
            GoogleCalendar::new("home@example.com", auth.clone())
                .with_api_base(server.uri())
                .with_clock(clock.clone()),
        ),
        Box::new(
            IcsSubscription::new(&format!("{}/team.ics", server.uri()), Some("Team".into()))
                .unwrap()
                .with_clock(clock.clone()),
        ),
    ];

    let store = MemStore::new(DeviceId("pc".into()));
    for src in &sources {
        let id = src.info().id;
        let cursor_key = format!("calendar/cursor/{id}");
        let delta = src.sync(store.kv_get(&cursor_key).unwrap()).await.unwrap();
        for body in delta_to_event_bodies(&id, &delta) {
            store.append_local(body).unwrap();
        }
        if let Some(c) = &delta.next_cursor {
            store.kv_set(&cursor_key, c).unwrap();
        }
    }

    let mut upcoming = store.calendar_events(NOW, NOW + 86_400_000).unwrap();
    upcoming.sort_by_key(|e| e.start);
    let titles: Vec<_> = upcoming.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(
        titles,
        vec![
            "Team sync",
            "Call with mom (declined)",
            "Dentist",
            "Design review"
        ]
    );

    let mut sched = ReminderScheduler::default();
    let reminders = sched.due(&upcoming, NOW);
    let r: Vec<_> = reminders.iter().map(|r| r.event.title.as_str()).collect();
    // Dentist: no link/attendees; Call with mom: declined
    assert_eq!(r, vec!["Team sync", "Design review"]);
    assert!(sched.due(&upcoming, NOW + 60_000).is_empty(), "fires once");
}
