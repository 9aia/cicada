//! Shared lifecycle plumbing: provider selection, state-path base
//! resolution, the file-backed fake provider, and the current-thread
//! runtime verbs build.
//!
//! Provider story: `fake` (default) is backed by [`FakeFileProvider`]
//! so the whole lifecycle persists across processes (test/demo only);
//! `google` constructs [`GoogleCalendarProvider`] over
//! `RetryHttp<ReqwestHttp>` with auth resolved from the environment
//! (see `cicada_google::GoogleAuthConfig::from_env`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use cicada_core::domain::calendar::Calendar;
use cicada_core::domain::capabilities::ProviderCapabilities;
use cicada_core::domain::error::Result as CicadaResult;
use cicada_core::domain::event::EventBody;
use cicada_core::domain::ids::{LocalId, RemoteId};
use cicada_core::domain::policy::WriteOptions;
use cicada_core::domain::state::RecordedCalendar;
use cicada_core::domain::time::TimeRange;
use cicada_core::ports::clock::SystemClock;
use cicada_core::ports::{
    AuthProvider, CalendarProvider, CalendarRef, Http, RemoteEvent, SecretStore,
};
use cicada_google::{
    FileSecretStore, GoogleAuthConfig, GoogleAuthProvider, GoogleCalendarProvider, ReqwestHttp,
    RetryHttp,
};
use cicada_schema::{Document, parse_document};
use cicada_testkit::FakeFileProvider;

/// The provider names interfaces accept.
const PROVIDERS: [&str; 2] = ["fake", "google"];

/// Validate a `--provider` value: `fake` (default) or `google`;
/// anything else is unknown. `who` labels the error (`cicada plan`,
/// `cicada-mcp`, …).
pub fn provider_name(name: Option<&str>, who: &str) -> Result<String, String> {
    match name.unwrap_or("fake") {
        "fake" => Ok("fake".to_owned()),
        "google" => Ok("google".to_owned()),
        other => Err(format!(
            "{who}: unknown provider {other:?} (supported: {})",
            PROVIDERS.join(", ")
        )),
    }
}

/// The selected provider backend — a runtime enum so both adapters
/// sit behind one `CalendarProvider` object.
#[derive(Debug)]
pub enum Backend {
    /// File-backed fake (test/demo). Boxed — it's much larger than
    /// the Google variant.
    Fake(Box<FakeFileProvider>),
    /// Google Calendar API v3.
    Google(GoogleCalendarProvider),
}

#[async_trait]
impl CalendarProvider for Backend {
    fn capabilities(&self) -> ProviderCapabilities {
        match self {
            Self::Fake(p) => p.capabilities(),
            Self::Google(p) => p.capabilities(),
        }
    }

    async fn list_events(
        &self,
        calendar: &CalendarRef,
        range: Option<TimeRange>,
    ) -> CicadaResult<Vec<RemoteEvent>> {
        match self {
            Self::Fake(p) => p.list_events(calendar, range).await,
            Self::Google(p) => p.list_events(calendar, range).await,
        }
    }

    async fn get_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
    ) -> CicadaResult<Option<RemoteEvent>> {
        match self {
            Self::Fake(p) => p.get_event(calendar, remote_id).await,
            Self::Google(p) => p.get_event(calendar, remote_id).await,
        }
    }

    async fn find_by_tag(
        &self,
        calendar: &CalendarRef,
        local_id: &LocalId,
    ) -> CicadaResult<Option<RemoteEvent>> {
        match self {
            Self::Fake(p) => p.find_by_tag(calendar, local_id).await,
            Self::Google(p) => p.find_by_tag(calendar, local_id).await,
        }
    }

    async fn create_event(
        &self,
        calendar: &CalendarRef,
        event: &EventBody,
        local_id: &LocalId,
        opts: &WriteOptions,
    ) -> CicadaResult<RemoteEvent> {
        match self {
            Self::Fake(p) => p.create_event(calendar, event, local_id, opts).await,
            Self::Google(p) => p.create_event(calendar, event, local_id, opts).await,
        }
    }

    async fn update_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
        event: &EventBody,
        opts: &WriteOptions,
    ) -> CicadaResult<RemoteEvent> {
        match self {
            Self::Fake(p) => p.update_event(calendar, remote_id, event, opts).await,
            Self::Google(p) => p.update_event(calendar, remote_id, event, opts).await,
        }
    }

    async fn delete_event(
        &self,
        calendar: &CalendarRef,
        remote_id: &RemoteId,
        opts: &WriteOptions,
    ) -> CicadaResult<()> {
        match self {
            Self::Fake(p) => p.delete_event(calendar, remote_id, opts).await,
            Self::Google(p) => p.delete_event(calendar, remote_id, opts).await,
        }
    }

    async fn free_busy(
        &self,
        calendar: &CalendarRef,
        range: TimeRange,
    ) -> CicadaResult<Vec<TimeRange>> {
        match self {
            Self::Fake(p) => p.free_busy(calendar, range).await,
            Self::Google(p) => p.free_busy(calendar, range).await,
        }
    }
}

