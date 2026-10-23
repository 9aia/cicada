# cicada

*The bug that shows up on time.*

Calendars as code, in Rust. Declare your calendars in YAML. Plan, apply,
and detect drift against live providers — the same way you manage
infrastructure. With an MCP server so agents can own and reconcile
calendars too.

```yaml
# calendar.yaml
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

```sh
cicada plan      # diff desired vs recorded vs live
cicada apply     # reconcile: create, update, delete, replace
cicada drift     # detect out-of-band changes
cicada range --from today --to +7d
cicada mcp       # serve MCP over stdio — same core, for agents
```

## Why

Calendar tools fall into four camps, none of which do this:

- **YAML → ICS generators** (yaml2ical, yaml2ics, gnomon) declare meetings in
  text but are export-only. They can't update, delete, or detect drift — without
  stable IDs and recorded state, re-running just creates duplicates.
- **Terraform providers** (sethvargo/terraform-provider-googlecalendar) do
  plan/apply but are HCL-coupled, Google-only, and their core can't be reused
  outside Terraform.
- **Unified calendar SDKs** (calendar-sdk, recal, OneCal) give a clean
  multi-provider API but are imperative — no declarative desired state, no
  lifecycle.
- **Calendar MCP servers** expose calendars to AI agents but entangle provider
  logic with transport and have no desired-state model.

**cicada is the integration of those ideas**: a declarative YAML DSL plus a
plan/apply/drift lifecycle with stable IDs and recorded state, built on a
pure-Rust core that is reusable across CLI and MCP. Agents are first-class
users — the same reconciliation an engineer runs with `cicada apply` is what an
agent reaches through MCP tools. No single primitive here is novel — the value
is in the combination and the lifecycle semantics.

## How it works

```
Interfaces (CLI / MCP)         thin adapters, no business logic
        │
Application (use cases)        Validate · Plan · Apply · Refresh · Drift ·
        │                      QueryRange · Show · Fmt
        │
Domain (entities + services)   Calendar · Event · Recurrence · Attendee ·
        │                      Reminder · Time · EventDiffer ·
        │                      RecurrenceExpander · CapabilityChecker
        │
Ports (traits)                 CalendarProvider · StateStore · AuthProvider ·
                               Clock · Logger · SecretStore · Http
```

The core is pure — zero dependencies on providers, files, or platform
specifics. Use cases depend only on port traits. Every provider, state backend,
auth method, and interface is an adapter. The same core powers `cicada` (the
CLI) and `cicada-mcp` (the agent interface).

### Plan / Apply / Drift

- **Desired** — what your YAML declares.
- **Recorded** — a state file mapping your stable IDs to remote IDs (the cache).
- **Live** — what the provider actually has right now.
- **Plan** — diff desired vs recorded, plus recorded vs live, into ordered
  `Create | Update | Delete | Replace | NoOp` operations.
- **Apply** — execute operations via the provider, persist new recorded state.
- **Drift** — recorded ≠ live (someone edited in the provider UI).

Calendar APIs aren't transactional, so apply is **sequenced, idempotent, and
resumable**: operations are ordered, each carries an idempotency key, state is
written per-operation as a journal, and `apply --resume` skips already-applied
ops after a crash.

### MCP for agents

`cicada-mcp` (or `cicada mcp`) serves the same use cases over MCP stdio —
agents read, plan, and reconcile through the identical core the CLI uses.
MCP is a day-one interface, not a bolt-on.

What that means in practice:

- **An agent's own calendar** — point cicada at a dedicated or
  service-account calendar; the agent authors `calendar.yaml` and
  reconciles it as code. Re-runs converge, never duplicate.
- **A human's calendar** — same flow, gated twice: the provider's
  permission model (delegated/shared calendar or the human's token) and
  `plan.apply`'s confirmation gate. Nothing mutates implicitly.
- **Drift is bidirectional** — human edits in the provider UI surface
  as drift and flow back into the loop; cicada never deletes events it
  didn't create (see the managed boundary in ADR-0002).

The agent decides *when* — `events.range` plus free/busy queries expose
gaps. cicada supplies the *how*: every mutation is a previewed,
approved diff.

### Providers and capabilities

Google Calendar is the first provider; Google Meet is a conferencing capability
of it, not a separate provider. `conferencing: meet` is an intent the planner
resolves to the selected provider's implementation.

Each provider declares `ProviderCapabilities` (attendee limits, reminder kinds,
recurrence edge cases, conferencing kinds). The planner consults capabilities at
plan time and rejects or warns on unsupported desired states — so multi-provider
support actually works instead of silently dropping fields.

### Recurrence

Three forms, all normalized to one internal rule with a single expander:

```yaml
recurrence: { freq: WEEKLY, byDay: [MO,TU,WE,TH,FR], count: 52 }   # structured
recurrence: { rrule: "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR" }          # raw RFC 5545
```

Desired and remote recurrence are compared **semantically**, not as strings.

## Project layout

```
crates/
  cicada-core/       domain + application + ports (pure, the reusable gem)
  cicada-schema/     serde schema + DSL parser (YAML -> domain)
  cicada-state/      JsonFileStateStore (GCS/S3 later via the same port)
  cicada-google/     CalendarProvider + AuthProvider impls
  cicada-mcp/        MCP server — same core, agent transport
  cicada-cli/        commands; wires adapters into core use cases
  cicada-testkit/    fakes, conformance suites, golden fixtures
xtask/               cargo xtask check|test|install
```

## Status

Early. Architecture is decided
([ADR-0001](project/adrs/0001-foundational-architecture.md)) and the
reconciliation contracts are pinned
([ADR-0002](project/adrs/0002-reconciliation-contracts.md)) — managed
boundary, remote tagging, safe resume, conflict and notification
policy, versioned state/plan formats. Implementation is in progress.
Google Calendar is the first provider; Teams, CalDAV, and others follow
via the same `CalendarProvider` port. MCP ships alongside the CLI, not
after it.

## Development

Rust workspace, sepia-style conventions.

```sh
cargo xtask check      # fmt + clippy -D warnings
cargo xtask test       # whole suite (nextest if present)
cargo xtask install    # release-build cicada + cicada-mcp → ~/.local/bin
```

## License

MIT
