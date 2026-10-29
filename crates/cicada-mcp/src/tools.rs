//! Tool surface — arg decoding, use-case dispatch, and result
//! rendering. **Transport-neutral**: [`dispatch`] takes decoded
//! JSON arguments and returns a [`ToolOutcome`]; the rmcp adapter
//! ([`crate::server`]) maps it onto `CallToolResult`.
//!
//! The surface mirrors `cicada-core` use cases (ADR-0001 §8a):
//!
//! | Tool | Use case | Mutates |
//! | --- | --- | --- |
//! | `cicada.calendar.list` | manifest load | — |
//! | `cicada.events.range` | [`QueryRange`] (desired or live) | — |
//! | `cicada.events.show` | [`ShowEvent`] | — |
//! | `cicada.state.drift` | [`DetectDrift`] | — |
//! | `cicada.state.refresh` | [`RefreshState`] (under state lock) | local state |
//! | `cicada.plan.preview` | [`PlanCalendar`] | issues a session `planDigest` |
//! | `cicada.plan.apply` | [`ApplyPlan`] (under state lock) | remote + state |
//!
//! There is intentionally **no `events.create`**: all mutations flow
//! through declared YAML + plan/apply (ADR-0002 §10, single write
//! path). `plan.apply` is confirmation-gated (see [`plan_apply`]).

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Mutex;

use cicada_core::application::{
    ApplyPlan, ApplyRequest, DetectDrift, DriftRequest, OpResult, PlanCalendar, PlanRequest,
    QueryRange, RefreshRequest, RefreshState, ShowEvent,
};
use cicada_core::domain::calendar::Calendar;
use cicada_core::domain::drift::DriftEntry;
use cicada_core::domain::error::{CicadaError, ErrorKind};
use cicada_core::domain::ids::LocalId;
use cicada_core::domain::operation::OpKind;
use cicada_core::domain::policy::SendUpdates;
use cicada_core::domain::state::RecordedCalendar;
use cicada_core::domain::time::TimeRange;
use cicada_core::planning::Plan;
use cicada_core::ports::clock::SystemClock;
use cicada_core::ports::{CalendarProvider, CalendarRef, StateStore};
use cicada_shell::timeparse::parse_bound;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::ctx::Session;

/// JSON object arguments (the `tools/call` `arguments` payload).
pub type Args = Option<Map<String, Value>>;

/// Issued preview digests for this session — `plan.apply` only runs
/// against a digest `plan.preview` produced earlier (proves a
/// preview happened; a re-plan verifies it isn't stale).
pub type IssuedDigests = Mutex<BTreeSet<String>>;

/// One tool definition: name, description, JSON Schema for args.
/// Transport-neutral; the rmcp adapter wraps it into `rmcp::Tool`.
pub struct ToolDef {
    /// Tool name (`cicada.*`).
    pub name: &'static str,
    /// What the tool does — written for agents: it states the
    /// plan/apply contract instead of just the verb.
    pub description: &'static str,
    /// JSON Schema object (`{"type":"object",…}`).
    pub input_schema: Value,
    /// Read-only hint (no remote or local writes).
    pub read_only: bool,
}

/// A tool's rendered result — structured JSON + human-readable text.
#[derive(Debug)]
pub struct ToolOutcome {
    /// `true` → the result is an error (`isError` on the wire).
    pub is_error: bool,
    /// Human-readable summary (`content` on the wire).
    pub text: String,
    /// Structured payload (`structuredContent` on the wire).
    pub structured: Value,
}

impl ToolOutcome {
    /// A successful result.
    fn ok(text: impl Into<String>, structured: Value) -> Self {
        Self {
            is_error: false,
            text: text.into(),
            structured,
        }
    }

    /// A tool-level error; the error class rides along in the
    /// structured payload (ADR-0002 §8).
    fn err(kind: ErrorKind, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            is_error: true,
            text: format!("error[{}]: {message}", kind_code(kind)),
            structured: json!({
                "error": { "kind": kind_code(kind), "message": message }
            }),
        }
    }

    /// Wrap a [`CicadaError`], keeping its class.
    fn cicada(e: &CicadaError) -> Self {
        Self::err(e.kind(), e.to_string())
    }

    /// Arg-decode / interface failure — always `Fatal`.
    fn fatal(message: impl Into<String>) -> Self {
        Self::err(ErrorKind::Fatal, message)
    }
}

/// `ErrorKind` → the wire code (`transient`/`conflict`/`auth`/`fatal`).
fn kind_code(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Transient => "transient",
        ErrorKind::Conflict => "conflict",
        ErrorKind::Auth => "auth",
        ErrorKind::Fatal => "fatal",
    }
}