/// Build the selected provider backend.
///
/// `fake` opens the file-backed fake; `google` builds the real
/// adapter: `GoogleAuthConfig::from_env` for credentials, the
/// file `SecretStore` for the refresh token, and
/// `RetryHttp<ReqwestHttp>` for transport (retries with backoff live
/// in the adapter layer, ADR-0001 §4).
pub fn provider(
    name: &str,
    fake_file: Option<&Path>,
    manifest_dir: &Path,
    who: &str,
) -> Result<Backend, String> {
    match name {
        "google" => google_provider(who),
        _ => fake_provider(fake_file, manifest_dir, who)
            .map(Box::new)
            .map(Backend::Fake),
    }
}

/// The `google` arm of [`provider`] — clear errors when credentials
/// aren't configured.
fn google_provider(who: &str) -> Result<Backend, String> {
    let config = GoogleAuthConfig::from_env().map_err(|e| format!("{who}: {e}"))?;
    let http: Arc<dyn Http> = Arc::new(RetryHttp::new(
        ReqwestHttp::new().map_err(|e| format!("{who}: {e}"))?,
    ));
    let secrets: Arc<dyn SecretStore> = Arc::new(FileSecretStore::default_location());
    let auth: Arc<dyn AuthProvider> = Arc::new(GoogleAuthProvider::new(
        config,
        http.clone(),
        Some(secrets),
        Arc::new(SystemClock),
    ));
    Ok(Backend::Google(GoogleCalendarProvider::new(http, auth)))
}

/// The directory `.cicada/` lives in — beside the manifest. When
/// explicit files were given, a leading `kind: Manifest` argument
/// anchors it to that manifest's directory; otherwise `root` (the
/// directory manifests are discovered in).
pub fn manifest_dir(root: &Path, files: &[PathBuf]) -> PathBuf {
    if let Some(first) = files.first() {
        if let Ok(source) = fs::read_to_string(first) {
            if let Ok(Document::Manifest(_)) = parse_document(&source) {
                return first
                    .parent()
                    .map_or_else(|| root.to_path_buf(), Path::to_path_buf);
            }
        }
    }
    root.to_path_buf()
}

/// Open the file-backed fake provider: an explicit path wins, else
/// `.cicada/fake-provider.json` beside the manifest (the same
/// `.cicada/` that hosts `state.yaml`).
pub fn fake_provider(
    fake_file: Option<&Path>,
    manifest_dir: &Path,
    who: &str,
) -> Result<FakeFileProvider, String> {
    let path = fake_file.map_or_else(
        || manifest_dir.join(".cicada").join("fake-provider.json"),
        Path::to_path_buf,
    );
    FakeFileProvider::open(&path).map_err(|e| format!("{who}: cannot open {}: {e}", path.display()))
}

/// A current-thread runtime — all port calls are `block_on`-driven.
/// For one-shot verbs; the MCP server runs its own long-lived
/// runtime instead.
pub fn runtime(who: &str) -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("{who}: cannot start runtime: {e}"))
}

/// Remote calendar binding for a logical calendar (ADR-0002 §9):
/// `metadata.calendarId` wins, then the recorded binding, then
/// `primary`.
pub fn remote_calendar(cal: &Calendar, recorded: Option<&RecordedCalendar>) -> CalendarRef {
    CalendarRef::new(
        cal.provider_calendar_id
            .clone()
            .or_else(|| {
                recorded.and_then(|r| {
                    if r.calendar_id.is_empty() {
                        None
                    } else {
                        Some(r.calendar_id.clone())
                    }
                })
            })
            .unwrap_or_else(|| "primary".to_owned()),
    )
}
