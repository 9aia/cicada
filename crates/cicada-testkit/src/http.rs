//! `FixtureHttp` — a scripted [`Http`] for provider-adapter tests.
//!
//! Expectations are declared up front; each `send` records the
//! request, then answers with the first still-unconsumed expectation
//! whose matcher accepts it. A request no expectation matches is a
//! `Fatal` error carrying the method+URL so the failure is obvious.
//!
//! Matchers see the *whole* request — headers and body — so tests can
//! pin `sendUpdates`, `If-Match`, or auth headers on specific calls.
//!
//! ```no_run
//! # use cicada_core::ports::http::{HttpMethod, HttpResponse};
//! # use cicada_testkit::FixtureHttp;
//! let http = FixtureHttp::new();
//! http.expect_json(
//!     HttpMethod::Get,
//!     "/calendars/primary/events",
//!     &serde_json::json!({"items": []}),
//! );
//! ```

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cicada_core::domain::error::{CicadaError, Result};
use cicada_core::ports::{Http, HttpMethod, HttpRequest, HttpResponse};

/// A canned response (or a forced failure) behind a matcher.
pub struct Expectation {
    matcher: Box<dyn Fn(&HttpRequest) -> bool + Send + Sync>,
    outcome: Result<HttpResponse>,
}

impl std::fmt::Debug for Expectation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Expectation")
            .field("outcome", &self.outcome)
            .finish_non_exhaustive()
    }
}

/// Scripted HTTP port impl. See module docs. `Clone` produces a
/// handle *sharing* the same expectations and request log — handy
/// when a decorator (e.g. a retry wrapper) needs an owned value.
#[derive(Debug, Default, Clone)]
pub struct FixtureHttp {
    /// Unconsumed expectations, in declaration order.
    expectations: Arc<Mutex<VecDeque<Expectation>>>,
    /// Every request `send` was called with, in order.
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl FixtureHttp {
    /// An empty fixture — every request fails `Fatal` until
    /// expectations are pushed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Expect `method` on any URL containing `url_fragment` (query
    /// string included); respond with `response`.
    pub fn expect(&self, method: HttpMethod, url_fragment: &str, response: HttpResponse) -> &Self {
        let fragment = url_fragment.to_owned();
        self.expect_fn(
            move |req| req.method == method && req.url.contains(&fragment),
            Ok(response),
        )
    }

    /// Expect `method` + URL fragment; respond with a JSON body and
    /// `Content-Type: application/json`.
    pub fn expect_json(
        &self,
        method: HttpMethod,
        url_fragment: &str,
        body: &serde_json::Value,
    ) -> &Self {
        let body = serde_json::to_vec(body).unwrap_or_default();
        self.expect(
            method,
            url_fragment,
            HttpResponse {
                status: 200,
                headers: vec![("content-type".to_owned(), "application/json".to_owned())],
                body,
            },
        )
    }

    /// Expect a request matching `matcher`; `outcome` may be an
    /// `Err` to simulate transport failure.
    pub fn expect_fn(
        &self,
        matcher: impl Fn(&HttpRequest) -> bool + Send + Sync + 'static,
        outcome: Result<HttpResponse>,
    ) -> &Self {
        self.expectations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(Expectation {
                matcher: Box::new(matcher),
                outcome,
            });
        self
    }

    /// Every request received so far, in order — for assertions
    /// after a provider op.
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// `true` when every declared expectation has been consumed —
    /// lets a test prove the adapter didn't silently skip a call.
    pub fn is_exhausted(&self) -> bool {
        self.expectations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }
}

#[async_trait]
impl Http for FixtureHttp {
    async fn send(&self, request: &HttpRequest) -> Result<HttpResponse> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());
        let mut expectations = self
            .expectations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = expectations.iter().position(|e| (e.matcher)(request));
        match index.and_then(|i| expectations.remove(i)) {
            Some(e) => e.outcome,
            None => Err(CicadaError::fatal(format!(
                "FixtureHttp: no expectation matched {} {}",
                request.method.as_str(),
                request.url
            ))),
        }
    }
}
