//! Google OAuth 2.0 for installed apps: loopback redirect on `127.0.0.1:<random port>`, PKCE
//! (S256) and `state`, opening the system browser through an injected [`OpenUrl`] callback (the
//! desktop shell / Android plugin provide the real implementation).
//!
//! * Any number of accounts, keyed by the email from the `id_token` (falls back to `userinfo`).
//! * Refresh tokens live in the [`SecretStore`] under
//!   `format!("{GOOGLE_REFRESH_PREFIX}{email}")`; the list of accounts (email + granted scopes, no
//!   secrets) under [`ACCOUNT_INDEX_KEY`]. Access tokens are cached in memory only.
//! * `invalid_grant` on refresh (revoked, expired, password change, or the 7-day expiry of an app
//!   in "Testing" status) → [`Error::AuthRequired`]; the UI should offer "sign in again".
//! * Removing an account revokes the refresh token at Google (best effort) and deletes it locally.
//!
//! The OAuth client id/secret come from user config (the user brings their own Google Cloud
//! project, see `docs/google-setup.md`). For a Desktop client the "secret" is not confidential.

use crate::net_err;
use almond_types::secrets::{SecretStore, keys::GOOGLE_REFRESH_PREFIX};
use almond_types::time::{UnixMs, now_ms};
use almond_types::{Error, Result};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const SCOPE_OPENID: &str = "openid";
pub const SCOPE_EMAIL: &str = "email";
pub const SCOPE_CALENDAR_READONLY: &str = "https://www.googleapis.com/auth/calendar.readonly";
pub const SCOPE_GMAIL_READONLY: &str = "https://www.googleapis.com/auth/gmail.readonly";

/// Secret-store key holding the JSON list of [`GoogleAccount`]s (no tokens in it).
pub const ACCOUNT_INDEX_KEY: &str = "almond/google/accounts";

/// Opens a URL in the user's browser. Desktop: `tauri-plugin-opener`; Android: Custom Tab.
pub type OpenUrl = Arc<dyn Fn(&str) -> Result<()> + Send + Sync>;

/// User-provided OAuth client ("Desktop app" client from the user's own GCP project).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleOAuthConfig {
    pub client_id: String,
    /// Desktop clients have a (non-confidential) secret; Google requires it at the token endpoint.
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default = "default_auth_url")]
    pub auth_url: String,
    #[serde(default = "default_token_url")]
    pub token_url: String,
    #[serde(default = "default_revoke_url")]
    pub revoke_url: String,
    #[serde(default = "default_userinfo_url")]
    pub userinfo_url: String,
}

fn default_auth_url() -> String {
    "https://accounts.google.com/o/oauth2/v2/auth".into()
}
fn default_token_url() -> String {
    "https://oauth2.googleapis.com/token".into()
}
fn default_revoke_url() -> String {
    "https://oauth2.googleapis.com/revoke".into()
}
fn default_userinfo_url() -> String {
    "https://openidconnect.googleapis.com/v1/userinfo".into()
}

impl GoogleOAuthConfig {
    pub fn new(client_id: impl Into<String>, client_secret: Option<String>) -> Self {
        GoogleOAuthConfig {
            client_id: client_id.into(),
            client_secret,
            auth_url: default_auth_url(),
            token_url: default_token_url(),
            revoke_url: default_revoke_url(),
            userinfo_url: default_userinfo_url(),
        }
    }

    /// Point every endpoint at `base` (tests / mock servers).
    pub fn with_base_url(mut self, base: &str) -> Self {
        let b = base.trim_end_matches('/');
        self.auth_url = format!("{b}/o/oauth2/v2/auth");
        self.token_url = format!("{b}/token");
        self.revoke_url = format!("{b}/revoke");
        self.userinfo_url = format!("{b}/v1/userinfo");
        self
    }
}

/// A connected Google account (persisted in the account index; contains no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleAccount {
    pub email: String,
    /// Scopes Google actually granted (the user may untick some on the consent screen).
    pub scopes: Vec<String>,
    pub added_at: UnixMs,
}

impl GoogleAccount {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }
    pub fn calendar_enabled(&self) -> bool {
        self.has_scope(SCOPE_CALENDAR_READONLY)
    }
    pub fn gmail_enabled(&self) -> bool {
        self.has_scope(SCOPE_GMAIL_READONLY)
    }
}

