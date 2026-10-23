# ADR-0001: Foundational Architecture for Calendar-as-Code

- **Status:** Proposed
- **Date:** 2026-08-19
- **Decision makers:** Project author
- **Supersedes:** none
- **Superseded by:** none

## Context

Cicada is a tool that treats calendars as declarative code: meetings, calendars,
and their recurrence, attendees, conferencing, and reminders are declared in YAML
and reconciled against live calendar providers (Google Calendar first, others
later) through a plan/apply/drift lifecycle borrowed from infrastructure-as-code.

The project must be:

- **Robust and resilient** — apply is idempotent, resumable, and honest about
  partial failure.
- **Modular** — a monorepo with a pure, reusable core.
- **Cacheable** — recorded state acts as a read-through cache; range queries are
  cached with TTL.
- **Professional, clean, elegant** — small surface, clear abstractions, no
  incidental complexity.
- **Composable** — YAML supports templates, groups, and references.
- **Extensible** — providers, state backends, auth methods, and interfaces
  (CLI/MCP) are all pluggable.
- **Domain-Driven** — the core is totally abstract and agnostic of platforms and
  interfaces, so it can be reused across CLI and MCP.
- **Agent-first** — MCP is a day-one interface, not a bolt-on: agents read,
  plan, and reconcile calendars through the same core the CLI drives.
- **Rust** — a single static toolchain: one cargo workspace, strict lints, and
  a pure core with no `std::fs`/network imports.

### Competitive landscape

A survey of existing work shows the idea is **not taken in its full form**:

- **Terraform providers for Google Calendar** (sethvargo, chewnoill) implement
  plan/apply but are HCL-coupled, Google-only, and non-reusable.
- **YAML calendar tools** (yaml2ical, yaml2ics, gnomon, timeblok) are
  export-only: they generate `.ics` files but cannot update, delete, detect
  drift, or reconcile against a live provider. Without stable IDs and recorded
  state they can only ever create duplicates.
- **Unified calendar SDKs** (calendar-sdk, recal, OneCal) provide multi-provider
  imperative APIs but no declarative DSL and no desired-state lifecycle.
- **Calendar MCP servers** (MarimerLLC, rauf543, hasna, lucasheight, caldav-mcp)
  expose calendars as agent tools but entangle provider logic with transport and
  have no declarative desired state.

The white space Cicada fills is the **integration** of: declarative YAML desired
state + plan/apply/drift lifecycle with stable IDs + a DDD core reusable across
interfaces + provider/state/auth ports. No single primitive is novel; the value
is in the combination and the lifecycle semantics.

### Forces

- Calendar APIs are **not transactional**. Apply must sequence operations and
  record outcomes honestly; we cannot fake atomicity.
- Recurrence is the **hard part** of calendars. Desired and remote recurrence
  must be compared semantically, not as strings.
- Not every provider supports every feature. The planner must consult
  capabilities and reject or warn on unsupported desired states.
- Google Meet is **not a separate provider** — it is a conferencing capability
  of the Google Calendar provider.
- The core must remain pure so it is testable without network, files, or
  providers, and reusable across interfaces.

## Decision

### 1. Domain-Driven Design with a pure core

The system is layered as:

```
Interfaces (CLI / MCP)         ── thin adapters, no business logic
        │
Application (use cases)        ── ValidateCalendar, PlanCalendar, ApplyPlan,
        │                         RefreshState, DetectDrift, QueryRange,
        │                         ShowEvent, FmtCalendar
        │
Domain (entities + services)   ── Calendar, Event, Recurrence, Attendee,
        │                         Reminder, Time, EventDiffer,
        │                         RecurrenceExpander, CapabilityChecker
        │
Ports (interfaces)             ── CalendarProvider, StateStore, AuthProvider,
                                  Clock, Logger, SecretStore, Http
```

