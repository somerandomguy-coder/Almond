//! OAuth flow against a mock Google (wiremock) with a simulated browser.

use almond_calendar::oauth::{
    AccessTokenProvider, AddAccountOptions, GoogleAuth, GoogleOAuthConfig, OpenUrl,
    SCOPE_CALENDAR_READONLY, SCOPE_GMAIL_READONLY,
};
use almond_types::Error;
use almond_types::mock::MemSecretStore;
use almond_types::secrets::{SecretStore, keys::GOOGLE_REFRESH_PREFIX};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn id_token(email: &str) -> String {
    let p = URL_SAFE_NO_PAD.encode(format!(r#"{{"email":"{email}","sub":"42"}}"#));
    format!("eyJhbGciOiJSUzI1NiJ9.{p}.c2ln")
}

/// Token endpoint that checks the PKCE verifier against the challenge the "browser" saw.
struct TokenResponder {
    challenge: Arc<Mutex<Option<String>>>,
    email: String,
    scope: String,
}

impl Respond for TokenResponder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let form: HashMap<String, String> = url::form_urlencoded::parse(&req.body)
            .into_owned()
            .collect();
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["code"], "the-code");
        assert_eq!(form["client_id"], "cid");
        assert_eq!(form["client_secret"], "csecret");
        let verifier = &form["code_verifier"];
        let ch = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(Some(ch), *self.challenge.lock().unwrap(), "PKCE mismatch");
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at-1",
            "expires_in": 3599,
            "refresh_token": "rt-1",
            "id_token": id_token(&self.email),
            "scope": self.scope,
            "token_type": "Bearer"
        }))
    }
}

/// A fake browser: parses the consent URL and calls the loopback redirect.
fn fake_browser(
    challenge: Arc<Mutex<Option<String>>>,
    tamper_state: bool,
) -> (OpenUrl, Arc<Mutex<Option<String>>>) {
    let seen = Arc::new(Mutex::new(None));
    let seen2 = seen.clone();
    let open: OpenUrl = Arc::new(move |u: &str| {
        *seen2.lock().unwrap() = Some(u.to_string());
        let url = url::Url::parse(u).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        *challenge.lock().unwrap() = Some(q["code_challenge"].clone());
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["access_type"], "offline");
        let redirect = q["redirect_uri"].clone();
        assert!(redirect.starts_with("http://127.0.0.1:"));
        let state = if tamper_state {
            "evil".to_string()
        } else {
            q["state"].clone()
        };
        tokio::spawn(async move {
            // a stray favicon request first, like real browsers
            let _ = reqwest::get(format!("{redirect}/favicon.ico")).await;
            let r = reqwest::get(format!("{redirect}/?state={state}&code=the-code&scope=x"))
                .await
                .unwrap();
            let _ = r.text().await;
        });
        Ok(())
    });
    (open, seen)
}

async fn setup(email: &str, scope: &str) -> (MockServer, Arc<Mutex<Option<String>>>) {
    let server = MockServer::start().await;
    let challenge = Arc::new(Mutex::new(None));
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(TokenResponder {
            challenge: challenge.clone(),
            email: email.into(),
            scope: scope.into(),
        })
        .mount(&server)
        .await;
    (server, challenge)
}

fn cfg(server: &MockServer) -> GoogleOAuthConfig {
    GoogleOAuthConfig::new("cid", Some("csecret".into())).with_base_url(&server.uri())
}

#[tokio::test]
async fn sign_in_stores_refresh_token_and_lists_accounts() {
    let scope =
        format!("openid https://www.googleapis.com/auth/userinfo.email {SCOPE_CALENDAR_READONLY}");
    let (server, challenge) = setup("alice@example.com", &scope).await;
    let secrets = Arc::new(MemSecretStore::default());
    let auth = GoogleAuth::new(cfg(&server), secrets.clone());
    let (open, seen) = fake_browser(challenge, false);

    let acct = auth
        .add_account(AddAccountOptions::default(), &open)
        .await
        .unwrap();
    assert_eq!(acct.email, "alice@example.com");
    assert!(acct.calendar_enabled());
    assert!(!acct.gmail_enabled());
    let consent = seen.lock().unwrap().clone().unwrap();
    assert!(!consent.contains("gmail"));
    assert_eq!(
        secrets
            .get(&format!("{GOOGLE_REFRESH_PREFIX}alice@example.com"))
            .unwrap()
            .as_deref(),
        Some(&b"rt-1"[..])
    );
    assert_eq!(auth.accounts().unwrap().len(), 1);
    // cached access token
    assert_eq!(
        auth.access_token("alice@example.com").await.unwrap(),
        "at-1"
    );
}