/// Options for [`GoogleAuth::add_account`].
#[derive(Debug, Clone, Default)]
pub struct AddAccountOptions {
    /// Also request `gmail.readonly` (optional per account).
    pub gmail: bool,
    /// Pre-select an account in Google's chooser (also used when re-authenticating).
    pub login_hint: Option<String>,
    /// How long to wait for the browser redirect. Default 5 minutes.
    pub timeout: Option<Duration>,
}

/// Something that can hand out a bearer token for a Google account.
#[async_trait]
pub trait AccessTokenProvider: Send + Sync {
    async fn access_token(&self, account: &str) -> Result<String>;
    /// Forget a cached token (call after a 401 so the next call refreshes).
    fn invalidate(&self, account: &str);
}

/// Fixed token — for tests and tools.
pub struct StaticTokenProvider(pub String);

#[async_trait]
impl AccessTokenProvider for StaticTokenProvider {
    async fn access_token(&self, _account: &str) -> Result<String> {
        Ok(self.0.clone())
    }
    fn invalidate(&self, _account: &str) {}
}

#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    expires_at: UnixMs,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorBody {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Google account manager + token provider.
pub struct GoogleAuth {
    cfg: GoogleOAuthConfig,
    secrets: Arc<dyn SecretStore>,
    http: reqwest::Client,
    cache: Mutex<HashMap<String, CachedToken>>,
    /// Serializes read-modify-write of the account index.
    index_lock: Mutex<()>,
}

impl GoogleAuth {
    pub fn new(cfg: GoogleOAuthConfig, secrets: Arc<dyn SecretStore>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self::with_http(cfg, secrets, http)
    }

    pub fn with_http(
        cfg: GoogleOAuthConfig,
        secrets: Arc<dyn SecretStore>,
        http: reqwest::Client,
    ) -> Self {
        GoogleAuth {
            cfg,
            secrets,
            http,
            cache: Mutex::new(HashMap::new()),
            index_lock: Mutex::new(()),
        }
    }

    pub fn config(&self) -> &GoogleOAuthConfig {
        &self.cfg
    }

    // ------------------------------------------------------------ account index

    pub fn accounts(&self) -> Result<Vec<GoogleAccount>> {
        let _g = self.index_lock.lock().expect("index lock");
        self.read_index()
    }

    pub fn account(&self, email: &str) -> Result<Option<GoogleAccount>> {
        Ok(self
            .accounts()?
            .into_iter()
            .find(|a| a.email.eq_ignore_ascii_case(email)))
    }

    fn read_index(&self) -> Result<Vec<GoogleAccount>> {
        match self.secrets.get(ACCOUNT_INDEX_KEY)? {
            Some(b) => Ok(serde_json::from_slice(&b)?),
            None => Ok(Vec::new()),
        }
    }

    fn write_index(&self, list: &[GoogleAccount]) -> Result<()> {
        self.secrets
            .set(ACCOUNT_INDEX_KEY, &serde_json::to_vec(list)?)
    }

    fn upsert_index(&self, acct: GoogleAccount) -> Result<()> {
        let _g = self.index_lock.lock().expect("index lock");
        let mut list = self.read_index()?;
        list.retain(|a| !a.email.eq_ignore_ascii_case(&acct.email));
        list.push(acct);
        list.sort_by(|a, b| a.email.cmp(&b.email));
        self.write_index(&list)
    }

    fn remove_from_index(&self, email: &str) -> Result<()> {
        let _g = self.index_lock.lock().expect("index lock");
        let mut list = self.read_index()?;
        list.retain(|a| !a.email.eq_ignore_ascii_case(email));
        self.write_index(&list)
    }

    fn refresh_key(email: &str) -> String {
        format!("{GOOGLE_REFRESH_PREFIX}{}", email.to_ascii_lowercase())
    }

    // ------------------------------------------------------------ sign-in

