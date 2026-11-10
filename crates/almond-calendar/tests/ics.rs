//! IcsSubscription over HTTP (wiremock): ETag/304, incremental diffs, error mapping.

use almond_calendar::{IcsSubscription, SyncWindow};
use almond_types::Error;
use almond_types::calendar::CalendarSource;
use std::sync::Arc;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// 2026-11-09T00:00:00Z
const NOW: i64 = 1_794_182_400_000;

fn feed(extra: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
BEGIN:VEVENT\r\nUID:daily@x\r\nDTSTART;TZID=Europe/Berlin:20261101T100000\r\nDTEND;TZID=Europe/Berlin:20261101T101500\r\n\
RRULE:FREQ=DAILY;UNTIL=20261112T080000Z\r\nSUMMARY:Daily\r\nLOCATION:https://teams.microsoft.com/l/meetup-join/abc\r\nEND:VEVENT\r\n\
{extra}END:VCALENDAR\r\n"
    )
}

fn sub(server: &MockServer) -> IcsSubscription {
    IcsSubscription::new(&format!("{}/cal.ics", server.uri()), Some("Feed".into()))
        .unwrap()
        .with_window(SyncWindow {
            past_days: 3,
            future_days: 7,
        })
        .with_clock(Arc::new(|| NOW))
}

#[tokio::test]
async fn etag_and_diffs() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cal.ics"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"v1\"")
                .set_body_string(feed(
                    "BEGIN:VEVENT\r\nUID:one@x\r\nDTSTART:20261110T080000Z\r\nSUMMARY:One-off\r\nEND:VEVENT\r\n",
                )),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let s = sub(&server);
    let d1 = s.sync(None).await.unwrap();
    // window [11-06, 11-16): daily 11-06..11-11 (UNTIL 11-12 08:00Z excludes 11-12 10:00 CET = 09:00Z) + one-off
    let titles: Vec<_> = d1.upserts.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(
        titles.iter().filter(|t| **t == "Daily").count(),
        6,
        "{titles:?}"
    );
    assert_eq!(titles.iter().filter(|t| **t == "One-off").count(), 1);
    assert_eq!(d1.upserts[0].event_id, "daily@x_20261106T090000Z");
    assert_eq!(d1.upserts[0].conference_app.as_deref(), Some("teams"));
    assert!(d1.upserts.iter().all(|e| e.source == s.info().id));
    let c1 = d1.next_cursor.unwrap();

    // 304 → nothing, same cursor
    Mock::given(method("GET"))
        .and(path("/cal.ics"))
        .and(header("if-none-match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let d2 = s.sync(Some(c1.clone())).await.unwrap();
    assert!(d2.upserts.is_empty() && d2.deletes.is_empty());
    assert_eq!(d2.next_cursor.as_deref(), Some(c1.as_str()));

    // changed feed: one-off removed → delete, daily unchanged → no upserts
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/cal.ics"))
        .respond_with(ResponseTemplate::new(200).set_body_string(feed("")))
        .mount(&server)
        .await;
    let d3 = s.sync(Some(c1)).await.unwrap();
    assert!(d3.upserts.is_empty(), "{:?}", d3.upserts);
    assert_eq!(d3.deletes, vec!["one@x".to_string()]);
}

#[tokio::test]
async fn http_errors_are_mapped() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cal.ics"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let e = sub(&server).sync(None).await.unwrap_err();
    assert!(matches!(e, Error::NotFound(_)), "{e:?}");

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>login</html>"))
        .mount(&server)
        .await;
    let e = sub(&server).sync(None).await.unwrap_err();
    assert!(matches!(e, Error::Invalid(_)), "{e:?}");
}
