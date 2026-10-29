+++
id = "ADR-0002"
title = "ADR-0002: Reconciliation Contracts — the decisions v0.1 must not retrofit"
status = "open"
priority = "medium"
+++

# ADR-0002: Reconciliation Contracts — the decisions v0.1 must not retrofit

- **Status:** Proposed
- **Date:** 2026-10-23
- **Decision makers:** Project author
- **Supersedes:** none
- **Superseded by:** none

ADR-0001 fixed the architecture. This ADR fixes the *contracts* — the
semantics that are cheap to decide now and expensive to change after
users have state files, remote events, and CI pipelines depending on
them. Every item here is a decision, not a redesign.

## 1. Managed boundary — cicada only deletes what it created

The managed set is **exactly the recorded state**: events whose local
IDs map to remote IDs in the state file. Consequences:

- `Delete` ops are generated only for local IDs in recorded state but
  absent from desired YAML.
- Live events unknown to recorded state are **never** modified or
  deleted — a human's lunch on a managed calendar is untouchable.
- This is also what makes multi-writer safe: two repos/agents can manage
  disjoint event sets on the same provider calendar, each reconciling
  only its own recorded slice.

## 2. Remote tagging — `cicada:id` on every managed event

Every event cicada writes carries provider-side metadata
(Google: `extendedProperties.private["cicada:id"] = <localId>` plus
`cicada:managed = "1"` and `cicada:state` content hash). This enables:

- **State rebuild** — a lost state file is reconstructed by listing
  tagged live events.
- **Adoption** — `cicada adopt` can claim a pre-existing untagged event
  into state instead of duplicating it.
- **Safe resume** — see §3.
- **Fleet safety** — tags, not guesses, delimit the managed boundary
  when several cicada sources share one calendar.

## 3. Resume reconciles ambiguous ops — the journal alone is not enough

Failure window: provider write succeeds, journal write crashes. The
journal says "not applied" but live says "applied". Naive resume would
re-`Create` and duplicate. Therefore:

- On `--resume`, before re-executing any `Create`/`Replace`, cicada
  queries the provider for the `cicada:id` tag; a hit means the op
  already landed → journal it and skip.
- `Update`/`Delete` are naturally idempotent against remote IDs.
- The port contract must expose `findByTag(localId)` on
  `CalendarProvider` for this to work.

## 4. Conflict policy — never silently overwrite a live edit

Apply performs an implicit refresh of the managed set first. If
`live ≠ recorded` for an event the plan touches (out-of-band edit):

- **Default:** fail that op with a drift-conflict error; state is
  untouched for it. `cicada refresh` + re-plan resolves.
- `--force`: desired wins; the live edit is overwritten (logged).
- `Replace` ops carry the recorded remote ETag/sequence when the
  provider supports conditional writes — optimistic concurrency where
  available, conflict check otherwise.

## 5. Notification policy — `sendUpdates` is a first-class knob

Providers email attendees on writes. Without a knob, every drift fix
spams the team. `spec.apply.sendUpdates: none | externalOnly | all`
(default `externalOnly`); also an `--notify` CLI flag and a
`plan.apply` tool parameter. Capability-checked: providers that can't
suppress notifications produce a plan warning.

## 6. Time model — wall-time + IANA zone, never bare instants

Recurring events are declared in local wall time (`"09:00"` +
`metadata.timeZone`). The domain `Time` type stores
`(LocalDateTime, TzId)` and materializes instants at expansion — so
DST shifts keep "09:00 standup" at 09:00 local. Bare-UTC storage of
recurring times is the classic calendar bug; it is banned in core.
Non-recurring absolute datetimes may carry explicit offsets.

## 7. State file contract — versioned, locked, XDG paths

```yaml
# .cicada/state.yaml (or CICADA_STATE path)
apiVersion: cicada/state-v1
calendars:
  work:
    provider: google
    calendarId: primary
    events:
      standup: { remoteId: "abc123", hash: "sha256:…", appliedAt: "…" }
journal: []   # per-op entries, appended as ops land
```

- Format versioned via `apiVersion`; migrations are explicit, tested.
- **Locking:** apply takes an advisory lock on the state file
  (`fd-lock`). Concurrent applies from CLI and a long-running MCP
  server collide without it.
- **Paths:** config `~/.config/cicada/`, state default
  `.cicada/state.yaml` next to the manifest (committable), secrets via
  `SecretStore` file adapter at `~/.local/state/cicada/secrets` mode
  0600.

## 8. Error taxonomy — retryable vs fatal is a port contract

Every port error classifies as `Transient` (429, 5xx, network — backoff
+ resume-safe), `Conflict` (§4), `Auth` (surface token flow), or
`Fatal` (validation, capability). MCP tools surface the class, not just
the message — agents need it to decide retry vs ask-human.

## 9. Calendar addressing — declared names map through recorded state

`kind: Calendar` declares a *logical* calendar (`metadata.name: work`).
Binding to a provider calendarId is explicit:

```yaml
metadata:
  name: work
  provider: google        # selects the provider adapter
  calendarId: primary     # concrete binding for v0.1
```

v0.1 requires a pre-existing provider calendar (`calendarId` in
metadata or recorded state). Calendar *provisioning* (create the remote
calendar itself) is deferred — `kind: Calendar` is a container of
events for now, and the envelope leaves room for `spec.provider` blocks
later. The root manifest `cicada.yaml` lists calendar files:

```yaml
# cicada.yaml
apiVersion: cicada/v1
kind: Manifest
calendars: [calendars/work.yaml, calendars/personal.yaml]
```

## 10. Single write path — MCP has no ad-hoc create

All mutations flow through plan/apply on declared YAML. MCP exposes no
`events.create` tool: an agent wanting an ad-hoc event writes (or amends)
a `kind: Event`/`Calendar` document and calls `plan.apply`. Two write
paths would create unmanaged events that immediately show as drift —
one lifecycle keeps the world consistent.

## 11. Plan output is a public contract

`cicada plan --out plan.json` is consumed by CI and agents. The JSON
carries `apiVersion: cicada/plan-v1`, the ordered op list with
human-readable diffs, capability warnings, and drift summary. Versioned
like state — agents and pipelines can depend on it.

## Consequences

- Apply is **safe by construction**: bounded deletions, tag-reconciled
  resume, conflict-checked overwrites, locked state.
- The v0.1 surface is small but the contracts are stable — later
  versions add providers/backends/templates **without** revisiting
  these semantics.
- Two decisions explicitly deferred, with the envelope shaped to admit
  them: calendar provisioning, YAML templating/`$ref` composability.