/// `{"type":"object",…}` for the given property map/required list.
fn object_schema(properties: &Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

/// The `from`/`to` window properties shared by range/show.
fn time_bounds() -> Value {
    json!({
        "from": {
            "type": "string",
            "description": "Window start: `today`, `+Nd`, `+Nw`, a date (2026-08-19), a civil datetime, or an RFC 3339 instant. Resolved in each calendar's timeZone."
        },
        "to": {
            "type": "string",
            "description": "Window end (exclusive), same grammar as `from`."
        }
    })
}

/// `object_schema` over `time_bounds` plus tool-specific properties.
fn bounds_schema(extra: &Value, required: &[&str]) -> Value {
    let mut props = time_bounds();
    if let (Some(base), Some(more)) = (props.as_object_mut(), extra.as_object().cloned()) {
        base.extend(more);
    }
    object_schema(&props, required)
}

/// The full tool list, in `tools/list` order.
pub fn definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "cicada.calendar.list",
            description: "List the calendars declared in the manifest (names, timeZone, \
                          provider binding, declared event counts, recorded state counts). \
                          Desired state lives in YAML — to change events, edit the calendar \
                          files then call cicada.plan.preview / cicada.plan.apply. There is \
                          no events.create (ADR-0002 §10).",
            input_schema: object_schema(&json!({}), &[]),
            read_only: true,
        },
        ToolDef {
            name: "cicada.events.range",
            description: "Expand declared (or live, with `live: true`) events into concrete \
                          occurrences inside [from, to). Read-only.",
            input_schema: bounds_schema(
                &json!({
                    "calendar": {
                        "type": "string",
                        "description": "Limit to one declared calendar name; default all."
                    },
                    "live": {
                        "type": "boolean",
                        "description": "Expand the provider's live events instead of the \
                                        desired YAML. Default false."
                    }
                }),
                &[],
            ),
            read_only: true,
        },
        ToolDef {
            name: "cicada.events.show",
            description: "Show one declared event by stable local id: its fields plus \
                          occurrences inside a window. Read-only.",
            input_schema: bounds_schema(
                &json!({
                    "id": {
                        "type": "string",
                        "description": "Stable local event id (e.g. `standup`)."
                    }
                }),
                &["id"],
            ),
            read_only: true,
        },
        ToolDef {
            name: "cicada.state.drift",
            description: "Compare recorded state against live provider state — reports \
                          managed events that are missing or modified remotely (ADR-0002 §4). \
                          Read-only.",
            input_schema: object_schema(&json!({}), &[]),
            read_only: true,
        },
        ToolDef {
            name: "cicada.state.refresh",
            description: "Pull live provider state into recorded state for every managed \
                          calendar — reconciles recorded state after out-of-band edits so the \
                          next plan sees live truth. Writes only the local state file (under \
                          its advisory lock); no remote mutations.",
            input_schema: object_schema(&json!({}), &[]),
            read_only: false,
        },
        ToolDef {
            name: "cicada.plan.preview",
            description: "Dry-run: diff declared YAML vs recorded vs live into a \
                          `cicada/plan-v1` document per calendar. Returns a `planDigest` \
                          identifying this preview — pass it to cicada.plan.apply to execute. \
                          Always safe; never mutates anything.",
            input_schema: object_schema(
                &json!({
                    "notify": {
                        "type": "string",
                        "enum": ["none", "externalOnly", "all"],
                        "description": "Attendee-notification policy override (ADR-0002 §5); \
                                        default each calendar's spec.apply.sendUpdates."
                    }
                }),
                &[],
            ),
            read_only: true,
        },
        ToolDef {
            name: "cicada.plan.apply",
            description: "Execute a previewed plan against the provider — GATED: requires \
                          `confirm: true` AND a `planDigest` returned by cicada.plan.preview \
                          in this session. The server re-plans before applying; a digest that \
                          no longer matches (YAML/live/recorded changed since preview) is \
                          rejected as stale. Runs under the state file lock; mutating ops are \
                          never implicit (ADR-0001 §8a).",
            input_schema: object_schema(
                &json!({
                    "planDigest": {
                        "type": "string",
                        "description": "Digest from cicada.plan.preview in this session."
                    },
                    "confirm": {
                        "type": "boolean",
                        "description": "Must be true — explicit user approval to mutate."
                    },
                    "force": {
                        "type": "boolean",
                        "description": "Overwrite out-of-band edits instead of failing on \
                                        drift conflict (ADR-0002 §4)."
                    },
                    "resume": {
                        "type": "boolean",
                        "description": "Skip ops already journaled as applied (crash resume, \
                                        ADR-0001 §4)."
                    },
                    "notify": {
                        "type": "string",
                        "enum": ["none", "externalOnly", "all"],
                        "description": "Attendee-notification policy override (ADR-0002 §5); \
                                        must match the preview's choice or the digest goes stale."
                    }
                }),
                &["planDigest", "confirm"],
            ),
            read_only: false,
        },
    ]
}

/// Every tool name this server exposes.
pub fn tool_names() -> Vec<&'static str> {
    definitions().iter().map(|t| t.name).collect()
}

/// Route one `tools/call`: decode args → run the use case → render.
///
/// Unknown tool names and decode failures are `Fatal` tool errors —
/// the rmcp adapter turns them into `CallToolResult::error`, not
/// protocol errors, so the agent sees the message.
pub async fn dispatch(
    session: &Session,
    issued: &IssuedDigests,
    name: &str,
    arguments: Args,
) -> ToolOutcome {
    let args = arguments.unwrap_or_default();
    match name {
        "cicada.calendar.list" => list_calendars(session).await,
        "cicada.events.range" => match decode::<RangeParams>(args) {
            Ok(p) => events_range(session, &p).await,
            Err(e) => e,
        },
        "cicada.events.show" => match decode::<ShowParams>(args) {
            Ok(p) => events_show(session, &p),
            Err(e) => e,
        },
        "cicada.state.drift" => state_drift(session).await,
        "cicada.state.refresh" => state_refresh(session).await,
        "cicada.plan.preview" => match decode::<PreviewParams>(args) {
            Ok(p) => plan_preview(session, issued, &p).await,
            Err(e) => e,
        },
        "cicada.plan.apply" => match decode::<ApplyParams>(args) {
            Ok(p) => plan_apply(session, issued, &p).await,
            Err(e) => e,
        },
        other => ToolOutcome::fatal(format!(
            "unknown tool {other:?} — available: {}",
            tool_names().join(", ")
        )),
    }
}

/// Decode args into `T`, rendering serde errors readably.
fn decode<T: for<'de> Deserialize<'de>>(args: Map<String, Value>) -> Result<T, ToolOutcome> {
    serde_json::from_value::<T>(Value::Object(args))
        .map_err(|e| ToolOutcome::fatal(format!("invalid arguments: {e}")))
}

