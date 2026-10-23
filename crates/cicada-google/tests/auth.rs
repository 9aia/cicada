//! `GoogleAuthProvider` tests — OAuth refresh, service-account JWT
//! exchange, token caching — all through `FixtureHttp`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cicada_core::domain::error::{CicadaError, Result};
use cicada_core::ports::{AuthProvider, HttpMethod, HttpResponse, SecretStore};
use cicada_google::{
    GoogleAuthConfig, GoogleAuthProvider, REFRESH_TOKEN_KEY, ServiceAccountKey, authorization_url,
};
use cicada_testkit::{FakeClock, FixtureHttp};
use jiff::{SignedDuration, Timestamp};

const SA_KEY_PEM: &str = include_str!("fixtures/sa_key.pem");

/// Epoch seconds for the test clock's `2026-10-23T00:00:00Z`.
fn clock_now_epoch() -> i64 {
    "2026-10-23T00:00:00Z"
        .parse::<Timestamp>()
        .unwrap()
        .as_second()
}

/// In-memory `SecretStore` for auth tests.
#[derive(Debug, Default)]
struct MapSecrets {
    map: Mutex<HashMap<String, String>>,
}

#[async_trait]
impl SecretStore for MapSecrets {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .cloned())
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(key);
        Ok(())
    }
}

fn token_response(token: &str, expires_in: u64, refresh: Option<&str>) -> HttpResponse {
    let mut body = serde_json::json!({
        "access_token": token,
        "expires_in": expires_in,
        "token_type": "Bearer",
    });
    if let Some(r) = refresh {
        body["refresh_token"] = r.into();
    }
    HttpResponse {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: serde_json::to_vec(&body).unwrap(),
    }
}

/// Decode a JWT's payload segment (assertions, not verification).
fn jwt_payload(jwt: &str) -> serde_json::Value {
    let payload = jwt.split('.').nth(1).expect("jwt payload");
    serde_json::from_slice(&base64url_decode(payload)).unwrap()
}

