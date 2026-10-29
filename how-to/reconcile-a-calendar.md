# Reconcile a calendar

This guide shows you how to drive the plan/apply/drift loop day to
day: change events, delete them, recover from crashes, and handle
out-of-band edits. It assumes you have a project set up (see
[Getting started](../tutorials/getting-started.md)).

## The everyday loop

```bash
$EDITOR calendars/work.yaml   # declare what you want
cicada validate               # catch schema + invariant errors
cicada plan                   # preview the diff
cicada apply                  # confirm, then reconcile
```

Re-running `plan` after a clean apply reports only `noop`s — cicada
converges, never duplicates.

## Change an event

Edit the YAML (summary, times, attendees — anything under
`spec.events[]` keeping the same `id`), then `plan` and `apply`. The
plan shows an `update` op with a field-level diff. Changing a
recurrence rule produces a `replace` (delete + recreate), because the
instance sets differ.

## Delete an event

Remove the entry from `spec.events[]` and apply — cicada generates a
`delete` for any `id` that is in recorded state but absent from your
YAML.

Cicada **never deletes events it did not create**. The managed set is
exactly what recorded state contains; a colleague's lunch on the same
calendar is untouchable, and two cicada projects can share one
provider calendar managing disjoint event sets.

## Confirm, or not

`cicada apply` prints the plan and prompts `[y/N]` before writing.
Automate with:

- `--auto-approve` — skip the prompt (CI, scripts).
- `--notify none|externalOnly|all` — override the calendar's
  `spec.apply.sendUpdates` for this run. Use `none` for bulk drift
  repairs that shouldn't email the team.

## Recover from a crash mid-apply

Apply journals each op into `.cicada/state.yaml` as it lands. If a
run dies partway:

```bash
cicada apply --resume
```

Already-journaled ops are skipped; for `create`/`replace`, cicada
also reconciles by the remote `cicada:id` tag — if the provider write
landed but the journal write crashed, the op is recorded as applied
instead of duplicated.

## Handle out-of-band edits

`cicada drift` reports managed events that changed live (`modified`)
or vanished (`missing`). When apply touches a drifted event it
refuses rather than silently overwriting:

- **Accept the live edit:** `cicada refresh` folds live truth into
  recorded state, then `cicada plan` + `apply` to push your YAML back
  on top — or edit the YAML to match.
- **Override it:** `cicada apply --force` — desired wins; the live
  edit is overwritten (logged per op).

A deleted-remotely event resolves the same way: `refresh` drops the
stale record, the next plan proposes `create`.

## Control notifications

Providers email attendees on writes. `spec.apply.sendUpdates` sets
the policy per calendar — `none`, `externalOnly` (default), or `all`.
Override per run with `apply --notify …`.
