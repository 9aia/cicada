//! `GoogleAuthProvider` — Google credentials behind the
//! [`AuthProvider`] port (ADR-0001 §2).
//!
//! Three credential modes, resolved by [`GoogleAuthConfig::from_env`]
//! in this order:
//!
//! 1. **Service account** — `CICADA_GOOGLE_SERVICE_ACCOUNT_JSON` or
//!    `GOOGLE_APPLICATION_CREDENTIALS` points at a Google service-
//!    account key JSON; an RS256 JWT bearer grant mints access
//!    tokens. `CICADA_GOOGLE_SUBJECT` optionally sets `sub` for
//!    domain-wide delegation.
//! 2. **OAuth installed app** — `CICADA_GOOGLE_CLIENT_ID` +
//!    `CICADA_GOOGLE_CLIENT_SECRET`; the refresh token comes from the
//!    [`SecretStore`] (`google/refresh-token`) or
//!    `CICADA_GOOGLE_REFRESH_TOKEN`. [`authorization_url`] prints the
//!    consent link and [`GoogleAuthProvider::exchange_code`] trades
//!    the pasted code for a refresh token — the `cicada provider add
//!    google` verb is a later build step; the plumbing lives here.
//! 3. **Static access token** — `CICADA_GOOGLE_ACCESS_TOKEN` as an
//!    escape hatch (no refresh; expires in ~1h).
//!
//! Tokens are cached in memory and refreshed 60s before `expires_at`
//! against the injected [`Clock`]. All failures surface as
//! [`CicadaError::Auth`] — never `Transient`, so callers don't
//! retry a flow that needs a human.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cicada_core::domain::error::{CicadaError, Result};
use cicada_core::ports::auth::Token;
use cicada_core::ports::{AuthProvider, Clock, Credentials, Http, HttpMethod, HttpRequest};
use cicada_core::ports::{HttpResponse, SecretStore};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use url::Url;

/// Google's OAuth 2.0 authorization endpoint (consent screen).
pub const AUTHORIZATION_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Google's token endpoint.
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// The calendar scope cicada needs (read/write events + freeBusy).
pub const CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar";
/// `SecretStore` key holding the OAuth refresh token.
pub const REFRESH_TOKEN_KEY: &str = "google/refresh-token";
/// `SecretStore` key holding the service-account key JSON persisted
/// by `cicada provider add google --service-account`. Read directly
/// off the [`crate::FileSecretStore`] path — `from_env` stays sync.
pub const SERVICE_ACCOUNT_KEY: &str = "google/service-account";
/// Loopback redirect for the installed-app flow (`http://127.0.0.1` —
/// Google maps any port for installed clients).
pub const LOOPBACK_REDIRECT: &str = "http://127.0.0.1";

/// Refresh this much earlier than `expires_at` — a request in
/// flight shouldn't ride a token into expiry.
const EXPIRY_SKEW: SignedDuration = SignedDuration::from_secs(60);

/// Parsed service-account key JSON (`type: service_account` file
/// from the Google Cloud console).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ServiceAccountKey {
    /// `service_account` — validated, not used.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// `client_email` — the JWT `iss`.
    pub client_email: String,
    /// PEM PKCS#8 private key — signs the JWT assertion.
    pub private_key: String,
    /// Token endpoint from the key file; [`TOKEN_URL`] when absent.
    #[serde(default)]
    pub token_uri: Option<String>,
    /// Project id — informational.
    #[serde(default)]
    pub project_id: Option<String>,
}

impl ServiceAccountKey {
    /// The token endpoint this key targets.
    pub fn token_uri(&self) -> &str {
        self.token_uri.as_deref().unwrap_or(TOKEN_URL)
    }
}

/// How the provider authenticates. Resolve with
/// [`GoogleAuthConfig::from_env`] or construct directly in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoogleAuthConfig {
    /// A bearer token used verbatim — no refresh possible.
    StaticToken {
        /// The access token.
        access_token: String,
    },
    /// OAuth installed-app credentials; the refresh token comes from
    /// `refresh_token` or the [`SecretStore`].
    OAuthClient {
        /// Installed-app client id.
        client_id: String,
        /// Installed-app client secret.
        client_secret: String,
        /// Token endpoint (default [`TOKEN_URL`]; override in tests).
        token_url: String,
        /// Inline refresh token (`CICADA_GOOGLE_REFRESH_TOKEN`) —
        /// takes precedence over the [`SecretStore`].
        refresh_token: Option<String>,
    },
    /// Service-account JWT bearer grant.
    ServiceAccount {
        /// The parsed key JSON.
        key: ServiceAccountKey,
        /// Domain-wide delegation subject (`sub`), if any.
        subject: Option<String>,
    },
}

