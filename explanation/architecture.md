# Architecture

cicada is a Rust workspace built on ports-and-adapters (DDD). The
point of the split: the same reconciliation engine powers the CLI a
human runs and the MCP server an agent drives — with zero
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

- **Two interfaces, one engine.** `cicada` and `cicada-mcp` are thin
  shells that wire adapters into the same use cases. Agents get the
  identical lifecycle humans get — `plan.apply` calls the same
  `ApplyPlan` the `apply` verb calls.
- **Testability.** Domain and planning logic run against in-memory
  fakes; the Google adapter is exercised by recorded HTTP fixtures.
  The full lifecycle shipped and passed end-to-end tests before any
  Google code existed.
- **Extensibility.** New providers, state backends, auth methods, and
  interfaces are new adapters. The core never changes.

## Crates

| Crate | Role |
|---|---|
| `cicada-core` | domain + application + ports (the reusable gem) |
| `cicada-schema` | serde schema + YAML DSL parser → domain |
| `cicada-state` | `JsonFileStateStore` — recorded state + journal |
| `cicada-google` | `CalendarProvider` + `AuthProvider` impls (Calendar API + Meet) |
| `cicada-shell` | shared interface plumbing: provider wiring, file resolution, time-bound parsing |
| `cicada-mcp` | `cicada-mcp` binary — MCP stdio server, transport only |
| `cicada-cli` | `cicada` binary — verbs wire adapters into use cases |
| `cicada-testkit` | fakes, conformance suites, golden fixtures |

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