**The core crate (`crates/cicada-core`) contains domain, application, and ports.
It has zero dependencies on providers, files, or platform specifics — no
`std::fs`, no `tokio`, no HTTP client types.** Use cases depend only on port
traits. This is what makes the core reusable across CLI and MCP — each
interface is a thin shell that wires adapters into the same use cases.

### 2. Ports defined in core, implemented by adapters

All cross-cutting concerns are ports defined in `core` and implemented outside
it:

| Port | Responsibility | First adapter | Future adapters |
|---|---|---|---|
| `CalendarProvider` | list/get/create/update/delete events, conferencing, freeBusy | `cicada-google` | `cicada-teams`, `cicada-caldav` |
| `StateStore` | persist recorded state (local ID → remote ID mapping) | `cicada-state` (JSON on disk) | `cicada-state-gcs`, `cicada-state-s3` |
| `AuthProvider` | acquire credentials | google oauth + service account in `cicada-google` | per-provider auth |
| `Clock` | injectable `now()` | system clock | fake clock (tests) |
| `Logger` | structured, leveled logging | `tracing` | `tracing` subscribers |
| `SecretStore` | persist tokens / refresh tokens | file-based | OS keychain |
| `Http` | injectable fetch | `reqwest` | recorded-fixture client (tests) |

`ProviderCapabilities` is first-class on `CalendarProvider`: attendee limits,
reminder kinds, recurrence edge cases, conferencing kinds. The planner consults
capabilities at plan time and emits warnings or errors for unsupported desired
states. This is what makes multi-provider actually work instead of silently
dropping fields.

### 3. Plan/Apply/Drift lifecycle with stable IDs and recorded state

Borrow the proven IaC workflow:

- **Desired state** — what the YAML declares.
- **Actual state** — what the provider currently has (live).
- **Recorded state** — a local cache (via `StateStore`) of the last reconciled
  actual state, keyed by the user's stable local IDs.
- **Plan** — diff desired vs recorded, plus recorded vs live (drift), producing
  an ordered set of operations: `Create | Update | Delete | Replace | NoOp`.
- **Apply** — execute operations via the provider port, persist new recorded
  state.
- **Drift** — recorded ≠ live (someone edited in the provider UI).

The **stable local ID** (`id: standup`) is the anchor that enables idempotent
update and delete across runs. Without it, apply can only ever create
duplicates — the failure mode of every export-only YAML tool.

### 4. Apply semantics: sequenced, idempotent, resumable

Calendar APIs are not transactional, so apply is sequenced and honest:

- Operations are ordered: `Delete` → `Create` → `Update` (minimizes conflicts).
- Each operation carries an idempotency key derived from
  `calendarId + localId + opKind + contentHash`.
- State is written **per-operation** as ops succeed (append-style journal), so a
  crash mid-apply leaves a valid, resumable state.
- `apply --resume` skips already-applied operations by checking the journal.
- Failed operations do not corrupt state; their results are recorded per-op.
- Conferencing creation is a sub-step of `Create`/`Replace` for the event it
  attaches to.
- Retries with exponential backoff and jitter happen at the HTTP adapter layer,
  respecting provider rate limits.

### 5. Recurrence: structured, raw RRULE, and structured-only

Recurrence is expressed in YAML in three ways, all normalized to an internal
`RecurrenceRule` with a single expander implementation:

- **Structured object** (ergonomic, common path):
  `{ freq: WEEKLY, byDay: [MO,TU,WE,TH,FR], interval: 1, count: 52 }`
- **Raw RRULE string** (escape hatch for exotic features like `BYSETPOS`):
  `rrule: "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"`
- **Structured-only** is the recommended path for new authors.

Desired and remote recurrence are compared **semantically** (normalized form),
not by string equality. For recurring events:

- A rule change → `Replace` (recreate, since instances differ).
- An instance-only change (exdate/override) → `Update` with exception patches.

`range` and `show` expand recurrence via the expander for local-only views; live
views expand remote rules.

### 6. DSL: Kubernetes-style YAML envelope