impl GoogleAuthConfig {
    /// Resolve auth config from the environment. See module docs for
    /// the variable list and precedence.
    ///
    /// # Errors
    /// `String` — a configuration error listing every supported
    /// variable when nothing usable is set.
    pub fn from_env() -> std::result::Result<Self, String> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());

        if let Some(path) = var("CICADA_GOOGLE_SERVICE_ACCOUNT_JSON")
            .or_else(|| var("GOOGLE_APPLICATION_CREDENTIALS"))
        {
            let source = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read service-account key {path}: {e}"))?;
            let key: ServiceAccountKey = serde_json::from_str(&source)
                .map_err(|e| format!("service-account key {path} is malformed: {e}"))?;
            return Ok(Self::ServiceAccount {
                key,
                subject: var("CICADA_GOOGLE_SUBJECT"),
            });
        }
        if let Some(token) = var("CICADA_GOOGLE_ACCESS_TOKEN") {
            return Ok(Self::StaticToken {
                access_token: token,
            });
        }
        if let (Some(client_id), Some(client_secret)) = (
            var("CICADA_GOOGLE_CLIENT_ID"),
            var("CICADA_GOOGLE_CLIENT_SECRET"),
        ) {
            return Ok(Self::OAuthClient {
                client_id,
                client_secret,
                token_url: var("CICADA_GOOGLE_TOKEN_URL").unwrap_or_else(|| TOKEN_URL.to_owned()),
                refresh_token: var("CICADA_GOOGLE_REFRESH_TOKEN"),
            });
        }
        // `cicada provider add google --service-account` persists the
        // key JSON at the SecretStore location — the last-resort
        // credential source after env. Read it off the
        // `FileSecretStore` path directly; `from_env` stays sync.
        let stored_path =
            crate::secrets::FileSecretStore::default_location().key_path(SERVICE_ACCOUNT_KEY);
        if let Ok(source) = std::fs::read_to_string(&stored_path) {
            let key: ServiceAccountKey = serde_json::from_str(&source).map_err(|e| {
                format!(
                    "persisted service-account key {} is malformed: {e} — \
                     fix it or delete the file",
                    stored_path.display()
                )
            })?;
            return Ok(Self::ServiceAccount {
                key,
                subject: var("CICADA_GOOGLE_SUBJECT"),
            });
        }
        Err(format!(
            "no google credentials configured — set one of:\n  \
             - {sa} (or GOOGLE_APPLICATION_CREDENTIALS): service-account key JSON path\n  \
             - {id} + {secret} (+ a refresh token via `{add}` or {rt}): OAuth installed app\n  \
             - {at}: a static access token (expires ~1h)",
            sa = "CICADA_GOOGLE_SERVICE_ACCOUNT_JSON",
            id = "CICADA_GOOGLE_CLIENT_ID",
            secret = "CICADA_GOOGLE_CLIENT_SECRET",
            add = "cicada provider add google",
            rt = "CICADA_GOOGLE_REFRESH_TOKEN",
            at = "CICADA_GOOGLE_ACCESS_TOKEN",
        ))
    }
}

/// What a token-endpoint call returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenExchange {
    /// The minted access token.
    pub credentials: Credentials,
    /// A rotated/new refresh token, when Google returns one —
    /// persist it into the [`SecretStore`].
    pub refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// The authorization URL to print for the installed-app flow
/// (`access_type=offline` so a refresh token comes back,
/// `prompt=consent` so it comes back *every* time).
pub fn authorization_url(client_id: &str, redirect_uri: &str, scope: &str) -> String {
    // AUTHORIZATION_URL is a compile-time constant — a parse failure
    // here is a bug, so degrade to a bare string rather than panic.
    let Ok(mut url) = Url::parse(AUTHORIZATION_URL) else {
        return AUTHORIZATION_URL.to_owned();
    };
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", scope)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");
    String::from(url)
}

/// `application/x-www-form-urlencoded` body from pairs.
fn form_body(pairs: &[(&str, &str)]) -> Vec<u8> {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().copied())
        .finish()
        .into_bytes()
}

