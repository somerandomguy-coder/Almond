//! GmailSource against a mock Gmail API (wiremock).

use almond_calendar::oauth::{
    ACCOUNT_INDEX_KEY, GoogleAuth, GoogleOAuthConfig, StaticTokenProvider,
};
use almond_mail::GmailSource;
use almond_types::Error;
use almond_types::mail::MailSource;
use almond_types::mock::MemSecretStore;
use almond_types::secrets::SecretStore;
use std::sync::Arc;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn msg(date: &str, from: &str, to: &str, subject: Option<&str>) -> serde_json::Value {
    let mut headers = vec![
        serde_json::json!({"name": "From", "value": from}),
        serde_json::json!({"name": "To", "value": to}),
    ];
    if let Some(s) = subject {
        headers.push(serde_json::json!({"name": "Subject", "value": s}));
    }
    serde_json::json!({"id": format!("m{date}"), "internalDate": date, "snippet": "last", "payload": {"headers": headers}})
}

#[tokio::test]
async fn threads_with_attendees() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users/me/threads"))
        .and(header("authorization", "Bearer tok"))
        .and(query_param(
            "q",
            "{from:ann@example.com to:ann@example.com cc:ann@example.com from:bob@example.com to:bob@example.com cc:bob@example.com}",
        ))
        .and(query_param("maxResults", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "threads": [
                {"id": "t1", "snippet": "See you &amp; thanks", "historyId": "1"},
                {"id": "t2", "snippet": "old", "historyId": "2"},
                {"id": "gone", "snippet": "x", "historyId": "3"}
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/me/threads/t1"))
        .and(query_param("format", "metadata"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "t1",
            "messages": [
                msg("1793000000000", "Ann <ann@example.com>", "me@example.com", Some("Q4 plan")),
                msg("1793100000000", "me@example.com", "\"Ann, A.\" <ann@example.com>, bob@example.com", Some("Re: Q4 plan"))
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/me/threads/t2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "t2",
            "messages": [msg("1792000000000", "bob@example.com", "me@example.com", None)]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/me/threads/gone"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let g = GmailSource::new(
        "Me@Example.com",
        Arc::new(StaticTokenProvider("tok".into())),
    )
    .with_api_base(server.uri());
    assert_eq!(g.account(), "me@example.com");
    let threads = g
        .threads_with(&["bob@example.com".into(), "ann@example.com".into()], 5)
        .await
        .unwrap();
    assert_eq!(threads.len(), 2);
    let t1 = &threads[0];
    assert_eq!(t1.thread_id, "t1");
    assert_eq!(t1.subject, "Q4 plan");
    assert_eq!(t1.participants, vec!["ann@example.com", "bob@example.com"]);
    assert_eq!(t1.snippet, "See you & thanks");
    assert_eq!(t1.last_message_at, 1_793_100_000_000);
    assert_eq!(
        t1.url.as_deref(),
        Some("https://mail.google.com/mail/u/me@example.com/#all/t1")
    );
    assert_eq!(threads[1].subject, "(no subject)");

    // nothing valid to search for → no request, empty result
    assert!(
        g.threads_with(&["nope".into()], 5)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn insufficient_scope_is_permission_denied() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": {"code": 403, "message": "Request had insufficient authentication scopes."}
        })))
        .mount(&server)
        .await;
    let g = GmailSource::new(
        "me@example.com",
        Arc::new(StaticTokenProvider("tok".into())),
    )
    .with_api_base(server.uri());
    let e = g
        .threads_with(&["a@example.com".into()], 3)
        .await
        .unwrap_err();
    assert!(matches!(e, Error::PermissionDenied(_)), "{e:?}");
}

#[test]
fn for_account_requires_gmail_scope() {
    let secrets = Arc::new(MemSecretStore::default());
    secrets
        .set(
            ACCOUNT_INDEX_KEY,
            br#"[{"email":"cal@example.com","scopes":["openid","https://www.googleapis.com/auth/calendar.readonly"],"added_at":1},
                {"email":"mail@example.com","scopes":["https://www.googleapis.com/auth/gmail.readonly"],"added_at":1}]"#,
        )
        .unwrap();
    let auth = Arc::new(GoogleAuth::new(
        GoogleOAuthConfig::new("cid", None),
        secrets,
    ));
    assert!(matches!(
        GmailSource::for_account(auth.clone(), "cal@example.com"),
        Err(Error::PermissionDenied(_))
    ));
    assert!(matches!(
        GmailSource::for_account(auth.clone(), "nobody@example.com"),
        Err(Error::NotFound(_))
    ));
    assert!(GmailSource::for_account(auth, "mail@example.com").is_ok());
}