/// Minimal base64url decode (no padding) — test-only.
fn base64url_decode(s: &str) -> Vec<u8> {
    let mut table = [255u8; 256];
    for (i, c) in (b'A'..=b'Z')
        .chain(b'a'..=b'z')
        .chain(b'0'..=b'9')
        .chain(*b"-_")
        .enumerate()
    {
        table[c as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &b in s.as_bytes() {
        let v = table[b as usize];
        assert_ne!(v, 255, "bad base64url char");
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// Parse the form body of a recorded token request.
fn form_pairs(req: &cicada_core::ports::HttpRequest) -> HashMap<String, String> {
    let body = String::from_utf8(req.body.clone().unwrap()).unwrap();
    url::form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[tokio::test]
async fn static_token_passes_through() {
    let http = Arc::new(FixtureHttp::new());
    let auth = GoogleAuthProvider::new(
        GoogleAuthConfig::StaticToken {
            access_token: "ya29.static".into(),
        },
        http,
        None,
        Arc::new(FakeClock::at("2026-10-23T00:00:00Z".parse().unwrap())),
    );
    let creds = auth.credentials().await.unwrap();
    assert_eq!(creds.authorization_header(), "Bearer ya29.static");
    assert_eq!(creds.expires_at, None);
}

#[tokio::test]
async fn oauth_refresh_uses_store_and_caches() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_fn(
        |r| r.method == HttpMethod::Post && r.url.contains("oauth2"),
        Ok(token_response("ya29.fresh", 3600, Some("rt-rotated"))),
    );
    let secrets = Arc::new(MapSecrets::default());
    secrets.set(REFRESH_TOKEN_KEY, "rt-1").await.unwrap();
    let clock = Arc::new(FakeClock::at("2026-10-23T00:00:00Z".parse().unwrap()));
    let auth = GoogleAuthProvider::new(
        GoogleAuthConfig::OAuthClient {
            client_id: "cid".into(),
            client_secret: "csec".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            refresh_token: None,
        },
        http.clone(),
        Some(secrets.clone()),
        clock.clone(),
    );

    let creds = auth.credentials().await.unwrap();
    assert_eq!(creds.authorization_header(), "Bearer ya29.fresh");
    assert!(creds.expires_at.is_some());

    // Request shape: refresh_token grant.
    let req = &http.requests()[0];
    assert_eq!(req.method, HttpMethod::Post);
    let form = form_pairs(req);
    assert_eq!(form["grant_type"], "refresh_token");
    assert_eq!(form["refresh_token"], "rt-1");
    assert_eq!(form["client_id"], "cid");
    assert_eq!(form["client_secret"], "csec");

    // Rotated refresh token persisted.
    assert_eq!(
        secrets.get(REFRESH_TOKEN_KEY).await.unwrap().as_deref(),
        Some("rt-rotated")
    );

    // Cached: no second request.
    let again = auth.credentials().await.unwrap();
    assert_eq!(again.authorization_header(), "Bearer ya29.fresh");
    assert_eq!(http.requests().len(), 1);

    // Advance past expiry → a new exchange.
    http.expect_fn(
        |r| r.method == HttpMethod::Post,
        Ok(token_response("ya29.newer", 3600, None)),
    );
    clock.advance(SignedDuration::from_hours(2));
    let creds = auth.credentials().await.unwrap();
    assert_eq!(creds.authorization_header(), "Bearer ya29.newer");
    // …and it used the *rotated* refresh token.
    let form = form_pairs(&http.requests()[1]);
    assert_eq!(form["refresh_token"], "rt-rotated");
    assert_eq!(http.requests().len(), 2);
    assert!(http.is_exhausted());
}

#[tokio::test]
async fn oauth_without_refresh_token_is_auth_error() {
    let http = Arc::new(FixtureHttp::new());
    let auth = GoogleAuthProvider::new(
        GoogleAuthConfig::OAuthClient {
            client_id: "cid".into(),
            client_secret: "csec".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            refresh_token: None,
        },
        http.clone(),
        Some(Arc::new(MapSecrets::default())),
        Arc::new(FakeClock::at(Timestamp::now())),
    );
    let err = auth.credentials().await.unwrap_err();
    assert!(matches!(err, CicadaError::Auth(_)), "got {err:?}");
    assert!(err.to_string().contains("provider add google"));
    assert!(http.requests().is_empty());
}

#[tokio::test]
async fn service_account_jwt_grant() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_fn(
        |r| r.method == HttpMethod::Post && r.url.contains("oauth2"),
        Ok(token_response("ya29.sa", 3599, None)),
    );
    let key = ServiceAccountKey {
        kind: Some("service_account".into()),
        client_email: "cicada@proj.iam.gserviceaccount.com".into(),
        private_key: SA_KEY_PEM.to_owned(),
        token_uri: None,
        project_id: Some("proj".into()),
    };
    let clock = Arc::new(FakeClock::at("2026-10-23T00:00:00Z".parse().unwrap()));
    let auth = GoogleAuthProvider::new(
        GoogleAuthConfig::ServiceAccount {
            key,
            subject: Some("admin@example.com".into()),
        },
        http.clone(),
        None,
        clock,
    );
    let creds = auth.credentials().await.unwrap();
    assert_eq!(creds.authorization_header(), "Bearer ya29.sa");

    let form = form_pairs(&http.requests()[0]);
    assert_eq!(
        form["grant_type"],
        "urn:ietf:params:oauth:grant-type:jwt-bearer"
    );
    let payload = jwt_payload(&form["assertion"]);
    assert_eq!(payload["iss"], "cicada@proj.iam.gserviceaccount.com");
    assert_eq!(payload["scope"], "https://www.googleapis.com/auth/calendar");
    assert_eq!(payload["aud"], "https://oauth2.googleapis.com/token");
    assert_eq!(payload["sub"], "admin@example.com");
    let iat = payload["iat"].as_i64().unwrap();
    let exp = payload["exp"].as_i64().unwrap();
    assert_eq!(exp - iat, 3600);
    // iat is the injected clock's now: 2026-10-23T00:00:00Z.
    assert_eq!(iat, clock_now_epoch());
    // Header says RS256.
    let header = form["assertion"].split('.').next().unwrap();
    let header: serde_json::Value = serde_json::from_slice(&base64url_decode(header)).unwrap();
    assert_eq!(header["alg"], "RS256");
    assert_eq!(header["typ"], "JWT");
}

#[tokio::test]
async fn token_endpoint_error_is_auth() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_fn(
        |r| r.method == HttpMethod::Post,
        Ok(HttpResponse {
            status: 400,
            headers: vec![],
            body: br#"{"error":"invalid_grant","error_description":"Token has been revoked."}"#
                .to_vec(),
        }),
    );
    let auth = GoogleAuthProvider::new(
        GoogleAuthConfig::OAuthClient {
            client_id: "cid".into(),
            client_secret: "csec".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            refresh_token: Some("rt".into()),
        },
        http,
        None,
        Arc::new(FakeClock::at(Timestamp::now())),
    );
    let err = auth.credentials().await.unwrap_err();
    assert!(matches!(err, CicadaError::Auth(_)), "got {err:?}");
    assert!(err.to_string().contains("revoked"));
}

#[test]
fn authorization_url_has_offline_params() {
    let url = authorization_url("cid", "http://127.0.0.1", "scope-x");
    for needle in [
        "client_id=cid",
        "redirect_uri=http%3A%2F%2F127.0.0.1",
        "response_type=code",
        "scope=scope-x",
        "access_type=offline",
        "prompt=consent",
    ] {
        assert!(url.contains(needle), "missing {needle} in {url}");
    }
    assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
}

#[tokio::test]
async fn exchange_code_posts_authorization_code() {
    let http = Arc::new(FixtureHttp::new());
    http.expect_fn(
        |r| r.method == HttpMethod::Post,
        Ok(token_response("ya29.code", 3600, Some("rt-new"))),
    );
    let exchange = GoogleAuthProvider::exchange_code(
        http.as_ref(),
        "https://oauth2.googleapis.com/token",
        "cid",
        "csec",
        "4/code-abc",
        "http://127.0.0.1",
        "2026-10-23T00:00:00Z".parse().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(exchange.refresh_token.as_deref(), Some("rt-new"));
    let form = form_pairs(&http.requests()[0]);
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "4/code-abc");
    assert_eq!(form["redirect_uri"], "http://127.0.0.1");
}