/// A form `POST` to the token endpoint; classifies the response.
async fn token_request(
    http: &dyn Http,
    token_url: &str,
    pairs: &[(&str, &str)],
) -> Result<TokenResponse> {
    let mut req = HttpRequest::new(HttpMethod::Post, token_url);
    req.headers.push((
        "content-type".to_owned(),
        "application/x-www-form-urlencoded".to_owned(),
    ));
    req.body = Some(form_body(pairs));
    let resp = http.send(&req).await.map_err(|e| match e {
        CicadaError::Transient { message, .. } => {
            CicadaError::auth(format!("token endpoint unreachable: {message}"))
        }
        other => other,
    })?;
    if !resp.is_success() {
        return Err(classify_token_error(&resp));
    }
    serde_json::from_slice(&resp.body)
        .map_err(|e| CicadaError::auth(format!("malformed google token response: {e}")))
}

/// Token-endpoint failures are `Auth` (invalid_grant needs a human,
/// and so does a revoked client); 429/5xx keep their `Transient`
/// class — a retry may fix those.
fn classify_token_error(resp: &HttpResponse) -> CicadaError {
    #[derive(Deserialize)]
    struct ErrBody {
        error: Option<String>,
        error_description: Option<String>,
    }
    let parsed = serde_json::from_slice::<ErrBody>(&resp.body).ok();
    let detail = parsed
        .as_ref()
        .and_then(|b| b.error_description.clone().or_else(|| b.error.clone()))
        .unwrap_or_else(|| {
            String::from_utf8_lossy(&resp.body)
                .trim()
                .chars()
                .take(200)
                .collect()
        });
    match resp.status {
        429 | 500..=599 => CicadaError::transient(format!(
            "google token endpoint error ({}): {detail}",
            resp.status
        )),
        _ => CicadaError::auth(format!(
            "google token endpoint error ({}): {detail}",
            resp.status
        )),
    }
}

fn to_exchange(resp: TokenResponse, now: Timestamp) -> TokenExchange {
    let expires_at = resp
        .expires_in
        .and_then(|secs| now.checked_add(SignedDuration::from_secs(secs)).ok());
    TokenExchange {
        credentials: Credentials {
            scheme: resp
                .token_type
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "Bearer".to_owned()),
            access_token: Token::new(resp.access_token),
            expires_at,
        },
        refresh_token: resp.refresh_token,
    }
}

/// The service-account JWT assertion (`iss`/`scope`/`aud`/`iat`/`exp`/`sub`).
#[derive(Debug, Serialize)]
struct JwtClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: i64,
    exp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    sub: Option<&'a str>,
}

/// Sign the JWT bearer assertion for `key`.
fn jwt_assertion(
    key: &ServiceAccountKey,
    subject: Option<&str>,
    now: Timestamp,
    scope: &str,
) -> Result<String> {
    let claims = JwtClaims {
        iss: &key.client_email,
        scope,
        aud: key.token_uri(),
        iat: now.as_second(),
        exp: (now + SignedDuration::from_hours(1)).as_second(),
        sub: subject,
    };
    let encoding_key = jsonwebtoken::EncodingKey::from_rsa_pem(key.private_key.as_bytes())
        .map_err(|e| {
            CicadaError::auth(format!("service-account private key is not RSA PEM: {e}"))
        })?;
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
        &claims,
        &encoding_key,
    )
    .map_err(|e| CicadaError::auth(format!("cannot sign service-account JWT: {e}")))
}

/// `AuthProvider` for Google — see module docs.
pub struct GoogleAuthProvider {
    config: GoogleAuthConfig,
    http: Arc<dyn Http>,
    secrets: Option<Arc<dyn SecretStore>>,
    clock: Arc<dyn Clock>,
    scope: String,
    /// The cached access token — refreshed lazily inside
    /// `credentials()`.
    cached: Mutex<Option<Credentials>>,
}