    /// Run the interactive sign-in: open the browser at Google's consent page, wait for the
    /// redirect to the loopback listener, exchange the code (PKCE) and store the refresh token.
    /// Signing in to an already-connected account replaces its token and scopes.
    pub async fn add_account(
        &self,
        opts: AddAccountOptions,
        open_url: &OpenUrl,
    ) -> Result<GoogleAccount> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}");

        let pkce = Pkce::generate();
        let state = random_token(16);
        let mut scopes = vec![SCOPE_OPENID, SCOPE_EMAIL, SCOPE_CALENDAR_READONLY];
        if opts.gmail {
            scopes.push(SCOPE_GMAIL_READONLY);
        }
        let requested = scopes.join(" ");

        let mut url = url::Url::parse(&self.cfg.auth_url)
            .map_err(|e| Error::Invalid(format!("auth_url: {e}")))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("client_id", &self.cfg.client_id)
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair("response_type", "code")
                .append_pair("scope", &requested)
                .append_pair("code_challenge", &pkce.challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", &state)
                .append_pair("access_type", "offline")
                // Always show consent so Google returns a refresh token, even on re-add.
                .append_pair("prompt", "consent select_account");
            if let Some(h) = &opts.login_hint {
                q.append_pair("login_hint", h);
            }
        }
        open_url(url.as_str())?;

        let timeout = opts.timeout.unwrap_or(Duration::from_secs(300));
        let code = tokio::time::timeout(timeout, wait_for_code(&listener, &state))
            .await
            .map_err(|_| Error::Cancelled)??;
        drop(listener);

        let mut form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code),
            ("client_id", self.cfg.client_id.clone()),
            ("redirect_uri", redirect_uri),
            ("code_verifier", pkce.verifier),
        ];
        if let Some(s) = &self.cfg.client_secret {
            form.push(("client_secret", s.clone()));
        }
        let tok = self.token_request(&form).await?;

        let email = match tok.id_token.as_deref().and_then(email_from_id_token) {
            Some(e) => e,
            None => self.userinfo_email(&tok.access_token).await?,
        };
        let refresh = tok.refresh_token.clone().ok_or_else(|| {
            Error::Other(
                "Google returned no refresh token; remove Almond at \
                 https://myaccount.google.com/permissions and sign in again"
                    .into(),
            )
        })?;
        self.secrets
            .set(&Self::refresh_key(&email), refresh.as_bytes())?;
        let granted: Vec<String> = match &tok.scope {
            Some(s) => s.split_whitespace().map(str::to_string).collect(),
            None => scopes.iter().map(|s| s.to_string()).collect(),
        };
        let acct = GoogleAccount {
            email: email.clone(),
            scopes: normalize_scopes(granted),
            added_at: now_ms(),
        };
        self.upsert_index(acct.clone())?;
        self.cache_token(&email, &tok);
        Ok(acct)
    }

    /// Revoke (best effort) and forget an account.
    pub async fn remove_account(&self, email: &str) -> Result<()> {
        let key = Self::refresh_key(email);
        if let Some(tok) = self.secrets.get(&key)? {
            let tok = String::from_utf8_lossy(&tok).to_string();
            // Best effort: an already-revoked token returns 400, which is fine.
            let _ = self
                .http
                .post(&self.cfg.revoke_url)
                .form(&[("token", tok.as_str())])
                .send()
                .await;
        }
        self.secrets.delete(&key)?;
        self.remove_from_index(email)?;
        self.invalidate(email);
        Ok(())
    }

    // ------------------------------------------------------------ tokens

    fn cache_token(&self, email: &str, tok: &TokenResponse) {
        let ttl = tok.expires_in.unwrap_or(3600);
        // Refresh a minute early.
        let expires_at = now_ms() + (ttl - 60).max(0) * 1000;
        self.cache.lock().expect("cache").insert(
            email.to_ascii_lowercase(),
            CachedToken {
                token: tok.access_token.clone(),
                expires_at,
            },
        );
    }

    async fn refresh(&self, email: &str) -> Result<String> {
        let refresh = self
            .secrets
            .get(&Self::refresh_key(email))?
            .ok_or_else(|| {
                Error::AuthRequired(format!("Google account {email} is not signed in"))
            })?;
        let refresh = String::from_utf8_lossy(&refresh).to_string();
        let mut form = vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh),
            ("client_id", self.cfg.client_id.clone()),
        ];
        if let Some(s) = &self.cfg.client_secret {
            form.push(("client_secret", s.clone()));
        }
        let tok = match self.token_request(&form).await {
            Err(Error::AuthRequired(m)) => {
                return Err(Error::AuthRequired(format!(
                    "Google account {email} must sign in again ({m})"
                )));
            }
            r => r?,
        };
        if let Some(new_refresh) = &tok.refresh_token {
            self.secrets
                .set(&Self::refresh_key(email), new_refresh.as_bytes())?;
        }
        self.cache_token(email, &tok);
        Ok(tok.access_token)
    }

    async fn token_request(&self, form: &[(&str, String)]) -> Result<TokenResponse> {
        let resp = self
            .http
            .post(&self.cfg.token_url)
            .form(form)
            .send()
            .await
            .map_err(net_err)?;
        let status = resp.status();
        if status.is_success() {
            return resp.json().await.map_err(net_err);
        }
        let body = resp.text().await.unwrap_or_default();
        let err: Option<OAuthErrorBody> = serde_json::from_str(&body).ok();
        match err {
            Some(e) if e.error == "invalid_grant" => Err(Error::AuthRequired(format!(
                "invalid_grant: {}",
                e.error_description.unwrap_or_default()
            ))),
            Some(e) if e.error == "invalid_client" || e.error == "unauthorized_client" => {
                Err(Error::Invalid(format!(
                    "Google rejected the OAuth client ({}); check client id/secret in settings",
                    e.error
                )))
            }
            _ if status.is_server_error() => {
                Err(Error::Network(format!("token endpoint {status}")))
            }
            _ => Err(Error::Other(format!("token endpoint {status}: {body}"))),
        }
    }

    async fn userinfo_email(&self, access_token: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct UserInfo {
            email: Option<String>,
        }
        let resp = self
            .http
            .get(&self.cfg.userinfo_url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(net_err)?;
        if !resp.status().is_success() {
            return Err(Error::Other(format!("userinfo: {}", resp.status())));
        }
        let ui: UserInfo = resp.json().await.map_err(net_err)?;
        ui.email
            .map(|e| e.to_ascii_lowercase())
            .ok_or_else(|| Error::Other("userinfo returned no email".into()))
    }
}

