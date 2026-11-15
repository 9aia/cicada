# Decisions

ADR-lite: the load-bearing decisions, each with the alternative it
beat and the consequence it carries. The contracts live in
`project/adrs/` — ADR-0001 (foundational architecture), ADR-0002
(reconciliation contracts); read those before touching apply
semantics.

## Plan/apply/drift over CRUD

**Decision:** calendars reconcile desired-state YAML against
recorded + live state, like infrastructure — not imperative
create/update calls.

- *Alternative:* SDK-style CRUD (`events.create`, `events.update`).
- *Consequence:* stable ids + recorded state make re-runs
  idempotent and drift detectable; the cost is the three-state
  model every user must learn. See
  [Plan, apply and drift](plan-apply-drift.md).

## A pure core behind ports

**Decision:** `cicada-core` has zero I/O — seven port traits
(provider, state, auth, clock, logger, secret, http) with adapters
in sibling crates.

- *Alternative:* provider logic inside the CLI/MCP shells.
- *Consequence:* CLI, ops, and MCP surfaces dispatch identical use
  cases; the whole domain tests against in-memory fakes. Cost:
  every side effect crosses a trait boundary.

## The `cicada` command is a tamias shim

**Decision:** no `[[bin]]` in `cicada-cli` — the command ships as
`libraries/cicada` (`bins[]` → `tamias-driver-cicada cli`), and
`tamias-driver-cicada mcp` renders the ops manifest as MCP tools.

- *Alternative:* a hand-rolled binary plus a bespoke `cicada-mcp`.
- *Consequence:* one engine, three surfaces (argv, ops, MCP) with
  zero logic duplication; distribution rides tamias like every org
tool. T-019 deleted `cicada-mcp` for exactly this reason.

## planDigest instead of session state

**Decision:** the preview→apply gate persists issued digests
beside the state file; `apply --plan-digest` enforces issued +
unspent + still-matching.

- *Alternative:* cicada-mcp's in-session digest (deleted, T-019).
- *Consequence:* the gate survives process boundaries and rides
  ordinary `cli.*` ops — any surface can enforce it without shared
  session state.

## Mutations are confirmation-gated

**Decision:** `apply` prompts unless `--auto-approve`; wire ops
fail closed (null stdin ⇒ abort); `provider.add` never becomes an
op because OAuth needs stdin/browser.

- *Alternative:* trust the caller.
- *Consequence:* agents can plan freely but apply only with
  explicit consent params — the same gate humans get.
