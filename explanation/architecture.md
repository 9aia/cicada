# Architecture

cicada is a Rust workspace built on ports-and-adapters (DDD). The
point of the split: the same reconciliation engine powers the `cicada`
CLI a human runs and the MCP tools an agent drives — with zero
business-logic duplication.

## Layers

```
Interfaces (CLI / MCP)        thin adapters, no business logic
        │
Application (use cases)       Validate · Plan · Apply · Refresh ·
        │                     Drift · QueryRange · Show · Fmt
        │
Domain (entities + services)  Calendar · Event · Recurrence ·
        │                     Attendee · Reminder · Time ·
        │                     EventDiffer · RecurrenceExpander ·
        │                     CapabilityChecker
        │
Ports (traits)                CalendarProvider · StateStore ·
                              AuthProvider · Clock · Logger ·
                              SecretStore · Http
```

`cicada-core` holds domain, application, and ports — and is **pure**:
no `std::fs`, no `tokio`, no HTTP types, no provider SDKs. Purity is
what makes "reuse across CLI and MCP" real rather than aspirational:
every side effect crosses a port trait, so the whole domain is
testable with in-memory fakes — fast, deterministic, no network.

## The seven ports

| Port | Responsibility | Shipped adapter | Possible adapters |
|---|---|---|---|
| `CalendarProvider` | list/get/create/update/delete events, `findByTag`, `freeBusy`, capabilities | `cicada-google` (+ `FakeFileProvider` for tests) | Teams, CalDAV |
| `StateStore` | recorded state + apply journal | `cicada-state` (versioned YAML on disk, locked, atomic) | GCS, S3 backends |
| `AuthProvider` | credential acquisition | Google OAuth / service-account / static token | per-provider auth |
| `Clock` | injectable `now()` | system clock | fake clock (tests, golden plans) |
| `Logger` | structured logging | console | `tracing` subscribers |
| `SecretStore` | token persistence | file store, `0600` | OS keychain |
| `Http` | injectable fetch | `reqwest` + retry wrapper | recorded-fixture client |

`ProviderCapabilities` is first-class on `CalendarProvider`: attendee
limits, reminder kinds, recurrence edge cases, conferencing kinds,
notification suppression, tagging, conditional writes. The planner
consults capabilities at plan time and rejects or warns on
unsupported desired states — this is what makes multi-provider work
honestly instead of silently dropping fields.

## Why a pure core

- **Two interfaces, one engine.** `cicada` (the tamias `bins[]`
  shim over `tamias-driver-cicada cli`) and the driver's `mcp`
  surface are thin shells that dispatch the same `cicada-core`
  use cases. Agents get the identical lifecycle humans get — every
  bounded verb is a `cli.<verb>` op, and `apply --plan-digest`
  carries the preview→apply gate engine-side.
- **Testability.** Domain and planning logic run against in-memory
  fakes; the Google adapter is exercised by recorded HTTP fixtures.
  The full lifecycle shipped and passed end-to-end tests before any
  Google code existed.
- **Extensibility.** New providers, state backends, auth methods, and
  interfaces are new adapters. The core never changes.

## Crates

<!-- crate-map -->
| crate | layer | purpose |
|---|---|---|
| `cicada-core` | domain | `cicada-core` — pure domain, ports, and planning scaffolding. Layered per ADR-0001: `domain` holds entities and value objects, `application` the use cases, `ports` the seven traits implemented by adap… |
| `cicada-google` | adapter | `cicada-google` — the Google Calendar provider adapter (ADR-0001 §10 step 6). |
| `cicada-schema` | adapter | `cicada-schema` — the YAML DSL: Kubernetes-style envelope (`apiVersion: cicada/v1`, `kind:`) parsed into `cicada-core` domain types. Two documents (ADR-0001 §6, ADR-0002 §9): |
| `cicada-shell` | adapter | `cicada-shell` — the plumbing every user-facing interface shares. `cicada-core` is pure; adapters live in their own crates. |
| `cicada-state` | adapter | `JsonFileStateStore` — the file-backed `StateStore` adapter (ADR-0002 §7). The state file is YAML in the `cicada/state-v1` shape: |
| `cicada` | engine | cicada — calendars as code: the engine lib + `cicada` CLI (init/validate/list/plan/apply/refresh/drift/range/show/fmt/provider) |
| `cicada-driver` | driver | tamias-driver-cicada — pavo engine-ops driver over the `cicada` CLI plus tamias content driver for `type: cicada` packages |
| `cicada-testkit` | testkit | `cicada-testkit` — reusable fakes and conformance helpers for tests across the workspace. |
| `xtask` | tooling | xtask — workspace automation: check / test / coverage / pack / install / fmt. |

## Single write path

All mutations flow through plan/apply on declared YAML — the CLI has
no `cicada event create` and MCP has no `events.create`. Two write
paths would create unmanaged events that immediately surface as
drift; one lifecycle keeps the world consistent.

## Time model

Recurring events are stored as `(local wall time, IANA zone)` and
materialised to instants at expansion — DST shifts keep a 09:00
standup at 09:00 local. Bare-UTC storage of recurring times is the
classic calendar bug and is banned in core.

See [Boundaries](boundaries.md) for owns / doesn't-own.

## Where it sits

In the org's layering — **delivery** (tamias distribution, scarab
config/templates, tarsi releases, bupha CI) → **memory** (myco
records, kuda code maps, sturn coordination, hypha substrate) →
**process** (cuora secrets, mimus proofs, **cicada** schedules,
lasius workspace, sepia sessions) → **agency** (peregrin campaigns,
pavo views, MCP fronts, agent harnesses).

Cicada is a **process-layer engine** — infrastructure-style
reconciliation for calendars. Humans drive `cicada` verbs; agents drive the same
verbs as `cli.*` ops or MCP tools; both land on the identical
`cicada-core` use cases, with the `planDigest` gate making
preview→apply explicit on every surface.