#[async_trait]
impl AccessTokenProvider for GoogleAuth {
    async fn access_token(&self, account: &str) -> Result<String> {
        let key = account.to_ascii_lowercase();
        if let Some(c) = self.cache.lock().expect("cache").get(&key)
            && c.expires_at > now_ms()
        {
            return Ok(c.token.clone());
        }
        self.refresh(&key).await
    }

    fn invalidate(&self, account: &str) {
        self.cache
            .lock()
            .expect("cache")
            .remove(&account.to_ascii_lowercase());
    }
}

fn normalize_scopes(mut v: Vec<String>) -> Vec<String> {
    // Google echoes `email` as the userinfo.email URL; keep both spellings recognisable.
    for s in v.iter_mut() {
        if s == "https://www.googleapis.com/auth/userinfo.email" {
            *s = SCOPE_EMAIL.into();
        }
    }
    v.sort();
    v.dedup();
    v
}

// ---------------------------------------------------------------- PKCE / state

struct Pkce {
    verifier: String,
    challenge: String,
}

impl Pkce {
    fn generate() -> Self {
        let verifier = random_token(32); // 43 chars, within RFC 7636's 43..=128
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Pkce {
            verifier,
            challenge,
        }
    }
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    for b in buf.iter_mut() {
        *b = rand::random();
    }
    URL_SAFE_NO_PAD.encode(&buf)
}

/// Extract the `email` claim from an id_token. The token comes straight from Google's token
/// endpoint over TLS, so (per OIDC Core §3.1.3.7) signature validation is not required here.
pub(crate) fn email_from_id_token(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("email")?.as_str().map(|s| s.to_ascii_lowercase())
}

// ---------------------------------------------------------------- loopback listener

const PAGE_OK: &str = "<!doctype html><meta charset=utf-8><title>Almond</title>\
<body style=\"font-family:sans-serif;text-align:center;margin-top:4em\">\
<h2>Almond is connected to Google.</h2><p>You can close this tab.</p>";
const PAGE_ERR: &str = "<!doctype html><meta charset=utf-8><title>Almond</title>\
<body style=\"font-family:sans-serif;text-align:center;margin-top:4em\">\
<h2>Sign-in was not completed.</h2><p>Return to Almond and try again.</p>";

