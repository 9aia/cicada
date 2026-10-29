# Plan, apply and drift

cicada borrows the infrastructure-as-code lifecycle and adapts it to
calendars, whose APIs are not transactional. This page is the mental
model: what the three states are, why apply is safe by construction,
and what the contracts guarantee.

## Three states

- **Desired** — what your YAML declares.
- **Recorded** — `.cicada/state.yaml`: the last reconciled state,
  keyed by your stable local ids, plus the apply journal.
- **Live** — what the provider actually has right now.

The **stable local id** (`id: standup`) is the anchor. Export-only
calendar tools fail precisely here: without stable ids mapped to
remote ids, a re-run can only create duplicates. Recorded state is
what lets cicada update and delete, not just create.

- **Plan** diffs desired vs recorded (what you changed) *and* recorded
  vs live (what changed out-of-band) into ordered operations:
  `create`, `update`, `delete`, `replace`, `noop`.
- **Apply** executes the plan and persists new recorded state.
- **Drift** is recorded ≠ live — the remote edit a colleague made in
  the provider UI.

## The managed boundary

The managed set is *exactly* recorded state. Consequences:

- `delete` ops are generated only for local ids recorded but absent
  from your YAML.
- Live events unknown to recorded state are never modified or
  deleted — a human's lunch on a managed calendar is untouchable.
- Two cicada sources (two repos, an agent and a human) can share one
  provider calendar, each reconciling only its own recorded slice.

Every event cicada writes carries provider-side tags — on Google,
`extendedProperties.private["cicada:id"]` plus `cicada:managed` and a
`cicada:state` content hash. Tags, not guesses, delimit the boundary:
they enable state rebuild, adoption reporting, and safe resume.

## Apply is sequenced, idempotent, resumable

Calendar APIs have no transactions, so cicada is honest about
partiality instead of faking atomicity:

- Ops run in a fixed order — `delete`, `replace`, `create`, `update`,
  `noop` — minimising conflicts.
- Each op carries an idempotency key
  (`calendarId | localId | opKind | contentHash`); providers that
  honour request ids use it.
- State is written **per op**, as a journal inside the versioned
  state document. A crash mid-apply leaves a valid, resumable file.
- `apply --resume` skips journaled ops. And because the failure
  window is "provider write landed, journal write crashed", resume
  also reconciles by `cicada:id` tag — a `create`/`replace` that
  finds its tag already live is journalled `skippedAlreadyApplied`,
  not duplicated. This check runs on every apply, which is also how a
  lost state file gets rebuilt.
- A failed op is journalled `failed` with its error kind and does not
  abort the run — the report carries every outcome.

Apply is **convergent, not atomic**. There is no rollback; there is
resume.

## Conflict and notification policy

Apply performs an implicit refresh of the managed set first. If live
diverged from recorded for an event the plan touches:

- **Default:** the op fails with a `conflict` error; state is
  untouched for it. `cicada refresh` + re-plan resolves.
- **`--force`:** desired wins; the live edit is overwritten (logged).
  The recorded etag is dropped so the write lands unconditionally.

Where the provider supports conditional writes (Google: etag →
`If-Match`), `replace`/`update` carry the recorded etag — optimistic
concurrency where available, conflict checking otherwise.

Providers email attendees on writes, so `sendUpdates` is a
first-class knob (`spec.apply.sendUpdates`, `--notify`, the
`plan.apply` parameter) defaulting to `externalOnly` — drift repairs
shouldn't spam the team.

## Versioned public contracts

The state file (`cicada/state-v1`) and plan output
(`cicada/plan-v1` via `plan --out`) are versioned documents that CI
and agents can depend on; migrations are explicit. Plan findings
carry severity — `warning` informs, `error` blocks apply (capability
gaps surface before anything writes).

## Why this shape

The alternatives were rejected for concrete reasons: Terraform
couples the core to a plugin RPC model; export-only tools can't
update, delete, or detect drift; heuristic stateless matching
(summary + time) is fragile for exactly the updates and deletes that
matter. Stable ids plus recorded state is the only sound foundation
for idempotent apply — and the journal is what makes an untransactional
world resumable anyway.
