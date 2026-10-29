# MCP tools

`cicada-mcp` (or `cicada mcp`) serves MCP over **stdio**. Each tool
decodes JSON arguments, runs a `cicada-core` use case, and returns
human-readable `content` text plus `structuredContent` JSON. Errors
arrive as tool results (`isError: true`), never protocol errors.

## Server options

| Flag / env | Default | Meaning |
|---|---|---|
| `--root` | `$CICADA_ROOT`, then cwd | Directory `cicada.yaml` is discovered in |
| `--provider` | `fake` | `fake` or `google` |
| `--fake-file` | `.cicada/fake-provider.json` | Fake backend file |
| `--state` | `.cicada/state.yaml`, `$CICADA_STATE` | Recorded-state file |

All tools re-read YAML, state, and the provider per call. `from`/`to`
window bounds accept `today`, `+Nd`, `+Nw`, `YYYY-MM-DD`, civil
datetimes, or RFC 3339 instants, resolved in each calendar's
`timeZone`.

## `cicada.calendar.list` — read-only

Declared calendars with bindings and counts. Takes no arguments.

```json
{"calendars": [{"name": "work", "timeZone": "UTC", "provider": "fake",
                "calendarId": "primary", "declaredEvents": 2,
                "recordedEvents": 2}]}
```

## `cicada.events.range` — read-only

Expand occurrences in `[from, to)` (defaults `today`, `+7d`).

| Arg | Type | Meaning |
|---|---|---|
| `calendar` | string | Limit to one declared calendar (default: all) |
| `from`, `to` | string | Window bounds |
| `live` | boolean | Expand the provider's live events instead of desired YAML |

```json
{"calendars": [{"calendar": "work", "live": false,
                "occurrences": [ … ]}], "totalOccurrences": 5}
```

## `cicada.events.show` — read-only

One event by stable local id (`id`, required), plus occurrences
(defaults `today`, `+30d`).

```json
{"calendar": "work", "event": { … }, "occurrences": [ … ]}
```

## `cicada.state.drift` — read-only

Recorded vs live for every recorded calendar.

```json
{"calendars": [{"calendar": "work", "calendarId": "primary",
                "drifted": 1,
                "drift": [{"kind": "modified", "localId": "standup",
                           "remoteId": "fake-2", …}]}],
 "totalDrifted": 1}
```

## `cicada.state.refresh`

Fold live into recorded under the state lock. Writes only local
state.

```json
{"calendars": [{"calendar": "work", "unchanged": 1, "updated": ["standup"],
                "removed": [], "reconciled": [ … ],
                "adoptionCandidates": []}]}
```

## `cicada.plan.preview` — read-only

Plan every declared calendar; issue a session `planDigest`. Arg:
`notify` (`none`|`externalOnly`|`all`) — must match on apply or the
digest goes stale.

```json
{"planDigest": "sha256:…", "hasErrors": false, "remoteOps": 2,
 "plans": [ /* cicada/plan-v1 documents */ ]}
```

## `cicada.plan.apply` — gated write

Execute a previewed plan under the state lock.

| Arg | Type | Required | Meaning |
|---|---|---|---|
| `planDigest` | string | yes | Digest from `plan.preview` this session |
| `confirm` | boolean | yes | Must be `true` — explicit approval |
| `force` | boolean | no | Overwrite out-of-band edits |
| `resume` | boolean | no | Skip already-journaled ops |
| `notify` | enum | no | Notification override — must match the preview's |

Gates, in order: `confirm: true` → digest issued by `plan.preview` in
this session → server re-plans under the lock and re-derives the
digest; a stale digest refuses. Digests are single-use.

```json
{"calendars": [{"calendar": "work", "applied": 2, "skipped": 0,
                "failed": 0, "outcomes": [ … ]}],
 "digest": "sha256:…"}
```

## Errors

Every error carries its [taxonomy](errors.md) class:

```json
{"error": {"kind": "fatal|conflict|auth|transient", "message": "…"}}
```

## The write contract

There is **no `events.create`** — all mutations flow through declared
YAML + `plan.preview` + `plan.apply` (single write path). This keeps
every remote event managed and drift meaningful.