```yaml
apiVersion: cicada/v1
kind: Calendar
metadata:
  name: work
  timeZone: America/Sao_Paulo
spec:
  events:
    - id: standup
      summary: Daily Standup
      start: "09:00"
      duration: 30m
      recurrence: { freq: WEEKLY, byDay: [MO,TU,WE,TH,FR] }
      attendees:
        - { email: alice@x.com, required: true }
      conferencing: meet
      reminders: [{ popup: 10m }]
```

Composability:

- A root `cicada.yaml` manifest references multiple calendar files.
- `templates/` for reusable meeting shapes.
- `groups.yml` for shared attendee sets referenced by `$ref`.
- `conferencing: meet` is an **intent**, not a URL. The URL is provider output;
  the planner resolves the intent to the selected provider's meet implementation.

### 7. Validation pipeline (four layers, fail fast)

1. **Schema** (serde) — YAML shape.
2. **Domain invariants** — end > start, valid recurrence, no duplicate IDs.
3. **Capability check** — provider supports declared features (plan-time).
4. **Conflict check** (optional) — free/busy query against provider.

### 8. Cargo workspace layout

```
crates/
  cicada-core/           # domain + application + ports (pure, the gem)
    src/
      domain/            # Event, Calendar, Recurrence, Attendee, Reminder, Time
      application/       # use cases (Validate, Plan, Apply, Refresh, Drift, ...)
      ports/             # the 7 traits
      planning/          # EventDiffer, OperationPlanner, CapabilityChecker
  cicada-schema/         # serde schema + DSL parser (YAML -> domain)
  cicada-state/          # JsonFileStateStore
  cicada-google/         # CalendarProvider + AuthProvider impls (Calendar API + Meet)
  cicada-cli/            # `cicada` binary; wires adapters into core use cases
  cicada-mcp/            # `cicada-mcp` binary — MCP stdio server, same core
  cicada-testkit/        # fakes, conformance suites, golden fixtures
xtask/                   # cargo xtask check|test|install
```

`cicada-core` has zero deps on providers, files, or platform specifics — no
`std::fs`, no `tokio`, no HTTP types. This is what makes "reuse in CLI / MCP"
real rather than aspirational.

### 8a. MCP as a first-class interface

`cicada-mcp` is a separate binary serving MCP over stdio (like sepia's driver
binaries — agents spawn it per session). It links the same `cicada-core` use
cases as the CLI; the crate contains only transport: tool definitions, argument
decoding, and result rendering. Tool surface mirrors the use cases:

- read: `calendar.list`, `events.range`, `events.show`, `state.drift`
- plan: `plan.preview` (dry-run, always safe)
- write: `plan.apply` — gated: requires explicit user confirmation in the
  calling agent's permission flow; mutating ops are never implicit

The MCP server is how "calendar as code" composes with agents: an agent reads
`calendar.yaml`, calls `plan.preview`, shows the diff, and only after approval
invokes `plan.apply`.

Access model: an agent manages whatever calendars its credentials reach —
a dedicated/service-account calendar is "the agent's own", delegated or
shared human calendars are gated by provider permissions *and* the
apply-confirmation gate. All writes go through plan/apply; see ADR-0002
for the reconciliation contracts (managed boundary, tagging, conflict
and notification policy) that make this safe.

### 9. CLI surface

```
cicada init
cicada validate [files...]
cicada plan [--provider google] [--out plan.json]
cicada apply [--auto-approve] [--resume]
cicada refresh          # pull live -> update state
cicada drift            # recorded vs live
cicada range --from --to [--provider|--local]
cicada show <id>
cicada fmt
cicada provider add google   # OAuth flow / service account
cicada mcp                   # alias for the cicada-mcp binary (stdio)
```

### 10. Build order

1. `cicada-core` domain + ports — entities, value objects, traits (pure, unit
   tested).
2. `cicada-schema` — serde schema + parser + `ValidateCalendar` use case →
   `cicada validate` works.
