# Manage recurring events

This guide shows you how to declare, inspect, and change recurring
events. The full field grammar is in the
[DSL reference](../reference/yaml-dsl.md#recurrence).

## Declare a rule

Prefer the structured form:

```yaml
- id: standup
  summary: Daily Standup
  start: "09:00"
  duration: 30m
  recurrence: { freq: WEEKLY, byDay: [MO, TU, WE, TH, FR] }
```

For rules outside the structured model (`BYSETPOS`, `BYWEEKNO`, …),
use the raw RFC 5545 escape hatch:

```yaml
recurrence: { rrule: "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1" }
# or bare:  recurrence: "FREQ=MONTHLY;…"
```

cicada compares rules **semantically** — `{byDay: [FR, MO]}` equals
`RRULE:FREQ=WEEKLY;BYDAY=MO,FR` — so reformulating a rule never
produces a phantom diff.

## Start times and DST

Recurring events are declared in **wall time** plus
`metadata.timeZone`, never as bare instants. `"09:00"` stays 09:00
local across DST shifts — the classic calendar bug is designed out.
A recurring `start` must be wall-time: `"HH:MM"` or a civil
`YYYY-MM-DD[THH:MM]`; an RFC 3339 instant on a recurring event is a
validation error.

A bare `"HH:MM"` start is a shorthand for *the earliest date the rule
generates* — handy for open-ended weekly patterns. Write an explicit
date when `count`/`until` matter, because they count from the anchor:
`count: 10` on a `"09:00"`/`MO,WE,FR` rule means ten occurrences
*from the anchor*, not from today.

## Inspect occurrences

```bash
cicada range --from today --to +30d          # all declared events
cicada show standup --from today --to +90d   # one event's occurrences
```

## Change a rule

Editing `recurrence` produces a **`replace`** op — delete + recreate —
because the instance set is a different event series. Field edits
that keep the rule (summary, attendees, reminders) stay `update`s.

```bash
cicada plan    # shows: replace  standup remote <id> → "…"
cicada apply
```

## Limits

- Structured rules cover `freq` (`DAILY`–`YEARLY`), `interval`,
  `count`, `until`, `byDay` (with ordinals like `-1FR` under
  `MONTHLY`/`YEARLY`), `byMonthDay`, `byMonth`.
- `HOURLY`/`MINUTELY`/`SECONDLY` parse but **don't expand** — `range`
  and `show` error on them (documented limitation; absolute-time
  semantics don't fit the wall-time model).
- Raw RRULEs using properties outside the structured model compare
  semantically but can't expand locally — use them only where the
  provider expands natively (Google honors the full FREQ set).
- Ordinal `byDay` requires `MONTHLY` or `YEARLY`; validation rejects
  `-1FR` under `WEEKLY`.