#[tokio::test]
async fn two_accounts_and_gmail_optional() {
    let secrets = Arc::new(MemSecretStore::default());
    let scope_a = format!("openid email {SCOPE_CALENDAR_READONLY}");
    let (s1, c1) = setup("a@example.com", &scope_a).await;
    let auth1 = GoogleAuth::new(cfg(&s1), secrets.clone());
    auth1
        .add_account(AddAccountOptions::default(), &fake_browser(c1, false).0)
        .await
        .unwrap();

    let scope_b = format!("openid email {SCOPE_CALENDAR_READONLY} {SCOPE_GMAIL_READONLY}");
    let (s2, c2) = setup("b@example.com", &scope_b).await;
    let auth2 = GoogleAuth::new(cfg(&s2), secrets.clone());
    let (open, seen) = fake_browser(c2, false);
    let b = auth2
        .add_account(
            AddAccountOptions {
                gmail: true,
                ..Default::default()
            },
            &open,
        )
        .await
        .unwrap();
    assert!(b.gmail_enabled());
    assert!(
        seen.lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .contains("gmail.readonly")
    );
    let emails: Vec<String> = auth2
        .accounts()
        .unwrap()
        .into_iter()
        .map(|a| a.email)
        .collect();
    assert_eq!(emails, vec!["a@example.com", "b@example.com"]);
}

#[tokio::test]
async fn state_mismatch_is_rejected() {
    let (server, challenge) = setup("alice@example.com", "openid").await;
    let auth = GoogleAuth::new(cfg(&server), Arc::new(MemSecretStore::default()));
    let (open, _) = fake_browser(challenge, true);
    let err = auth
        .add_account(AddAccountOptions::default(), &open)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::PermissionDenied(_)), "{err:?}");
    assert!(auth.accounts().unwrap().is_empty());
}

#[tokio::test]
async fn refresh_and_invalid_grant() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("refresh_token=good"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "fresh", "expires_in": 3600
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("refresh_token=revoked"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant", "error_description": "Token has been expired or revoked."
        })))
        .mount(&server)
        .await;
    let secrets = Arc::new(MemSecretStore::default());
    secrets
        .set(&format!("{GOOGLE_REFRESH_PREFIX}ok@example.com"), b"good")
        .unwrap();
    secrets
        .set(
            &format!("{GOOGLE_REFRESH_PREFIX}old@example.com"),
            b"revoked",
        )
        .unwrap();
    let auth = GoogleAuth::new(cfg(&server), secrets);
    assert_eq!(auth.access_token("ok@example.com").await.unwrap(), "fresh");
    let e = auth.access_token("old@example.com").await.unwrap_err();
    assert!(matches!(e, Error::AuthRequired(_)), "{e:?}");
    let e = auth.access_token("nobody@example.com").await.unwrap_err();
    assert!(matches!(e, Error::AuthRequired(_)), "{e:?}");
}

#[tokio::test]
async fn remove_account_revokes_and_deletes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .and(body_string_contains("token=rt-x"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let secrets = Arc::new(MemSecretStore::default());
    let key = format!("{GOOGLE_REFRESH_PREFIX}x@example.com");
    secrets.set(&key, b"rt-x").unwrap();
    secrets
        .set(
            almond_calendar::oauth::ACCOUNT_INDEX_KEY,
            br#"[{"email":"x@example.com","scopes":["openid"],"added_at":1}]"#,
        )
        .unwrap();
    let auth = GoogleAuth::new(cfg(&server), secrets.clone());
    assert_eq!(auth.accounts().unwrap().len(), 1);
    auth.remove_account("x@example.com").await.unwrap();
    assert!(secrets.get(&key).unwrap().is_none());
    assert!(auth.accounts().unwrap().is_empty());
}

/// Real sign-in. Run with:
/// `ALMOND_GOOGLE_CLIENT_ID=... ALMOND_GOOGLE_CLIENT_SECRET=... cargo test -p almond-calendar --test oauth -- --ignored --nocapture`
#[tokio::test]
#[ignore = "needs a real Google OAuth client and a browser"]
async fn real_google_sign_in() {
    let id = std::env::var("ALMOND_GOOGLE_CLIENT_ID").expect("ALMOND_GOOGLE_CLIENT_ID");
    let secret = std::env::var("ALMOND_GOOGLE_CLIENT_SECRET").ok();
    let auth = GoogleAuth::new(
        GoogleOAuthConfig::new(id, secret),
        Arc::new(MemSecretStore::default()),
    );
    let open: OpenUrl = Arc::new(|u: &str| {
        println!("Open this URL in your browser:\n{u}");
        Ok(())
    });
    let acct = auth
        .add_account(AddAccountOptions::default(), &open)
        .await
        .unwrap();
    println!("signed in, scopes: {:?}", acct.scopes);
    auth.remove_account(&acct.email).await.unwrap();
}
