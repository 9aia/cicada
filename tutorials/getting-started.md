# Getting started

This tutorial takes you from an empty directory to a fully reconciled
calendar — declare events in YAML, plan the diff, apply it, then watch
cicada detect an out-of-band edit as drift.

Everything below runs against the bundled **fake provider**: a
file-backed stand-in for a real calendar service. No Google account,
no credentials, no network.

You will need Rust (`cargo`) installed.

## Install cicada

Clone the repository and run the install task:

```bash
git clone https://github.com/9aia/cicada.git
cd cicada
cargo xtask install
```

This release-builds two binaries into `~/.local/bin`:

- `cicada` — the CLI
- `cicada-mcp` — the MCP server for agents (you will not need it today)

Check the install:

```bash
cicada --help
```

## Scaffold a project

Make a fresh directory and scaffold it:

```bash
mkdir ~/calendars && cd ~/calendars
cicada init
```

cicada creates three things:

```
cicada.yaml          — the manifest (lists your calendar files)
calendars/work.yaml  — a starter calendar
.cicada/             — runtime dir; state.yaml lands here on apply
```

Open `calendars/work.yaml`. It declares one logical calendar, `work`,
with two events — a recurring standup and a one-off offsite:

```yaml
apiVersion: cicada/v1
kind: Calendar
metadata:
  name: work
  timeZone: UTC
  provider: fake        # the bundled demo provider
  calendarId: primary
spec:
  events:
  - id: standup
    summary: Daily Standup
    start: "09:00"      # wall clock — recurs at 09:00 local, DST-proof
    duration: 30m
    recurrence:
      freq: WEEKLY
      byDay: [MO, TU, WE, TH, FR]
    attendees:
    - email: alice@example.com
    reminders:
    - popup: 10m
    conferencing: meet
  - id: offsite
    summary: Team Offsite
    start: 2026-03-20T10:00
    duration: 4h
```

## Validate the YAML

```bash
cicada validate
```

```
info: discovered manifest cicada.yaml
✓ calendars/work.yaml — work — 2 event(s)
1 valid, 0 failed
```

Validate checks the schema shape plus domain invariants (end after
start, sane recurrence, no duplicate ids) — before anything touches a
provider.

## Plan

```bash
cicada plan
```

```
calendar work (provider: fake · calendar: primary) — generated …
  create  offsite "Team Offsite" — 2026-03-20T10:00:00 [UTC]
  create  standup "Daily Standup" — 1970-01-01T09:00:00 [UTC]
plan: 2 create(2 remote ops)
```

Plan diffs three states — **desired** (your YAML), **recorded** (the
state file, empty so far) and **live** (what the provider has, nothing
yet) — into ordered operations. Both events are new, so the plan is
two `create`s. Nothing has been written.

(Don't worry about the 1970 date on `standup` — a bare `"09:00"`
start anchors to the earliest date its recurrence rule generates.
`range`, below, shows the real occurrences.)

## Apply

```bash
cicada apply --auto-approve
```

`--auto-approve` skips the confirmation prompt; without it cicada
shows the plan and asks before writing.

```
  ✓ create  offsite → remote fake-1
  ✓ create  standup → remote fake-2
apply work: 2 applied · 0 skipped · 0 failed
```

Two things just appeared in `.cicada/`:

- `state.yaml` — recorded state: your stable ids (`standup`) mapped to
  the remote ids the provider assigned (`fake-2`), plus a per-op
  journal. This is what makes re-runs converge instead of duplicating.
- `fake-provider.json` — the fake provider's database. On Google,
  this would be your actual calendar.

Run `cicada plan` again — it now reports `noop`s, because desired,
recorded, and live all agree.

## See the events

```bash
cicada range --from today --to +7d
```

```
work:
  2026-10-29 09:00 [UTC] → 09:30 · standup — Daily Standup
  2026-10-30 09:00 [UTC] → 09:30 · standup — Daily Standup
  …
```

`range` expands recurrence into concrete occurrences — five weekdays,
as declared. `cicada show standup` gives one event's full detail.

## Detect drift

Now simulate someone editing the calendar behind cicada's back — the
equivalent of a colleague dragging the meeting in the Google Calendar
UI. With the fake provider, that means editing its database file.

Open `.cicada/fake-provider.json`, find the event whose `remoteId` is
`fake-2`, and change its `summary` to `"Daily Standup (moved!)"`.
Save, then:

```bash
cicada drift
```

```
work: 1 drifted
  ! standup (remote fake-2): modified — live differs from recorded
```

`drift` compares recorded state against live and exits non-zero — this
is how CI notices out-of-band edits. Note that cicada only tracks
events *it* applied; a brand-new event created in the provider UI is
invisible to drift, and untouchable by apply.

## Reconcile

Two ways to resolve drift. First, `refresh` — pull live truth into
recorded state:

```bash
cicada refresh
```

```
refreshed work: 1 updated · 0 removed · 1 unchanged
  ~ standup — record now matches live
```

Now recorded matches live, so `cicada drift` is clean. But your YAML
still says `"Daily Standup"` — plan again and cicada proposes the
repair:

```bash
cicada plan
```

```
  update  standup remote fake-2 → "Daily Standup"
          ~ summary: Daily Standup (moved!) → Daily Standup
```

Apply it and the declared name is pushed back to the provider. The
loop is complete: **desired → plan → apply → drift → refresh →
re-plan**.

## What's next

- [Connect Google Calendar](../how-to/connect-google.md) — the same
  lifecycle against a real provider.
- [Reconcile a calendar](../how-to/reconcile-a-calendar.md) — force,
  resume, and notifications.
- [Use cicada from an agent](../how-to/use-from-an-agent.md) — the MCP
  interface to everything you just did by hand.
- [Why plan/apply/drift?](../explanation/plan-apply-drift.md) — the
  reconciliation model underneath.
