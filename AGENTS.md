# Cicada

Calendars as code, in Rust — crates plus a `cicada` CLI and a
`cicada-mcp` server. Declare calendars in YAML, then plan/apply/drift
against live providers the way you manage infrastructure. Agents are
first-class users: `cicada-mcp` exposes the same core over MCP stdio.

## Layout

Ports-and-adapters. `cicada-core` is pure domain (no I/O); adapters
depend on it; `cicada-cli` and `cicada-mcp` are thin shells that wire
adapters into the same use cases.

| Crate | Role |
| --- | --- |
| `crates/cicada-core` | Calendar/Event/Recurrence IR, use cases, the 7 port traits, planner (pure) |
| `crates/cicada-schema` | serde schema + YAML DSL parser (`apiVersion: cicada/v1`) → domain |
| `crates/cicada-state` | `JsonFileStateStore` — recorded state + per-op apply journal |
| `crates/cicada-google` | `CalendarProvider` + `AuthProvider` impls (Calendar API + Meet) |
| `crates/cicada-mcp` | `cicada-mcp` binary — MCP stdio server, transport only |
| `crates/cicada-cli` | `cicada` binary — init/validate/plan/apply/refresh/drift/range/show/fmt/mcp |
| `crates/cicada-testkit` | fake provider + state store, conformance suites, golden fixtures |

## Boundary

cicada owns the desired-state lifecycle — declare, plan, apply, drift.
Anything an agent or human does to a calendar goes through use cases in
`cicada-core`; interfaces (CLI flags, MCP tools) contain no business
logic. Mutating MCP tools are confirmation-gated — `plan.apply` is never
implicit.

## Commands

```bash
cargo xtask check          # fmt + clippy -D warnings
cargo xtask test           # whole suite (nextest if present)
cargo xtask install        # release-build cicada + cicada-mcp → ~/.local/bin
cargo xtask coverage       # llvm-cov report (HTML → target/llvm-cov/html);
                           # extra args pass through, e.g. --fail-under-lines 100
```

Git hooks live in `.hooks/` (`core.hooksPath` = `.hooks`):
pre-commit runs `cargo fmt --check`, pre-push `cargo xtask check`.

## Rules

- Workspace lints are strict (pedantic + `unwrap_used` deny-ish in
  `Cargo.toml`); no warnings tolerated. Tests `#![allow]` liberally.
- No `unsafe`.
- `cicada-core` stays pure — no `std::fs`, no `tokio`, no HTTP types.
  I/O lives in adapter crates behind port traits.
- Blocking I/O runs on `spawn_blocking` when the core goes async.
- Golden fixtures in `crates/cicada-testkit/fixtures/` are frozen
  output — regenerate deliberately if the wire changes.
- `.config/nextest.toml` is the test-runner config.

## Docs

- `project/adrs/0001-foundational-architecture.md` — the decision record.
- `project/adrs/0002-reconciliation-contracts.md` — managed boundary,
  remote tagging, safe resume, conflict/notification policy, versioned
  state and plan formats. Read before touching apply semantics.
- `docs/comparison.md` — competitive landscape.
