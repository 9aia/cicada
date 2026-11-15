# Concepts

The domain vocabulary, each term defined once. Field-level
contracts live in [Reference](../reference/).

## Desired / recorded / live

The three states reconciliation diffs: **desired** is your YAML;
**recorded** is `.cicada/state.yaml` (last reconciled state +
apply journal); **live** is what the provider actually has.
Plan = desired vs recorded + recorded vs live; drift = recorded ≠
live. See [Plan, apply and drift](plan-apply-drift.md).

## Stable local id

`id: standup` in YAML — the anchor mapping declared events to
remote ids via recorded state and provider-side tags
(`cicada:id`, `cicada:managed`, `cicada:state`). Without it,
re-runs can only duplicate.

## Managed boundary

Exactly the recorded set: cicada only ever modifies/deletes events
it applied. Live events unknown to recorded state are untouchable —
a colleague's lunch on a managed calendar is safe.

## Manifest

`cicada.yaml` — lists the calendar YAML files cicada reconciles.
`cicada init` scaffolds it with `calendars/work.yaml` bound to the
`fake` demo provider.

## Provider / capabilities

A `CalendarProvider` adapter (`fake` file-backed demo, `google`
Calendar API v3) plus first-class `ProviderCapabilities` —
attendee limits, reminder kinds, conferencing, tagging, conditional
writes — consulted at plan time so unsupported desired states
reject or warn instead of silently dropping fields.

## planDigest

The preview→apply gate, engine-side: `plan` issues a digest
persisted beside the state file; `apply --plan-digest` must name
an issued, unspent digest whose re-plan still matches. Single-use;
a stale plan refuses. This replaced cicada-mcp's session-bound
digest when T-019 deleted that binary.

## The surfaces

- `cicada` — the tamias `bins[]` shim → `tamias-driver-cicada cli`
  → engine argv (init/validate/list/plan/apply/refresh/drift/
  range/show/fmt/provider).
- `cli.<verb>` ops + `run` — the pavo engine-ops wire.
- `mcp` — the ops manifest rendered as MCP tools (verbs as tool
  names). `provider.add` stays an interactive surface — OAuth
  needs stdin/browser, so it never becomes an op or tool.

## Single write path

There is no `events.create` anywhere: mutations flow only through
declared YAML → plan → apply. Two write paths would create
unmanaged events that immediately surface as drift.
