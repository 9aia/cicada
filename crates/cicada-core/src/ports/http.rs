//! `Http` — injectable request/response with portable types only
//! (no `reqwest` types in signatures; ADR-0001 §1). Adapters
//! implement retry/backoff at this layer (ADR-0001 §4) and must
//! classify transport failures per ADR-0002 §8.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::domain::error::Result;

/// HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// GET.
    Get,
    /// POST.
    Post,
    /// PUT.
    Put,
    /// PATCH.
    Patch,
    /// DELETE.
    Delete,
}

impl HttpMethod {
    /// Wire form (`GET`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

/// A portable HTTP request — body is raw bytes so callers choose the
/// encoding (JSON, form, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRequest {
    /// Method.
    pub method: HttpMethod,
    /// Absolute URL.
    pub url: String,
    /// Header pairs, in order.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Request body, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    /// Shorthand constructor.
    pub fn new(method: HttpMethod, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    /// Add a header (builder style).
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Set a JSON body and `Content-Type` (builder style).
    ///
    /// # Errors
    /// [`CicadaError::Fatal`] if `value` can't serialize.
    pub fn json<T: Serialize>(self, value: &T) -> Result<Self> {
        self.json_bytes(serde_json::to_vec(value))
    }

    /// Shared tail of [`HttpRequest::json`] — non-generic so the
    /// `?` arm has a single instantiation (per-instantiation
    /// coverage would otherwise count an unexercised error path
    /// per `T`).
    fn json_bytes(mut self, body: std::result::Result<Vec<u8>, serde_json::Error>) -> Result<Self> {
        self.body =
            Some(body.map_err(|e| {
                crate::domain::error::CicadaError::fatal(format!("json body: {e}"))
            })?);
        self.headers
            .push(("content-type".to_owned(), "application/json".to_owned()));
        Ok(self)
    }
}

/// A portable HTTP response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpResponse {
    /// Status code.
    pub status: u16,
    /// Header pairs, in order received.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Raw body.
    #[serde(default)]
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// First header value matching `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// `true` for 2xx.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The injectable HTTP client. Adapters (`reqwest` first, recorded
/// fixtures for tests) implement retry with exponential backoff and
/// jitter *here* — below the port — so use cases never think about
/// rate limits.
#[async_trait]
pub trait Http: Send + Sync {
    /// Perform the request. Non-2xx statuses are **not** errors —
    /// they're responses; callers classify them. Transport-level
    /// failures are `Transient` (or `Auth` for TLS/identity issues).
    async fn send(&self, request: &HttpRequest) -> Result<HttpResponse>;
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn method_wire_names() {
        for (m, s) in [
            (HttpMethod::Get, "GET"),
            (HttpMethod::Post, "POST"),
            (HttpMethod::Put, "PUT"),
            (HttpMethod::Patch, "PATCH"),
            (HttpMethod::Delete, "DELETE"),
        ] {
            assert_eq!(m.as_str(), s);
        }
    }

    #[test]
    fn request_builders() {
        let req = HttpRequest::new(HttpMethod::Get, "http://x")
            .header("a", "b")
            .json(&serde_json::json!({"k": 1}))
            .unwrap();
        assert!(req.headers.contains(&("a".to_owned(), "b".to_owned())));
        assert!(
            req.headers
                .contains(&("content-type".to_owned(), "application/json".to_owned()))
        );
        assert_eq!(req.body.as_deref(), Some(br#"{"k":1}"#.as_slice()));
    }

    #[test]
    fn json_body_reports_serialize_errors() {
        // JSON object keys must be strings — a tuple-keyed map
        // can't serialize and surfaces the map_err arm.
        let mut bad = std::collections::BTreeMap::new();
        bad.insert((1_i32, 2_i32), "x");
        assert!(
            HttpRequest::new(HttpMethod::Post, "http://x")
                .json(&bad)
                .is_err()
        );
    }
}