/// Accept connections until one carries the OAuth redirect; return the authorization code.
async fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    loop {
        let (mut sock, _) = listener.accept().await?;
        let target = match read_request_target(&mut sock).await {
            Ok(Some(t)) => t,
            _ => {
                let _ = respond(&mut sock, "400 Bad Request", PAGE_ERR).await;
                continue;
            }
        };
        let url = match url::Url::parse(&format!("http://127.0.0.1{target}")) {
            Ok(u) => u,
            Err(_) => {
                let _ = respond(&mut sock, "400 Bad Request", PAGE_ERR).await;
                continue;
            }
        };
        let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
        if !params.contains_key("code") && !params.contains_key("error") {
            // favicon.ico, browser prefetch, …
            let _ = respond(&mut sock, "404 Not Found", "").await;
            continue;
        }
        if params.get("state").map(String::as_str) != Some(expected_state) {
            let _ = respond(&mut sock, "400 Bad Request", PAGE_ERR).await;
            return Err(Error::PermissionDenied(
                "OAuth state mismatch (possible CSRF); sign-in aborted".into(),
            ));
        }
        if let Some(err) = params.get("error") {
            let _ = respond(&mut sock, "200 OK", PAGE_ERR).await;
            return Err(if err == "access_denied" {
                Error::Cancelled
            } else {
                Error::PermissionDenied(format!("Google sign-in failed: {err}"))
            });
        }
        let _ = respond(&mut sock, "200 OK", PAGE_OK).await;
        return Ok(params["code"].clone());
    }
}

/// Read the request head and return the request-target of a GET (e.g. `/?code=..&state=..`).
async fn read_request_target(sock: &mut TcpStream) -> Result<Option<String>> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(10), sock.read(&mut chunk))
            .await
            .map_err(|_| Error::Network("loopback read timeout".into()))??;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16 * 1024 {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let line = head.lines().next().unwrap_or_default();
    let mut parts = line.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some("GET"), Some(t)) if t.starts_with('/') => Ok(Some(t.to_string())),
        _ => Ok(None),
    }
}

async fn respond(sock: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(resp.as_bytes()).await?;
    sock.shutdown().await
}

// ---------------------------------------------------------------- authorized requests

/// GET `url` with a bearer token for `account`; on 401 invalidate the token and retry once.
pub async fn authorized_get(
    http: &reqwest::Client,
    auth: &dyn AccessTokenProvider,
    account: &str,
    url: url::Url,
) -> Result<reqwest::Response> {
    for attempt in 0..2 {
        let token = auth.access_token(account).await?;
        let resp = http
            .get(url.clone())
            .bearer_auth(token)
            .send()
            .await
            .map_err(net_err)?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
            auth.invalidate(account);
            continue;
        }
        return Ok(resp);
    }
    unreachable!("loop returns on second attempt")
}

/// Turn a Google API response into `T` or a mapped [`Error`].
pub async fn json_or_error<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    if status.is_success() {
        return resp.json().await.map_err(net_err);
    }
    let body = resp.text().await.unwrap_or_default();
    let msg = format!("Google API {status}: {}", truncate(&body, 300));
    Err(match status.as_u16() {
        401 => Error::AuthRequired(msg),
        403 => Error::PermissionDenied(msg),
        404 => Error::NotFound(msg),
        429 | 500..=599 => Error::Network(msg),
        _ => Error::Other(msg),
    })
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_shape() {
        let p = Pkce::generate();
        assert_eq!(p.verifier.len(), 43);
        assert_eq!(
            p.challenge,
            URL_SAFE_NO_PAD.encode(Sha256::digest(p.verifier.as_bytes()))
        );
        assert_ne!(Pkce::generate().verifier, p.verifier);
    }

    #[test]
    fn id_token_email() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"Alice@Example.com","sub":"1"}"#);
        let jwt = format!("eyJhbGciOiJub25lIn0.{payload}.sig");
        assert_eq!(
            email_from_id_token(&jwt).as_deref(),
            Some("alice@example.com")
        );
        assert_eq!(email_from_id_token("garbage"), None);
    }
}
