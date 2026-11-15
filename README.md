# cicada

*The bug that shows up on time.*

Calendars as code, in Rust. Declare your calendars in YAML. Plan, apply,
and detect drift against live providers — the same way you manage
infrastructure. With an MCP server so agents can own and reconcile
calendars too.

## Install

The `cicada` command is a tamias pack (`libraries/cicada`) — a
generated `bins[]` shim → `tamias-driver-cicada` → the engine lib.
There is no cargo-installed `cicada` binary. With `tamias` installed:

```sh
tamias install 9aia:cicada@<version>    # org registry (needs TAMIAS_REGISTRY_TOKEN)
```

From a checkout:

```sh
cargo xtask pack                                  # stage the driver payload
tamias install --allow-drivers ./libraries/cicada  # deploys `cicada`
```

Or install the driver to `~/.local/bin` with
`cargo xtask install` (release builds; the `cicada` command itself
still comes from the tamias pack).

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
tamias-driver-cicada mcp  # serve the ops manifest as MCP tools — same core, for agents
```

## Why

Calendar tools fall into four camps — YAML→ICS generators (export-only,
no lifecycle), Terraform providers (HCL-coupled, single-provider),
unified calendar SDKs (imperative, no desired state), and calendar MCP
servers (no reconciliation model). **cicada is the integration of those
ideas**: a declarative YAML DSL plus a plan/apply/drift lifecycle with
stable IDs and recorded state, on a pure-Rust core reusable across CLI
and MCP. The competitive landscape analysis lives on the docs site
([explanation/landscape](https://9aia.com/cicada/explanation/landscape)).

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
specifics. Every provider, state backend, auth method, and interface is
an adapter; the same core powers `cicada` and `tamias-driver-cicada mcp`.

**Plan/apply/drift**: *desired* (your YAML) vs *recorded* (a state file
mapping stable IDs to remote IDs) vs *live* (what the provider has).
Plan diffs them into ordered `Create | Update | Delete | Replace | NoOp`
ops; apply executes idempotently and resumably (`--resume` survives
crashes); drift reports recorded ≠ live. Mutating surfaces are
confirmation-gated: `plan` issues a `planDigest` and `apply
--plan-digest` enforces it — nothing mutates implicitly.

**Providers**: Google Calendar is the first provider (service-account
JWT, OAuth, static token; Calendar API v3 + Meet). Each provider
declares capabilities the planner consults at plan time. cicada never
deletes events it didn't create — see ADR-0002 for the managed boundary.

**Distribution**: a cicada project *is* a tamias package
(`tamias.yaml` with `type: cicada`). `tamias-driver-cicada` is the
content driver — `install` validates, `deploy` writes plan-only state,
`uninstall` reports drift; apply stays an explicit `cicada apply`
decision. A worked bundle lives in `examples/tamias-bundle/`.

**Orchestration**: peregrin campaigns compose cicada through the
`shell` and `schedule` drivers — see
`examples/campaigns/morning-brief.yaml`.

## Project layout

Ports-and-adapters Rust workspace — `cicada-core` is pure domain;
adapters (`-schema`, `-state`, `-google`) plug in behind its port
traits; `cicada-cli`/`cicada-driver` are thin shells. The dev map lives
in [AGENTS.md](AGENTS.md); the generated crate map is on the docs site
([architecture](https://9aia.com/cicada/explanation/architecture)).

## Status

WIP — the full lifecycle works end-to-end against the file-backed
`fake` provider (the default): `init` → `validate` → `plan` →
`apply` → `drift`/`refresh` → `range`/`show` → `fmt`/`list`, plus
`tamias-driver-cicada mcp` over the same core. The Google Calendar
provider is implemented and fixture-tested — live runs are
credential-gated; see the
[providers reference](https://9aia.com/cicada/reference/providers).

## Development

```sh
cargo xtask check      # fmt + clippy -D warnings
cargo xtask test       # whole suite (nextest if present)
cargo xtask pack       # stage libraries/cicada driver payload
cargo xtask install    # release-build tamias-driver-cicada → ~/.local/bin
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
