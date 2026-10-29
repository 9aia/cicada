# YAML DSL

Every document is a Kubernetes-style envelope:

```yaml
apiVersion: cicada/v1    # the only supported version
kind: Calendar           # Calendar | Manifest
```

Two kinds exist. `deny_unknown_fields` applies everywhere — a
misspelled field is a parse error, with the field path reported
(`spec.events[2].start: …`). One YAML document per file.

## `kind: Manifest`

The root `cicada.yaml` (or `cicada.yml`) listing calendar files:

```yaml
apiVersion: cicada/v1
kind: Manifest
calendars: [calendars/work.yaml, calendars/personal.yaml]
```

`calendars` is a list of paths **relative to the manifest's
directory**. All CLI verbs discover this file when run without
arguments.

## `kind: Calendar`

```yaml
apiVersion: cicada/v1
kind: Calendar
metadata:
  name: work                    # logical name — keys recorded state
  timeZone: America/Sao_Paulo   # IANA zone; wall times resolve here
  provider: google              # optional provider selector
  calendarId: primary           # remote binding (else recorded, else "primary")
spec:
  apply:
    sendUpdates: externalOnly   # none | externalOnly | all (default)
  events:
    - id: standup               # stable local id — required, unique
      summary: Daily Standup    # required
      description: …            # optional
      location: …               # optional
      start: "09:00"            # required — see Time forms
      duration: 30m             # `end` XOR `duration` — one required
      recurrence: { freq: WEEKLY, byDay: [MO,TU,WE,TH,FR] }
      attendees: [{ email: alice@x.com, required: true }]
      reminders: [{ popup: 10m }, { email: 1h }]
      conferencing: meet
      transparency: opaque      # opaque (default) | transparent
```

### `metadata`

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | Logical calendar name; keys `state.yaml`'s `calendars:` map |
| `timeZone` | yes | IANA zone (`America/Sao_Paulo`) for wall-time resolution |
| `provider` | no | Selects the provider adapter for this calendar |
| `calendarId` | no | Concrete remote calendar binding (`primary`, or the provider's calendar id) |

v0.1 binds to **existing** remote calendars — provisioning a remote
calendar is deferred.

### `spec.apply.sendUpdates`

Attendee-notification policy on writes: `none`, `externalOnly`
(default — drift fixes shouldn't spam the team), or `all`.
`--notify` and the `plan.apply` tool parameter override per run.

### `spec.events[]`

| Field | Type | Meaning |
|---|---|---|
| `id` | string | Stable local id. The anchor for update/delete/drift — never reuse ids for different events |
| `summary` | string | Title shown on the calendar |
| `description` | string | Long-form text |
| `location` | string | Where |
| `start` | time | See Time forms |
| `end` / `duration` | time / duration | Exactly one required |
| `recurrence` | rule | See Recurrence |
| `attendees` | list | `{email, required (default true), status}` — `status` (`needsAction`, `accepted`, `declined`, `tentative`) is provider-owned; excluded from content hashes |
| `reminders` | list | `{popup: <dur>}` or `{email: <dur>}` — exactly one key per entry |
| `conferencing` | string | Intent: `meet`, `teams`, or a provider-named kind. Resolved at plan time against provider capabilities |
| `transparency` | string | `opaque` (busy, default) or `transparent` (free) |

### Time forms

`start`/`end` accept:

- `"HH:MM"` — bare wall clock. Recurring events only (a one-off needs
  a date); requires a *structured* recurrence rule. Anchors to the
  earliest date the rule generates on/after 1970-01-01.
- `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM[:SS]` — civil time; attaches
  `metadata.timeZone`.
- RFC 3339 instant with offset (`2026-08-19T09:00:00-03:00`) —
  absolute time. Recurring events may not use this form (wall time is
  required so DST keeps "09:00" at 09:00).

A bare-clock `end` inherits the start's date, rolling a day forward
when earlier (`23:00`→`01:00` is an overnight event).

### Durations

`s`, `m`, `h`, `d` (24h), `w` (7d). Groups chain: `1h30m` or
`"1h 30m"`. A unit is always required — bare `30` errors. Used by
`duration` and reminder offsets.

### Recurrence {#recurrence}

Three accepted forms, all normalised to one rule:

```yaml
recurrence: { freq: WEEKLY, byDay: [MO,TU,WE,TH,FR], count: 52 }  # structured
recurrence: { rrule: "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR" }         # raw RRULE
recurrence: "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"                    # bare RRULE
```

Structured fields (camelCase):

| Field | Values |
|---|---|
| `freq` | `DAILY`, `WEEKLY`, `MONTHLY`, `YEARLY` expandable; `HOURLY`, `MINUTELY`, `SECONDLY` parse but do not expand |
| `interval` | ≥ 1 (default 1) |
| `count` | ≥ 1 — counts from the anchor |
| `until` | inclusive bound: date, civil datetime, or instant |
| `byDay` | `MO`…`SU`, optionally ordinal `1MO`, `-1FR` (ordinals need `MONTHLY`/`YEARLY`) |
| `byMonthDay` | `1`–`31`, or `-1`–`-31` from month end |
| `byMonth` | `1`–`12` |

Desired and remote rules compare **semantically** (normalised), not
as strings. Raw RRULEs outside the structured model (`BYSETPOS`,
`BYWEEKNO`, `BYHOUR`, …) compare fine but cannot be expanded locally
— a documented limitation surfaced as a `Fatal` error on expansion.

### Domain invariants (validation layer 2)

- `end` must be after `start` (as instants).
- Duplicate `id`s within a calendar are rejected.
- Recurring events require a wall-time `start`.
- `interval ≥ 1`, `count ≥ 1`, `byMonthDay ∈ -31..=-1 ∪ 1..=31`,
  `byMonth ∈ 1..=12`, ordinal `byDay` only under `MONTHLY`/`YEARLY`.

### Deferred

`templates/`, `groups.yml`, and `$ref` composability are not yet
implemented — the envelope leaves room for them.
