# Cicada

Calendars as code, in Rust — crates plus a `cicada` CLI and a
`cicada-mcp` server. Declare calendars in YAML, then plan/apply/drift
against live providers the way you manage infrastructure. Agents are
first-class users: `cicada-mcp` exposes the same core over MCP stdio.

The `cicada` CLI is also a **generated surface** (org proposal P-021)
*and* a tamias `bins[]` pack (T-012): `crates/cicada-cli` is the
engine lib (`cicada::command()`/`run_from`) with **no `[[bin]]`** —
the `cicada` command ships as `libraries/cicada` (`bins:
[{name: cicada, shim: cli, driver: cicada}]`), and
`tamias-driver-cicada` derives an ops manifest from its clap tree —
`cli.<verb>` wire queries over `serve`, the passthrough `cli` shim
verb (strips `--dir`/`--name` → `cicada::run_from`, inherited stdio),
plus feature-gated `mcp`. `cicada-mcp` stays: its
`plan.preview`→`plan.apply` preview-digest gate is session state
the generated ops→tools mapping can't express.

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
| `crates/cicada-mcp` | `cicada-mcp` binary — MCP stdio server, transport only (kept: digest-gated apply) |
| `crates/cicada-cli` | `cicada` engine lib — init/validate/plan/apply/refresh/drift/range/show/fmt/mcp; `Cli`/`command()`/`dispatch`/`run_from` for drivers. **No `[[bin]]`**: the command ships as the tamias pack in `libraries/cicada/` (T-012); engine e2e tests live in `cicada-driver/tests/` and spawn `tamias-driver-cicada run` |
| `crates/cicada-driver` | `tamias-driver-cicada` binary — triple surface: tamias `type: cicada` content driver (install validates, deploy plans, uninstall reports) **and** pavo engine-ops driver (`manifest`/`capabilities`/`ops`/`run`/`serve`, `cli` shim passthrough + feature-gated `mcp`) |
| `libraries/cicada` | the tamias pack — `tamias.yaml` declares `bins: [{name: cicada, shim: cli, driver: cicada}]`; `cargo xtask pack` stages `drivers/bin/<triple>/tamias-driver-cicada` (gitignored) |
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
cargo xtask pack           # stage libraries/cicada driver payload
                           # (release tamias-driver-cicada → drivers/bin/<triple>/)
cargo xtask install        # release-build cicada-mcp + tamias-driver-cicada →
                           # ~/.local/bin (the `cicada` command itself is the
                           # tamias pack's generated bins[] shim — `cargo xtask
                           # pack` + `tamias install --allow-drivers ./libraries/cicada`)
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

<!-- myco:agent-awareness -->

## Agent awareness

- Session start / any work discussion: run `myco status --prompt`
  before proposing next steps — know the board first.
- User describes new work → offer to capture it (`myco todo add`,
  `issue new`, `proposal new`). Don't wait to be asked.
- Picking up a task: `myco resume <ID>` is the briefing.
- Wrapping up: `myco handoff <ID>` anything resumable; keep statuses
  honest (claimed → in-progress → blocked/done).

<!-- /myco:agent-awareness -->

<!-- sturn:agent-awareness -->

## Sturn awareness

- **No PRs.** The pushed branch/commit is the deliverable; review
  streams continuously (sturn WAL), merge is a human/sturn call.
- sturn-wired repo (`sturn.toml` present): `sturn claim "<glob>"
  --ws <ws>` *before* edits — claims surface conflicts in ms.
- Ops via `sturn write|rename|delete`; `sturn snap` folds the ws
  into a real commit; `sturn push` publishes heads + refs/sturn/*.
- `sturn leave` on exit — drops the actor's claims.

<!-- /sturn:agent-awareness -->

