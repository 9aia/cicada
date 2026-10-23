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
                match provider.list_events(&remote, Some(window)).await {
                    Ok(live) => QueryRange::new().expand_remote(&live, window),
                    Err(e) => Err(e),
                }
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
    let state = match session.store().load().await {
        Ok(s) => s,
        Err(e) => return ToolOutcome::cicada(&e),
    };
    if state.calendars.is_empty() {
        return ToolOutcome::ok(
            "no recorded state — nothing applied yet",
            json!({ "calendars": [], "totalDrifted": 0 }),
        );
    }
    let provider = match session.provider("cicada.state.drift") {
        Ok(p) => p,
        Err(e) => return ToolOutcome::fatal(e),
    };
    let calendars = match session.calendars() {
        Ok(c) => c,
        Err(e) => return ToolOutcome::fatal(e),
    };

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
        match DetectDrift::new().run(&req).await {
            Ok(entries) => {
                total += entries.len();
                let _ = writeln!(text, "{name}: {}", drift_summary(&entries));
                per_calendar.push(json!({
                    "calendar": name,
                    "calendarId": remote.as_str(),
                    "drifted": entries.len(),
                    "drift": entries,
                }));
            }
            Err(e) => return ToolOutcome::cicada(&e),
        }
    }
    ToolOutcome::ok(
        text.trim_end().to_owned(),
        json!({ "calendars": per_calendar, "totalDrifted": total }),
    )
}

// ------------------------------------------------------------------
// state.refresh — local-state write under the lock
// ------------------------------------------------------------------

/// `cicada.state.refresh` — fold live into recorded, under the
/// advisory lock (ADR-0002 §7).
async fn state_refresh(session: &Session) -> ToolOutcome {
    let _guard = match session.store().lock() {
        Ok(g) => g,
        Err(e) => return ToolOutcome::cicada(&e),
    };
    let state = match session.store().load().await {
        Ok(s) => s,
        Err(e) => return ToolOutcome::cicada(&e),
    };
    if state.calendars.is_empty() {
        return ToolOutcome::ok(
            "no recorded state — nothing applied yet",
            json!({ "calendars": [] }),
        );
    }
    let provider = match session.provider("cicada.state.refresh") {
        Ok(p) => p,
        Err(e) => return ToolOutcome::fatal(e),
    };
    let calendars = match session.calendars() {
        Ok(c) => c,
        Err(e) => return ToolOutcome::fatal(e),
    };
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
        match RefreshState::new().run(&req).await {
            Ok(report) => {
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
            Err(e) => return ToolOutcome::cicada(&e),
        }
    }
    ToolOutcome::ok(text.trim_end().to_owned(), json!({ "calendars": reports }))
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
    let staged = match plan_all(session, notify).await {
        Ok(p) => p,
        Err(outcome) => return outcome,
    };
    let plans: Vec<&Plan> = staged.iter().map(|(_, p)| p).collect();
    let digest = match plans_digest(&plans) {
        Ok(d) => d,
        Err(e) => return ToolOutcome::fatal(e),
    };
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

    // The advisory lock spans re-plan + apply: the digest check and
    // the writes must see one consistent recorded/live snapshot
    // (ADR-0002 §4/§7).
    let _guard = match session.store().lock() {
        Ok(g) => g,
        Err(e) => return ToolOutcome::cicada(&e),
    };
    let staged = match plan_all(session, notify).await {
        Ok(p) => p,
        Err(outcome) => return outcome,
    };
    let plans: Vec<&Plan> = staged.iter().map(|(_, p)| p).collect();
    match plans_digest(&plans) {
        Ok(fresh) if fresh != digest => {
            return ToolOutcome::fatal(format!(
                "stale planDigest — desired, live, or recorded state changed since \
                 this preview was issued (expected {digest}, re-planned to {fresh}). \
                 Run cicada.plan.preview again and apply the new digest."
            ));
        }
        Ok(_) => {}
        Err(e) => return ToolOutcome::fatal(e),
    }
    // The digest is spent — a second apply must re-preview.
    if let Ok(mut set) = issued.lock() {
        set.remove(digest);
    }

    let provider = match session.provider("cicada.plan.apply") {
        Ok(p) => p,
        Err(e) => return ToolOutcome::fatal(e),
    };
    let clock = SystemClock;

    let mut results = Vec::new();
    let mut text = String::new();
    let mut any_failed = false;
    for (cal, plan) in &staged {
        let remote = remote_for_cal(session, cal).await;
        let remote = match remote {
            Ok(r) => r,
            Err(e) => return ToolOutcome::cicada(&e),
        };
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
        match ApplyPlan::new().run(&req).await {
            Ok(report) => {
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
            Err(e) => return ToolOutcome::cicada(&e),
        }
    }
    let mut outcome = ToolOutcome::ok(
        text.trim_end().to_owned(),
        json!({ "calendars": results, "digest": digest }),
    );
    outcome.is_error = any_failed;
    outcome
}

// ------------------------------------------------------------------
// Shared helpers
// ------------------------------------------------------------------

/// Plan every declared calendar against current recorded + live
/// state, returning `(calendar, plan)` pairs so callers can't
/// misalign the two across re-loads.
async fn plan_all(
    session: &Session,
    notify: Option<SendUpdates>,
) -> Result<Vec<(Calendar, Plan)>, ToolOutcome> {
    let calendars = session.calendars().map_err(ToolOutcome::fatal)?;
    let provider = session
        .provider("cicada.plan")
        .map_err(ToolOutcome::fatal)?;
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
            provider: &provider,
            remote_calendar: &remote,
            clock: &clock,
            send_updates: notify.unwrap_or(cal.send_updates),
        };
        match PlanCalendar::new().run(&req).await {
            Ok(plan) => staged.push((cal.clone(), plan)),
            Err(e) => return Err(ToolOutcome::cicada(&e)),
        }
    }
    Ok(staged)
}

/// The remote binding for a calendar — re-loads recorded state (the
/// apply path must see the freshest record under the lock).
async fn remote_for_cal(session: &Session, cal: &Calendar) -> cicada_core::Result<CalendarRef> {
    let state = session.store().load().await?;
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
    let mut v = json!({
        "op": o.op.code(),
        "localId": o.local_id.as_str(),
        "key": o.key,
    });
    if let Some(obj) = v.as_object_mut() {
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
    }
    v
}

/// `sha256:` digest over the plan set, `generatedAt` stripped — the
/// preview → apply re-plan must produce identical digests when
/// nothing changed, and timestamps always differ.
fn plans_digest(plans: &[&Plan]) -> Result<String, String> {
    let mut value =
        serde_json::to_value(plans).map_err(|e| format!("cannot serialize plans: {e}"))?;
    if let Some(plans) = value.as_array_mut() {
        for plan in plans {
            if let Some(obj) = plan.as_object_mut() {
                obj.remove("generatedAt");
            }
        }
    }
    let bytes = serde_json::to_vec(&value).map_err(|e| format!("cannot hash plans: {e}"))?;
    let hash = Sha256::digest(&bytes);
    let hex = hash.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    Ok(format!("sha256:{hex}"))
}