3. `core/planning` + `PlanCalendar` → `cicada plan` works against an in-memory
   fake provider (`cicada-testkit`).
4. `cicada-state` + `ApplyPlan` + `RefreshState` + `DetectDrift` → full
   lifecycle with a fake provider.
5. `cicada-mcp` — stdio server exposing read + `plan.preview` tools against
   the fake provider; agent E2E before any Google code exists.
6. `cicada-google` (auth + calendar + meet) → real end-to-end.
7. `cli`/`mcp` polish: `show`, `range`, `fmt`, `provider add`, `plan.apply`
   tool with confirmation gating.
8. Golden tests for plans; HTTP fixture tests for the provider.

The pure core is testable and shippable before any Google code exists — which is
the whole point of the DDD split.

## Consequences

### Positive

- **Reusable core.** The same use cases power CLI and MCP with no
  business-logic duplication — agents get the identical lifecycle humans get.
- **Testable core.** Domain and planning logic is pure and tested with in-memory
  fakes — fast, deterministic, no network.
- **Provider-agnostic by construction.** Capabilities force the planner to
  respect what each provider actually supports.
- **Idempotent apply.** Stable IDs + recorded state mean re-running converges
  instead of creating duplicates.
- **Honest partial failure.** Per-op state journal makes apply resumable and
  crash-safe.
- **Extensible.** New providers, state backends, auth methods, and interfaces
  are new adapters, not core changes.

### Negative

- **More upfront structure.** Seven ports and a layered core is more machinery
  than a single-file script. Justified by the multi-interface and
  multi-provider goals.
- **State file is required.** Unlike export-only tools, apply needs recorded
  state to track remote IDs. This is a real dependency users must manage (commit
  it, or store in a remote backend).
- **Recurrence complexity.** Semantic recurrence comparison and instance
  overrides are genuinely hard. The structured form covers common cases; the
  RRULE escape hatch covers the rest, but the differ must handle both.
- **No fake transactions.** We cannot roll back a partial apply; we can only
  resume. Users must understand apply is convergent, not atomic.

### Risks and mitigations

- **Risk:** Core purity slips (I/O or platform types leak in).
  **Mitigation:** `cicada-core` has no `std::fs`/`tokio`/HTTP imports — enforce
  with a dependency allowlist in `cargo deny`/CI. Ports use portable types.
- **Risk:** Capability model becomes a dumping ground.
  **Mitigation:** Capabilities are typed and enumerated; unsupported features
  produce plan errors, not silent drops.
- **Risk:** State file format evolves and breaks existing states.
  **Mitigation:** State file is versioned (`apiVersion`); migrations are
  explicit and tested.

## Alternatives considered

### A. Terraform provider (HCL + plugin RPC)

Rejected. Couples the core to Terraform's plugin model and HCL, making it
non-reusable for CLI/MCP. The sethvargo provider is the cautionary example:
it works but cannot be reused outside Terraform.

### B. Export-only YAML → ICS (like yaml2ics/gnomon)

Rejected. Cannot update, delete, or detect drift. Without stable IDs and
recorded state, re-running creates duplicates. This is the failure mode of every
existing YAML calendar tool.

### C. Imperative unified SDK (like calendar-sdk)

Rejected as the *whole* solution, but its package split (`core` + `providers/*`
+ `mock`) validates our layout. We borrow the shape; we do not borrow the
imperative API. Our value is the declarative desired-state lifecycle on top.

### D. MCP server with provider logic in tool handlers

Rejected. Entangles business logic with transport. Our DDD core + ports keeps
MCP as a thin adapter, so the same core serves CLI and MCP.

### E. Stateless reconcile by matching heuristics (no state file)

Rejected. Matching desired to live by summary+time heuristics is fragile for
updates and deletes, and makes drift detection unreliable. Recorded state with
stable IDs is the only sound foundation for idempotent apply.