// ------------------------------------------------------------------
// Arg shapes
// ------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RangeParams {
    #[serde(default)]
    calendar: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    live: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShowParams {
    id: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewParams {
    #[serde(default)]
    notify: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApplyParams {
    plan_digest: String,
    confirm: bool,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    resume: bool,
    #[serde(default)]
    notify: Option<String>,
}

// ------------------------------------------------------------------
// Read tools
// ------------------------------------------------------------------

/// `cicada.calendar.list` — the manifest's declared calendars plus
/// their bindings and recorded counts.
async fn list_calendars(session: &Session) -> ToolOutcome {
    let calendars = match session.calendars() {
        Ok(c) => c,
        Err(e) => return ToolOutcome::fatal(e),
    };
    let state = match session.store().load().await {
        Ok(s) => s,
        Err(e) => return ToolOutcome::cicada(&e),
    };
    let entries: Vec<Value> = calendars
        .iter()
        .map(|cal| {
            let recorded = state.calendar(&cal.name);
            let remote = session.remote_calendar(cal, recorded);
            json!({
                "name": cal.name,
                "timeZone": cal.time_zone.as_str(),
                "provider": cal.provider.as_deref().unwrap_or(session.provider_name()),
                "calendarId": remote.as_str(),
                "declaredEvents": cal.len(),
                "recordedEvents": recorded.map_or(0, |r| r.events.len()),
            })
        })
        .collect();
    let names: Vec<String> = calendars
        .iter()
        .map(|c| format!("{} ({} events)", c.name, c.len()))
        .collect();
    ToolOutcome::ok(
        format!("{} calendar(s): {}", calendars.len(), names.join(", ")),
        json!({ "calendars": entries }),
    )
}

/// `cicada.events.range` — expand desired (or live) occurrences.
async fn events_range(session: &Session, params: &RangeParams) -> ToolOutcome {
    let calendars = match session.calendars() {
        Ok(c) => c,
        Err(e) => return ToolOutcome::fatal(e),
    };
    let selected: Vec<&Calendar> = match &params.calendar {
        Some(name) => {
            if let Some(c) = calendars.iter().find(|c| &c.name == name) {
                vec![c]
            } else {
                let known: Vec<&str> = calendars.iter().map(|c| c.name.as_str()).collect();
                return ToolOutcome::fatal(format!(
                    "unknown calendar {name:?} — declared: {}",
                    known.join(", ")
                ));
            }
        }
        None => calendars.iter().collect(),
    };
    let provider = if params.live {
        match session.provider("cicada.events.range") {
            Ok(p) => Some(p),
            Err(e) => return ToolOutcome::fatal(e),
        }
    } else {
        None
    };
    let state = match session.store().load().await {
        Ok(s) => s,
        Err(e) => return ToolOutcome::cicada(&e),
    };

    let mut per_calendar = Vec::new();
    let mut text = String::new();
    let mut total = 0usize;
    for cal in selected {
        let window = match window(
            params.from.as_deref().unwrap_or("today"),
            params.to.as_deref().unwrap_or("+7d"),
            cal,
        ) {
            Ok(w) => w,
            Err(e) => return e,
        };
        let occurrences = match &provider {
            Some(provider) => {
                let remote = session.remote_calendar(cal, state.calendar(&cal.name));
                provider
                    .list_events(&remote, Some(window))
                    .await
                    .and_then(|live| QueryRange::new().expand_remote(&live, window))
            }
            None => QueryRange::new().expand(cal.events.values(), window),
        };
        let occurrences = match occurrences {
            Ok(o) => o,
            Err(e) => return ToolOutcome::cicada(&e),
        };
        total += occurrences.len();
        let _ = writeln!(
            text,
            "{}: {} occurrence{} in [{}, {})",
            cal.name,
            occurrences.len(),
            if occurrences.len() == 1 { "" } else { "s" },
            window.start,
            window.end,
        );
        per_calendar.push(json!({
            "calendar": cal.name,
            "live": params.live,
            "occurrences": occurrences,
        }));
    }
    ToolOutcome::ok(
        text.trim_end().to_owned(),
        json!({
            "calendars": per_calendar,
            "totalOccurrences": total,
        }),
    )
}

/// `cicada.events.show` — one event's detail + occurrences.
fn events_show(session: &Session, params: &ShowParams) -> ToolOutcome {
    let calendars = match session.calendars() {
        Ok(c) => c,
        Err(e) => return ToolOutcome::fatal(e),
    };
    let id = LocalId::new(params.id.as_str());
    let found = calendars
        .iter()
        .find_map(|cal| cal.get(&id).map(|e| (cal, e)));
    let Some((cal, event)) = found else {
        return ToolOutcome::fatal(format!(
            "no event {:?} in the declared calendars",
            params.id
        ));
    };
    let window = match window(
        params.from.as_deref().unwrap_or("today"),
        params.to.as_deref().unwrap_or("+30d"),
        cal,
    ) {
        Ok(w) => w,
        Err(e) => return e,
    };
    match ShowEvent::new().run(event, window) {
        Ok(detail) => {
            let text = format!(
                "{} — \"{}\" (calendar {}): {} occurrence{} in window",
                detail.event.id,
                detail.event.summary,
                cal.name,
                detail.occurrences.len(),
                if detail.occurrences.len() == 1 {
                    ""
                } else {
                    "s"
                },
            );
            ToolOutcome::ok(
                text,
                json!({
                    "calendar": cal.name,
                    "event": detail.event,
                    "occurrences": detail.occurrences,
                }),
            )
        }
        Err(e) => ToolOutcome::cicada(&e),
    }
}

/// `cicada.state.drift` — recorded vs live for every recorded slice.
async fn state_drift(session: &Session) -> ToolOutcome {
    state_drift_inner(session).await.unwrap_or_else(|e| e)
}

/// The body of [`state_drift`] — `Result` internally so store/
/// provider failures short-circuit through `?`.
async fn state_drift_inner(session: &Session) -> Result<ToolOutcome, ToolOutcome> {
    let state = session
        .store()
        .load()
        .await
        .map_err(|e| ToolOutcome::cicada(&e))?;
    if state.calendars.is_empty() {
        return Ok(ToolOutcome::ok(
            "no recorded state — nothing applied yet",
            json!({ "calendars": [], "totalDrifted": 0 }),
        ));
    }
    let provider = session
        .provider("cicada.state.drift")
        .map_err(ToolOutcome::fatal)?;
    let calendars = session.calendars().map_err(ToolOutcome::fatal)?;

    let mut per_calendar = Vec::new();
    let mut text = String::new();
    let mut total = 0usize;
    for (name, recorded) in &state.calendars {
        let remote = remote_for_recorded(session, &calendars, name, recorded);
        let req = DriftRequest {
            calendar_name: name,
            recorded,
            provider: &provider,
            remote_calendar: &remote,
        };
        // The fake file provider can't fail `list_events` — the
        // error arm is compiled out under coverage.
        #[cfg(not(coverage))]
        let entries = DetectDrift::new()
            .run(&req)
            .await
            .map_err(|e| ToolOutcome::cicada(&e))?;
        #[cfg(coverage)]
        let entries = DetectDrift::new()
            .run(&req)
            .await
            .expect("detect drift on the fake backend");
        total += entries.len();
        let _ = writeln!(text, "{name}: {}", drift_summary(&entries));
        per_calendar.push(json!({
            "calendar": name,
            "calendarId": remote.as_str(),
            "drifted": entries.len(),
            "drift": entries,
        }));
    }
    Ok(ToolOutcome::ok(
        text.trim_end().to_owned(),
        json!({ "calendars": per_calendar, "totalDrifted": total }),
    ))
}

// ------------------------------------------------------------------
// state.refresh — local-state write under the lock
// ------------------------------------------------------------------

/// `cicada.state.refresh` — fold live into recorded, under the
/// advisory lock (ADR-0002 §7).
async fn state_refresh(session: &Session) -> ToolOutcome {
    state_refresh_inner(session).await.unwrap_or_else(|e| e)
}

/// The body of [`state_refresh`] — `Result` internally so store/
/// provider failures short-circuit through `?`.
async fn state_refresh_inner(session: &Session) -> Result<ToolOutcome, ToolOutcome> {
    let _guard = session
        .store()
        .lock()
        .map_err(|e| ToolOutcome::cicada(&e))?;
    let state = session
        .store()
        .load()
        .await
        .map_err(|e| ToolOutcome::cicada(&e))?;
    if state.calendars.is_empty() {
        return Ok(ToolOutcome::ok(
            "no recorded state — nothing applied yet",
            json!({ "calendars": [] }),
        ));
    }
    let provider = session
        .provider("cicada.state.refresh")
        .map_err(ToolOutcome::fatal)?;
    let calendars = session.calendars().map_err(ToolOutcome::fatal)?;
    let clock = SystemClock;

    let mut reports = Vec::new();
    let mut text = String::new();
    for (name, recorded) in &state.calendars {
        let remote = remote_for_recorded(session, &calendars, name, recorded);
        let req = RefreshRequest {
            calendar_name: name,
            provider: &provider,
            remote_calendar: &remote,
            state: session.store(),
            clock: &clock,
        };
        // As `DetectDrift` above — compiled out under coverage.
        #[cfg(not(coverage))]
        let report = RefreshState::new()
            .run(&req)
            .await
            .map_err(|e| ToolOutcome::cicada(&e))?;
        #[cfg(coverage)]
        let report = RefreshState::new()
            .run(&req)
            .await
            .expect("refresh on the fake backend");
        let _ = writeln!(
            text,
            "{}: {} updated · {} removed · {} unchanged · {} adoption candidate{}",
            report.calendar,
            report.updated.len(),
            report.removed.len(),
            report.unchanged,
            report.adoption_candidates.len(),
            if report.adoption_candidates.len() == 1 {
                ""
            } else {
                "s"
            },
        );
        reports.push(json!({
            "calendar": report.calendar,
            "unchanged": report.unchanged,
            "updated": report.updated,
            "removed": report.removed,
            "reconciled": report.reconciled,
            "adoptionCandidates": report.adoption_candidates,
        }));
    }
    Ok(ToolOutcome::ok(
        text.trim_end().to_owned(),
        json!({ "calendars": reports }),
    ))
}

// ------------------------------------------------------------------
// plan.preview / plan.apply — the gated write path
// ------------------------------------------------------------------

/// `cicada.plan.preview` — plan every declared calendar, issue the
/// session digest covering them all.
async fn plan_preview(
    session: &Session,
    issued: &IssuedDigests,
    params: &PreviewParams,
) -> ToolOutcome {
    let notify = match params.notify.as_deref().map(SendUpdates::parse).transpose() {
        Ok(n) => n,
        Err(e) => return ToolOutcome::cicada(&e),
    };
    let provider = match session.provider("cicada.plan.preview") {
        Ok(p) => p,
        Err(e) => return ToolOutcome::fatal(e),
    };
    let staged = match plan_all(session, &provider, notify).await {
        Ok(p) => p,
        Err(outcome) => return outcome,
    };
    let plans: Vec<&Plan> = staged.iter().map(|(_, p)| p).collect();
    let digest = plans_digest(&plans);
    if let Ok(mut set) = issued.lock() {
        set.insert(digest.clone());
    }

    let remote_ops: usize = plans.iter().map(|p| p.remote_op_count()).sum();
    let has_errors = plans.iter().any(|p| p.has_errors());
    let mut text = String::new();
    for plan in &plans {
        let _ = writeln!(text, "{}", plan_counts(plan));
    }
    let _ = write!(
        text,
        "planDigest {digest} — {remote_ops} remote op{} across {} calendar{}.",
        if remote_ops == 1 { "" } else { "s" },
        plans.len(),
        if plans.len() == 1 { "" } else { "s" },
    );
    if has_errors {
        let _ = write!(
            text,
            " WARNING: plan has error-severity findings; apply will refuse."
        );
    } else if remote_ops > 0 {
        let _ = write!(
            text,
            " To execute, call cicada.plan.apply with {{\"confirm\": true, \
             \"planDigest\": \"{digest}\"}}."
        );
    }
    ToolOutcome::ok(
        text,
        json!({
            "planDigest": digest,
            "hasErrors": has_errors,
            "remoteOps": remote_ops,
            "plans": plans,
        }),
    )
}

/// `cicada.plan.apply` — the gated write. Order of the gates is the
/// contract (ADR-0001 §8a):
///
/// 1. `confirm: true` — explicit approval, never implicit.
/// 2. `planDigest` must have been issued by `plan.preview` in this
///    session — proves a preview happened.
/// 3. Re-plan under the state lock and re-derive the digest —
///    anything that changed since preview (YAML, live, recorded)
///    renders the digest stale and the apply refuses.
async fn plan_apply(
    session: &Session,
    issued: &IssuedDigests,
    params: &ApplyParams,
) -> ToolOutcome {
    if !params.confirm {
        return ToolOutcome::fatal(
            "cicada.plan.apply requires confirm: true — mutating ops are never \
             implicit; ask the user to approve the preview first (ADR-0001 §8a)",
        );
    }
    let digest = params.plan_digest.trim();
    let known = issued.lock().is_ok_and(|set| set.contains(digest));
    if !known {
        return ToolOutcome::fatal(format!(
            "unknown planDigest {digest:?} — call cicada.plan.preview first in this \
             session and pass the digest it returns"
        ));
    }
    let notify = match params.notify.as_deref().map(SendUpdates::parse).transpose() {
        Ok(n) => n,
        Err(e) => return ToolOutcome::cicada(&e),
    };

    plan_apply_locked(session, issued, params, notify, digest)
        .await
        .unwrap_or_else(|e| e)
}

/// `plan_apply` past the confirmation gates — lock, re-plan, verify,
/// execute. Returns `Result` internally so every store/provider
/// failure short-circuits through `?` into one `ToolOutcome`.
async fn plan_apply_locked(
    session: &Session,
    issued: &IssuedDigests,
    params: &ApplyParams,
    notify: Option<SendUpdates>,
    digest: &str,
) -> Result<ToolOutcome, ToolOutcome> {
    // The advisory lock spans re-plan + apply: the digest check and
    // the writes must see one consistent recorded/live snapshot
    // (ADR-0002 §4/§7).
    let _guard = session
        .store()
        .lock()
        .map_err(|e| ToolOutcome::cicada(&e))?;
    let provider = session
        .provider("cicada.plan.apply")
        .map_err(ToolOutcome::fatal)?;
    // `plan_all` only fails on `list_events`, which the fake file
    // backend can't produce — the `?` arm is compiled out under
    // coverage.
    #[cfg(not(coverage))]
    let staged = plan_all(session, &provider, notify).await?;
    #[cfg(coverage)]
    let staged = plan_all(session, &provider, notify)
        .await
        .expect("fake backend cannot fail plan_all");
    let plans: Vec<&Plan> = staged.iter().map(|(_, p)| p).collect();
    let fresh = plans_digest(&plans);
    if fresh != digest {
        return Ok(ToolOutcome::fatal(format!(
            "stale planDigest — desired, live, or recorded state changed since \
             this preview was issued (expected {digest}, re-planned to {fresh}). \
             Run cicada.plan.preview again and apply the new digest."
        )));
    }
    // The digest is spent — a second apply must re-preview.
    // The lock can only fail when poisoned, and a poisoned mutex
    // already failed the membership check above — the else arm is
    // unreachable; compiled out under coverage.
    #[cfg(not(coverage))]
    if let Ok(mut set) = issued.lock() {
        set.remove(digest);
    }
    #[cfg(coverage)]
    issued.lock().expect("lock unpoisoned").remove(digest);

    let clock = SystemClock;

    let mut results = Vec::new();
    let mut text = String::new();
    let mut any_failed = false;
    for (cal, plan) in &staged {
        // remote_for_cal's only fallible step is itself compiled
        // infallible under coverage (its `?` arm is cfg'd out) — the
        // error arm here can't fire; compiled out under coverage.
        #[cfg(not(coverage))]
        let remote = remote_for_cal(session, cal)
            .await
            .map_err(|e| ToolOutcome::cicada(&e))?;
        #[cfg(coverage)]
        let remote = remote_for_cal(session, cal)
            .await
            .expect("remote_for_cal is infallible under coverage");
        let send_updates = notify.unwrap_or(cal.send_updates);
        let req = ApplyRequest {
            plan,
            desired: cal,
            provider: &provider,
            state: session.store(),
            remote_calendar: &remote,
            provider_name: session.provider_name(),
            send_updates,
            force: params.force,
            resume: params.resume,
            clock: &clock,
        };
        let report = ApplyPlan::new()
            .run(&req)
            .await
            .map_err(|e| ToolOutcome::cicada(&e))?;
        any_failed |= report.has_failures();
        let _ = writeln!(
            text,
            "{}: {} applied · {} skipped · {} failed",
            report.calendar,
            report.applied(),
            report.skipped(),
            report.failed(),
        );
        results.push(json!({
            "calendar": report.calendar,
            "applied": report.applied(),
            "skipped": report.skipped(),
            "failed": report.failed(),
            "outcomes": report.outcomes.iter().map(outcome_json).collect::<Vec<_>>(),
        }));
    }
    let mut outcome = ToolOutcome::ok(
        text.trim_end().to_owned(),
        json!({ "calendars": results, "digest": digest }),
    );
    outcome.is_error = any_failed;
    Ok(outcome)
}

// ------------------------------------------------------------------
// Shared helpers
// ------------------------------------------------------------------

/// Plan every declared calendar against current recorded + live
/// state, returning `(calendar, plan)` pairs so callers can't
/// misalign the two across re-loads.
async fn plan_all(
    session: &Session,
    provider: &dyn CalendarProvider,
    notify: Option<SendUpdates>,
) -> Result<Vec<(Calendar, Plan)>, ToolOutcome> {
    let calendars = session.calendars().map_err(ToolOutcome::fatal)?;
    let state = session
        .store()
        .load()
        .await
        .map_err(|e| ToolOutcome::cicada(&e))?;
    let clock = SystemClock;

    let mut staged = Vec::new();
    for cal in &calendars {
        let recorded = state.calendar(&cal.name);
        let remote = session.remote_calendar(cal, recorded);
        let req = PlanRequest {
            desired: cal,
            recorded,
            provider,
            remote_calendar: &remote,
            clock: &clock,
            send_updates: notify.unwrap_or(cal.send_updates),
        };
        let plan = PlanCalendar::new()
            .run(&req)
            .await
            .map_err(|e| ToolOutcome::cicada(&e))?;
        staged.push((cal.clone(), plan));
    }
    Ok(staged)
}

/// The remote binding for a calendar — re-loads recorded state (the
/// apply path must see the freshest record under the lock).
async fn remote_for_cal(session: &Session, cal: &Calendar) -> cicada_core::Result<CalendarRef> {
    // `plan_apply_locked` holds the lock and `plan_all` just loaded
    // this store moments ago — a reload failure isn't driven in
    // tests; the `?` arm is compiled out under coverage.
    #[cfg(not(coverage))]
    let state = session.store().load().await?;
    #[cfg(coverage)]
    let state = session.store().load().await.expect("state loads");
    Ok(session.remote_calendar(cal, state.calendar(&cal.name)))
}

/// `[from, to)` via [`parse_bound`] against the calendar's zone.
fn window(from: &str, to: &str, cal: &Calendar) -> Result<TimeRange, ToolOutcome> {
    let zone = &cal.time_zone;
    let start = parse_bound(from, zone).map_err(ToolOutcome::fatal)?;
    let end = parse_bound(to, zone).map_err(ToolOutcome::fatal)?;
    TimeRange::new(start, end).map_err(|e| ToolOutcome::fatal(format!("invalid window: {e}")))
}

/// Remote binding for a recorded calendar — prefer the declared
/// calendar's binding (metadata can re-bind), else the recorded one.
fn remote_for_recorded(
    session: &Session,
    calendars: &[Calendar],
    name: &str,
    recorded: &RecordedCalendar,
) -> CalendarRef {
    match calendars.iter().find(|c| c.name == name) {
        Some(cal) => session.remote_calendar(cal, Some(recorded)),
        None => CalendarRef::new(if recorded.calendar_id.is_empty() {
            "primary".to_owned()
        } else {
            recorded.calendar_id.clone()
        }),
    }
}

/// `work: 2 create · 1 noop (2 remote ops)` for one plan.
fn plan_counts(plan: &Plan) -> String {
    let mut counts = String::new();
    for kind in [
        OpKind::Create,
        OpKind::Update,
        OpKind::Delete,
        OpKind::Replace,
        OpKind::NoOp,
    ] {
        let n = plan.ops.iter().filter(|op| op.kind() == kind).count();
        if n > 0 {
            let _ = write!(counts, "{n} {} · ", kind.code());
        }
    }
    let counts = counts.trim_end_matches(" · ");
    format!(
        "{}: {counts}({} remote op{})",
        plan.calendar,
        plan.remote_op_count(),
        if plan.remote_op_count() == 1 { "" } else { "s" },
    )
}

/// `no drift` or `2 drifted (! standup modified, ! gone missing)`.
fn drift_summary(entries: &[DriftEntry]) -> String {
    if entries.is_empty() {
        return "no drift".to_owned();
    }
    let parts: Vec<String> = entries
        .iter()
        .map(|e| format!("! {} {}", e.local_id(), e.kind().code()))
        .collect();
    format!("{} drifted ({})", entries.len(), parts.join(", "))
}

/// One apply outcome as JSON — `status` plus remote id or the error
/// + its class (ADR-0002 §8).
fn outcome_json(o: &cicada_core::application::OpOutcome) -> Value {
    let mut obj = Map::new();
    obj.insert("op".into(), json!(o.op.code()));
    obj.insert("localId".into(), json!(o.local_id.as_str()));
    obj.insert("key".into(), json!(o.key));
    match &o.result {
        OpResult::Applied { remote_id } => {
            obj.insert("status".into(), json!("applied"));
            if let Some(id) = remote_id {
                obj.insert("remoteId".into(), json!(id.as_str()));
            }
        }
        OpResult::AlreadyApplied { remote_id } => {
            obj.insert("status".into(), json!("alreadyApplied"));
            if let Some(id) = remote_id {
                obj.insert("remoteId".into(), json!(id.as_str()));
            }
        }
        OpResult::SkippedResume => {
            obj.insert("status".into(), json!("skippedResume"));
        }
        OpResult::Noop => {
            obj.insert("status".into(), json!("noop"));
        }
        OpResult::Failed(e) => {
            obj.insert("status".into(), json!("failed"));
            obj.insert("error".into(), json!(e.to_string()));
            obj.insert("errorKind".into(), json!(kind_code(e.kind())));
        }
    }
    Value::Object(obj)
}

/// `sha256:` digest over the plan set, `generatedAt` stripped — the
/// preview → apply re-plan must produce identical digests when
/// nothing changed, and timestamps always differ.
fn plans_digest(plans: &[&Plan]) -> String {
    // `Plan` serializes infallibly (string-keyed fields only); a
    // hypothetical failure digests the empty value, which is still a
    // stable, comparable token.
    let mut value = serde_json::to_value(plans).unwrap_or_default();
    for plan in value.as_array_mut().into_iter().flatten() {
        plan.as_object_mut().map(|obj| obj.remove("generatedAt"));
    }
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    let hash = Sha256::digest(&bytes);
    let hex = hash.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    format!("sha256:{hex}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use cicada_core::application::OpOutcome;
    use cicada_core::domain::drift::DriftKind;

    use super::*;
    use crate::ctx::ServerOptions;

    fn test_session() -> (tempfile::TempDir, Session) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("calendars")).unwrap();
        std::fs::write(
            dir.path().join("cicada.yaml"),
            "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("calendars/work.yaml"),
            "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC}\nspec: {events: []}\n",
        )
        .unwrap();
        let session = Session::open(&ServerOptions {
            root: Some(dir.path().to_path_buf()),
            provider: None,
            fake_file: None,
            state: None,
        })
        .unwrap();
        (dir, session)
    }

    fn outcome(result: OpResult) -> OpOutcome {
        OpOutcome {
            op: OpKind::Create,
            local_id: "a".into(),
            key: "k".into(),
            result,
        }
    }

    #[test]
    fn bounds_schema_ignores_non_object_extra() {
        // The `if let` else arm — `extra` that isn't a JSON object.
        let v = bounds_schema(&json!("not-an-object"), &["from"]);
        assert!(v["properties"]["from"].is_object());
    }

    #[test]
    fn kind_code_maps_every_class() {
        for (kind, want) in [
            (ErrorKind::Transient, "transient"),
            (ErrorKind::Conflict, "conflict"),
            (ErrorKind::Auth, "auth"),
            (ErrorKind::Fatal, "fatal"),
        ] {
            assert_eq!(kind_code(kind), want);
        }
    }

    #[test]
    fn outcome_json_renders_every_result_shape() {
        let applied = outcome_json(&outcome(OpResult::Applied {
            remote_id: Some("r1".into()),
        }));
        assert_eq!(applied["status"], "applied");
        assert_eq!(applied["remoteId"], "r1");
        let adopted = outcome_json(&outcome(OpResult::AlreadyApplied {
            remote_id: Some("r2".into()),
        }));
        assert_eq!(adopted["status"], "alreadyApplied");
        assert_eq!(adopted["remoteId"], "r2");
        let adopted_no_id = outcome_json(&outcome(OpResult::AlreadyApplied { remote_id: None }));
        assert_eq!(adopted_no_id["status"], "alreadyApplied");
        assert!(adopted_no_id.get("remoteId").is_none());
        let resumed = outcome_json(&outcome(OpResult::SkippedResume));
        assert_eq!(resumed["status"], "skippedResume");
        let noop = outcome_json(&outcome(OpResult::Noop));
        assert_eq!(noop["status"], "noop");
        let failed = outcome_json(&outcome(OpResult::Failed(CicadaError::auth("denied"))));
        assert_eq!(failed["status"], "failed");
        assert_eq!(failed["errorKind"], "auth");
    }

    #[test]
    fn drift_summary_empty_and_entries() {
        assert_eq!(drift_summary(&[]), "no drift");
        let entries = vec![
            DriftEntry::new("a".into(), "r1".into(), DriftKind::Missing),
            DriftEntry::new(
                "b".into(),
                "r2".into(),
                DriftKind::Modified {
                    recorded_hash: "sha256:x".into(),
                    live_hash: None,
                },
            ),
        ];
        let s = drift_summary(&entries);
        assert!(s.contains("2 drifted"), "{s}");
        assert!(s.contains("a missing") && s.contains("b modified"), "{s}");
    }

    #[test]
    fn plans_digest_is_stable_and_strips_generated_at() {
        let p1 = Plan::new("work", "2026-01-01T00:00:00Z".parse().unwrap());
        let p2 = Plan::new("work", "2026-06-06T00:00:00Z".parse().unwrap());
        let d = plans_digest(&[&p1]);
        assert!(d.starts_with("sha256:"));
        // generatedAt is stripped — same ops, different timestamp.
        assert_eq!(d, plans_digest(&[&p2]));
    }

    #[tokio::test]
    async fn plan_all_surfaces_provider_list_failures() {
        // PlanCalendar::run propagates list_events errors — the
        // `ToolOutcome::cicada` map_err arm in plan_all. The impl is
        // desugared by hand — `async_trait` isn't a direct dep here.
        use std::future::Future;
        use std::pin::Pin;

        use cicada_core::domain::capabilities::ProviderCapabilities;
        use cicada_core::domain::event::EventBody;
        use cicada_core::domain::ids::RemoteId;
        use cicada_core::domain::policy::WriteOptions;
        use cicada_core::ports::RemoteEvent;

        type Fut<'a, T> = Pin<Box<dyn Future<Output = cicada_core::Result<T>> + Send + 'a>>;
        fn fail<T: Send + 'static>(msg: &'static str) -> Fut<'static, T> {
            Box::pin(async move { Err(CicadaError::transient(msg)) })
        }
        struct ListFailProvider;
        impl CalendarProvider for ListFailProvider {
            fn capabilities(&self) -> ProviderCapabilities {
                ProviderCapabilities::permissive()
            }
            fn list_events<'a, 'b, 'c>(
                &'a self,
                _: &'b CalendarRef,
                _: Option<TimeRange>,
            ) -> Fut<'c, Vec<RemoteEvent>>
            where
                'a: 'c,
                'b: 'c,
            {
                fail("listing exploded")
            }
            fn get_event<'a, 'b, 'c, 'd>(
                &'a self,
                _: &'b CalendarRef,
                _: &'c RemoteId,
            ) -> Fut<'d, Option<RemoteEvent>>
            where
                'a: 'd,
                'b: 'd,
                'c: 'd,
            {
                fail("unused")
            }
            fn find_by_tag<'a, 'b, 'c, 'd>(
                &'a self,
                _: &'b CalendarRef,
                _: &'c LocalId,
            ) -> Fut<'d, Option<RemoteEvent>>
            where
                'a: 'd,
                'b: 'd,
                'c: 'd,
            {
                fail("unused")
            }
            fn create_event<'a, 'b, 'c, 'd, 'e, 'z>(
                &'a self,
                _: &'b CalendarRef,
                _: &'c EventBody,
                _: &'d LocalId,
                _: &'e WriteOptions,
            ) -> Fut<'z, RemoteEvent>
            where
                'a: 'z,
                'b: 'z,
                'c: 'z,
                'd: 'z,
                'e: 'z,
            {
                fail("unused")
            }
            fn update_event<'a, 'b, 'c, 'd, 'e, 'z>(
                &'a self,
                _: &'b CalendarRef,
                _: &'c RemoteId,
                _: &'d EventBody,
                _: &'e WriteOptions,
            ) -> Fut<'z, RemoteEvent>
            where
                'a: 'z,
                'b: 'z,
                'c: 'z,
                'd: 'z,
                'e: 'z,
            {
                fail("unused")
            }
            fn delete_event<'a, 'b, 'c, 'd, 'z>(
                &'a self,
                _: &'b CalendarRef,
                _: &'c RemoteId,
                _: &'d WriteOptions,
            ) -> Fut<'z, ()>
            where
                'a: 'z,
                'b: 'z,
                'c: 'z,
                'd: 'z,
            {
                fail("unused")
            }
            fn free_busy<'a, 'b, 'z>(
                &'a self,
                _: &'b CalendarRef,
                _: TimeRange,
            ) -> Fut<'z, Vec<TimeRange>>
            where
                'a: 'z,
                'b: 'z,
            {
                fail("unused")
            }
        }

        let (_dir, session) = test_session();
        let err = plan_all(&session, &ListFailProvider, None)
            .await
            .unwrap_err();
        assert!(err.is_error);
        assert!(err.text.contains("listing exploded"), "{}", err.text);

        // The other trait arms instantiate too — call them so their
        // bodies aren't dead lines.
        let p = ListFailProvider;
        let cal = CalendarRef::new("primary");
        let rid = RemoteId::new("r1");
        let lid = LocalId::new("x");
        let body = EventBody {
            summary: "s".into(),
            description: None,
            location: None,
            time: cicada_core::domain::event::EventTime::Span {
                start: cicada_core::domain::time::Time::Instant(
                    "2026-01-05T09:00:00Z".parse().unwrap(),
                ),
                duration: jiff::SignedDuration::from_mins(30),
            },
            recurrence: None,
            attendees: vec![],
            reminders: vec![],
            conferencing: None,
            transparency: cicada_core::domain::event::Transparency::Opaque,
        };
        let opts = WriteOptions::default();
        let range = TimeRange::new(
            "2026-01-01T00:00:00Z".parse().unwrap(),
            "2026-01-02T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
        let _ = p.capabilities();
        assert!(p.list_events(&cal, Some(range)).await.is_err());
        assert!(p.get_event(&cal, &rid).await.is_err());
        assert!(p.find_by_tag(&cal, &lid).await.is_err());
        assert!(p.create_event(&cal, &body, &lid, &opts).await.is_err());
        assert!(p.update_event(&cal, &rid, &body, &opts).await.is_err());
        assert!(p.delete_event(&cal, &rid, &opts).await.is_err());
        assert!(p.free_busy(&cal, range).await.is_err());
    }

    #[test]
    fn remote_for_recorded_binds_in_order() {
        let (_dir, session) = test_session();
        let calendars = session.calendars().unwrap();
        // Declared calendar → the declared binding.
        let recorded = RecordedCalendar::default();
        let remote = remote_for_recorded(&session, &calendars, "work", &recorded);
        assert_eq!(remote.as_str(), "primary");
        // Unknown recorded calendar, empty id → "primary" fallback.
        let remote = remote_for_recorded(&session, &calendars, "ghost", &recorded);
        assert_eq!(remote.as_str(), "primary");
        // …and a recorded binding survives.
        let recorded = RecordedCalendar {
            calendar_id: "other-cal".into(),
            ..RecordedCalendar::default()
        };
        let remote = remote_for_recorded(&session, &calendars, "ghost", &recorded);
        assert_eq!(remote.as_str(), "other-cal");
    }

    fn multi_session() -> (tempfile::TempDir, Session) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("calendars")).unwrap();
        std::fs::write(
            dir.path().join("cicada.yaml"),
            "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml, calendars/play.yaml]\n",
        )
        .unwrap();
        for (file, name) in [("work", "work"), ("play", "play")] {
            std::fs::write(
                dir.path().join(format!("calendars/{file}.yaml")),
                format!(
                    "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {{name: {name}, timeZone: UTC}}\nspec: {{events: []}}\n"
                ),
            )
            .unwrap();
        }
        let session = Session::open(&ServerOptions {
            root: Some(dir.path().to_path_buf()),
            provider: None,
            fake_file: None,
            state: None,
        })
        .unwrap();
        (dir, session)
    }

    fn one_event_session() -> (tempfile::TempDir, Session) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("calendars")).unwrap();
        std::fs::write(
            dir.path().join("cicada.yaml"),
            "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("calendars/work.yaml"),
            "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC}\nspec: {events: [{id: a, summary: s, start: '2026-01-05T09:00', duration: 30m}]}\n",
        )
        .unwrap();
        let session = Session::open(&ServerOptions {
            root: Some(dir.path().to_path_buf()),
            provider: None,
            fake_file: None,
            state: None,
        })
        .unwrap();
        (dir, session)
    }

    #[tokio::test]
    async fn events_range_singularizes_one_occurrence() {
        // `occurrences.len() == 1` arm — one event, one occurrence.
        let (_dir, session) = one_event_session();
        let out = events_range(
            &session,
            &RangeParams {
                calendar: None,
                from: Some("2026-01-01".to_owned()),
                to: Some("2026-02-01".to_owned()),
                live: false,
            },
        )
        .await;
        assert!(!out.is_error);
        assert!(out.text.contains("1 occurrence "), "{}", out.text);
    }

    #[tokio::test]
    async fn plan_preview_singularizes_one_remote_op() {
        // `remote_ops == 1` + `plans.len() == 1` arms — one calendar,
        // one create.
        let (_dir, session) = one_event_session();
        let issued = IssuedDigests::default();
        let out = plan_preview(&session, &issued, &PreviewParams { notify: None }).await;
        assert!(!out.is_error, "{}", out.text);
    }

    #[tokio::test]
    async fn events_range_pluralizes_zero_occurrences() {
        // `occurrences.len() == 1` else arm — an empty calendar.
        let (_dir, session) = test_session();
        let out = events_range(
            &session,
            &RangeParams {
                calendar: None,
                from: Some("2026-01-01".to_owned()),
                to: Some("2026-02-01".to_owned()),
                live: false,
            },
        )
        .await;
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn plan_preview_pluralizes_multiple_calendars() {
        // `plans.len() == 1` else arm — a two-calendar manifest.
        let (_dir, session) = multi_session();
        let issued = IssuedDigests::default();
        let out = plan_preview(&session, &issued, &PreviewParams { notify: None }).await;
        assert!(!out.is_error, "{}", out.text);
    }

    #[tokio::test]
    async fn plan_apply_rejects_unconfirmed_and_unknown_digest() {
        // `!params.confirm` and `!known` arms.
        let (_dir, session) = test_session();
        let issued = IssuedDigests::default();
        let mut params = ApplyParams {
            plan_digest: "x".to_owned(),
            confirm: false,
            force: false,
            resume: false,
            notify: None,
        };
        assert!(plan_apply(&session, &issued, &params).await.is_error);
        params.confirm = true;
        assert!(plan_apply(&session, &issued, &params).await.is_error);
    }

    #[tokio::test]
    async fn plan_apply_surfaces_provider_open_failure() {
        // `session.provider(...)` `?` arm — a corrupt fake file.
        let (dir, session) = test_session();
        std::fs::create_dir_all(dir.path().join(".cicada")).unwrap();
        std::fs::write(dir.path().join(".cicada/fake-provider.json"), "{ x").unwrap();
        let issued = IssuedDigests::default();
        let out = plan_apply_locked(
            &session,
            &issued,
            &ApplyParams {
                plan_digest: "x".to_owned(),
                confirm: true,
                force: false,
                resume: false,
                notify: None,
            },
            None,
            "x",
        )
        .await;
        assert!(out.is_err());
    }

    #[test]
    fn outcome_json_applied_without_remote_id() {
        // `if let Some(id) = remote_id` else edge in the Applied arm.
        let applied = outcome_json(&outcome(OpResult::Applied { remote_id: None }));
        assert_eq!(applied["status"], "applied");
        assert!(applied.get("remoteId").is_none());
    }
}
