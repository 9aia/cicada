# cicada

*The bug that shows up on time.*

Calendars as code, in Rust. Declare your calendars in YAML. Plan, apply,
and detect drift against live providers — the same way you manage
infrastructure. With an MCP server so agents can own and reconcile
calendars too.

## Install

```sh
cargo install --git https://github.com/9aia/cicada cicada           # `cicada`
cargo install --git https://github.com/9aia/cicada cicada-mcp       # `cicada-mcp`
cargo install --git https://github.com/9aia/cicada --features cli,mcp cicada-driver   # `tamias-driver-cicada`
```

## Quickstart

```sh
cicada init            # scaffold cicada.yaml + calendars/work.yaml
cicada validate        # schema + domain checks
cicada plan            # diff desired vs recorded vs live
cicada apply           # reconcile (idempotent, resumable)
cicada drift           # detect out-of-band changes
```

The bundled `fake` provider demos the full lifecycle with no
credentials; `cicada provider add google` wires a real calendar.

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
cicada range --from today --to +7d
cicada show standup
cicada fmt --check
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

### Distribution — tamias

A cicada project *is* a tamias package: `tamias.yaml` with
`type: cicada` wrapping `cicada.yaml` + `calendars/`. The
`tamias-driver-cicada` binary (crate `cicada-driver`) is the content
driver tamias dispatches lifecycle verbs to:

| verb | what it does |
| --- | --- |
| `install` | validate manifest + calendars (schema + domain); nothing applied |
| `deploy` | write `driver.json` + `plan.json` under the state dir — **plan-only**, never applies |
| `uninstall` | drift/managed-inventory report; deletes no remote events |
| `status` | recorded-state summary; no provider calls |
| `config-schema` | JSON Schema of the optional `--config` file |

`deploy` staying plan-only is deliberate: ADR-0002's confirmation
gate means apply remains an explicit `cicada apply` (or MCP
`plan.apply`) decision. Provenance — which ref/sha produced the
desired state — is recorded by tamias itself.

```sh
cargo build -p cicada-driver          # → tamias-driver-cicada on PATH
tamias install ./examples/tamias-bundle
tamias deploy cicada-demo --config ./examples/tamias-bundle/driver.yaml
tamias driver exec cicada-demo status --config ./examples/tamias-bundle/driver.yaml
```

See `examples/tamias-bundle/README.md` for the full walkthrough
(`provider: fake` needs no credentials).

### Orchestration — peregrin

No dedicated driver: peregrin campaigns compose cicada through the
existing `shell` and `schedule` drivers — the same verbs an operator
runs, as campaign steps. `examples/campaigns/morning-brief.yaml` is
a working reference:

```yaml
- id: brief
  driver: schedule
  on: [start]
  emits: [brief.due]
  params: { cron: "30 7 * * 1-5", timezone: America/Sao_Paulo }
- id: range
  driver: shell
  on: [brief.due]
  emits: [brief.range.done, brief.range.failed]
  params:
    cmd: "cicada range --from today --to +1d"
    emit:
      "0": "brief.range.done"
      default: "brief.range.failed"
```

`driver: shell` runs `params.cmd` under the system shell; `params.emit`
maps `rc` → event type (falling back to `cmd.done`/`node.failed`), so
the campaign reads occurrences from the done event's `stdout`.

Recurring drift-watch (`schedule` → `cicada drift` → notify on
non-empty) and provision-on-event (`github-webhook` → `cicada
plan`/`apply` for a team bundle) are the same shape. Calendar events
as *triggers* (cicada → peregrin entity plane) is a possible
follow-up; today `cicada-mcp` gives agents the same core directly.

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
  cicada-cli/        `cicada` engine lib + binary; wires adapters into core use cases
  cicada-driver/     tamias-driver-cicada — `type: cicada` content driver +
                     pavo engine-ops driver (`ops`/`serve`/`run`; `cli`/`mcp`
                     features generate CLI + MCP surfaces from the ops manifest)
  cicada-testkit/    fakes, conformance suites, golden fixtures
xtask/               cargo xtask check|test|install
```

## Status

v0.1 feature-complete. Everything works end-to-end against the
file-backed `fake` provider (`--provider fake`, the default):
`init` → `validate` → `plan` → `apply` → `drift`/`refresh` →
`range`/`show` → `fmt`, plus `cicada mcp` serving the same use cases
over stdio with a confirmation-gated `plan.apply`.

Google Calendar is implemented (auth: service-account JWT, OAuth
installed app, static token; Calendar API v3 + Meet; etag
conditional writes; `sendUpdates`) and exercised by HTTP-fixture
tests — live runs are credential-gated:

```sh
cicada provider add google                          # OAuth loopback/paste flow
cicada provider add google --service-account key.json
cicada plan --provider google
```

Env credentials (`GOOGLE_APPLICATION_CREDENTIALS`,
`CICADA_GOOGLE_CLIENT_ID`/`_SECRET` + refresh token,
`CICADA_GOOGLE_ACCESS_TOKEN`) also work and take precedence. Teams,
CalDAV, and others follow via the same `CalendarProvider` port.

## Development

Rust workspace, sepia-style conventions.

```sh
cargo xtask check      # fmt + clippy -D warnings
cargo xtask test       # whole suite (nextest if present)
cargo xtask install    # release-build cicada + cicada-mcp → ~/.local/bin
```

## Docs

Full docs: **[9aia.com/cicada](https://9aia.com/cicada/)** — getting-started
tutorial, how-to guides, CLI/DSL/MCP/state reference, architecture
explanation (Diátaxis).

[CONTRIBUTING](CONTRIBUTING.md) · [SECURITY](SECURITY.md) ·
[LICENSE](LICENSE) (MIT) · [project](project/index.md) — TODO, backlog,
ADRs

## License

MIT