impl std::fmt::Debug for GoogleAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleAuthProvider")
            .field("config", &self.config)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl GoogleAuthProvider {
    /// An auth provider over `http` (token endpoint calls), the
    /// optional [`SecretStore`] (refresh-token persistence), and
    /// `clock` (expiry).
    pub fn new(
        config: GoogleAuthConfig,
        http: Arc<dyn Http>,
        secrets: Option<Arc<dyn SecretStore>>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            config,
            http,
            secrets,
            clock,
            scope: CALENDAR_SCOPE.to_owned(),
            cached: Mutex::new(None),
        }
    }

    /// A non-calendar scope (`calendar.readonly`, …) — rare.
    #[must_use]
    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    /// The cached credentials, if still fresh at `now`.
    fn cached_credentials(&self, now: Timestamp) -> Option<Credentials> {
        self.cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .filter(|c| !c.is_expired(now + EXPIRY_SKEW))
    }

    fn cache(&self, creds: Credentials) {
        *self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(creds);
    }

    /// The refresh token: inline config first, then the store.
    async fn refresh_token(&self, inline: Option<&String>) -> Result<String> {
        if let Some(t) = inline {
            return Ok(t.clone());
        }
        match &self.secrets {
            Some(store) => store
                .get(REFRESH_TOKEN_KEY)
                .await?
                .ok_or_else(missing_refresh_token),
            None => Err(missing_refresh_token()),
        }
    }

    /// `exchange_code` for the installed-app flow: POST the
    /// authorization code, return the token bundle (its
    /// `refresh_token` should be persisted to the [`SecretStore`]).
    ///
    /// # Errors
    /// Token-endpoint failures classified per ADR-0002 §8.
    pub async fn exchange_code(
        http: &dyn Http,
        token_url: &str,
        client_id: &str,
        client_secret: &str,
        code: &str,
        redirect_uri: &str,
        now: Timestamp,
    ) -> Result<TokenExchange> {
        let resp = token_request(
            http,
            token_url,
            &[
                ("code", code),
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("redirect_uri", redirect_uri),
                ("grant_type", "authorization_code"),
            ],
        )
        .await?;
        Ok(to_exchange(resp, now))
    }
}

/// The "no refresh token" error — one string for both branches.
fn missing_refresh_token() -> CicadaError {
    CicadaError::auth(
        "no google refresh token — run `cicada provider add google` to authorize, \
         or set CICADA_GOOGLE_REFRESH_TOKEN",
    )
}

#[async_trait]
impl AuthProvider for GoogleAuthProvider {
    async fn credentials(&self) -> Result<Credentials> {
        let now = self.clock.now();
        match &self.config {
            GoogleAuthConfig::StaticToken { access_token } => Ok(Credentials {
                scheme: "Bearer".to_owned(),
                access_token: Token::new(access_token.clone()),
                expires_at: None,
            }),
            GoogleAuthConfig::OAuthClient {
                client_id,
                client_secret,
                token_url,
                refresh_token,
            } => {
                if let Some(c) = self.cached_credentials(now) {
                    return Ok(c);
                }
                let refresh = self.refresh_token(refresh_token.as_ref()).await?;
                let resp = token_request(
                    self.http.as_ref(),
                    token_url,
                    &[
                        ("client_id", client_id.as_str()),
                        ("client_secret", client_secret.as_str()),
                        ("refresh_token", refresh.as_str()),
                        ("grant_type", "refresh_token"),
                    ],
                )
                .await?;
                // Google may rotate the refresh token — persist it.
                if let (Some(store), Some(new_refresh)) = (&self.secrets, &resp.refresh_token) {
                    if let Err(e) = store.set(REFRESH_TOKEN_KEY, new_refresh).await {
                        tracing::warn!("cannot persist rotated google refresh token: {e}");
                    }
                }
                let exchange = to_exchange(resp, now);
                self.cache(exchange.credentials.clone());
                Ok(exchange.credentials)
            }
            GoogleAuthConfig::ServiceAccount { key, subject } => {
                if let Some(c) = self.cached_credentials(now) {
                    return Ok(c);
                }
                let assertion = jwt_assertion(key, subject.as_deref(), now, &self.scope)?;
                let resp = token_request(
                    self.http.as_ref(),
                    key.token_uri(),
                    &[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                        ("assertion", assertion.as_str()),
                    ],
                )
                .await?;
                let exchange = to_exchange(resp, now);
                self.cache(exchange.credentials.clone());
                Ok(exchange.credentials)
            }
        }
    }
}

/// Resolve the secrets directory: `$CICADA_SECRETS_DIR`, then
/// `$XDG_STATE_HOME/cicada/secrets`, then `~/.local/state/cicada/secrets`
/// (ADR-0002 §7).
pub fn default_secrets_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("CICADA_SECRETS_DIR") {
        return std::path::PathBuf::from(dir);
    }
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        return Path::new(&state).join("cicada/secrets");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Path::new(&home).join(".local/state/cicada/secrets");
    }
    std::path::PathBuf::from(".cicada/secrets")
}
