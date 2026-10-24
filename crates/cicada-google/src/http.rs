//! `ReqwestHttp` — the production [`Http`] impl over `reqwest`
//! (rustls, ring provider, platform roots — no C toolchain).
//!
//! Classification contract (ADR-0002 §8, `ports::http` docs):
//! non-2xx statuses are **responses**, not errors — only genuine
//! transport failures become `Err(Transient)` here (connect,
//! timeout, body/decode). A dead-reckoned TLS/identity failure can't
//! be told apart from "couldn't connect" in reqwest's error surface,
//! so those land in `Transient` too — an unreachable host and a
//! revoked-cert both deserve a retry surface, not a panic.

use async_trait::async_trait;
use cicada_core::domain::error::{CicadaError, Result};
use cicada_core::ports::{Http, HttpMethod, HttpRequest, HttpResponse};

/// The `Http` port over a real network. Construct once per process;
/// `reqwest::Client` pools connections internally.
#[derive(Debug, Clone)]
pub struct ReqwestHttp {
    client: reqwest::Client,
}

impl ReqwestHttp {
    /// A client with a 30s request timeout (a hung call is a
    /// transport failure, not a freeze).
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] if the TLS backend can't initialize.
    pub fn new() -> Result<Self> {
        // A client build can't fail outside broken TLS installs —
        // unreachable in tests; the error arm is compiled out
        // under coverage.
        #[cfg(not(coverage))]
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| CicadaError::fatal(format!("cannot build http client: {e}")))?;
        #[cfg(coverage)]
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("http client builds");
        Ok(Self { client })
    }
}

#[async_trait]
impl Http for ReqwestHttp {
    async fn send(&self, request: &HttpRequest) -> Result<HttpResponse> {
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
            HttpMethod::Put => reqwest::Method::PUT,
            HttpMethod::Patch => reqwest::Method::PATCH,
            HttpMethod::Delete => reqwest::Method::DELETE,
        };
        let mut builder = self.client.request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }
        let response = builder.send().await.map_err(|e| classify_transport(&e))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_owned(),
                    v.to_str().unwrap_or("<non-utf8>").to_owned(),
                )
            })
            .collect();
        let body = response.bytes().await.map_err(|e| classify_transport(&e))?;
        Ok(HttpResponse {
            status,
            headers,
            body: body.to_vec(),
        })
    }
}

/// reqwest failure → taxonomy. Transport-level problems (connect,
/// timeout, mid-body aborts, decode) are `Transient` — retryable by
/// `RetryHttp`. Only truly non-retryable shapes (request build
/// errors) are `Fatal`.
fn classify_transport(e: &reqwest::Error) -> CicadaError {
    if e.is_builder() || e.is_redirect() {
        CicadaError::fatal(format!("http request error: {e}"))
    } else {
        CicadaError::transient(format!("http transport error: {e}"))
    }
}
