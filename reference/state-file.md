# State file

Recorded state lives in one YAML document — default
`.cicada/state.yaml` beside the manifest (`--state` flag or
`CICADA_STATE` overrides). Format `cicada/state-v1`.

```yaml
apiVersion: cicada/state-v1
calendars:
  work:                          # logical name (metadata.name)
    provider: fake               # adapter that wrote the records
    calendarId: primary          # remote binding
    events:                      # localId → last reconciled record
      standup:
        remoteId: fake-2
        hash: sha256:8c27a083…   # content hash at apply time
        appliedAt: 2026-10-29T04:53:57.308Z
        etag: etag-2             # last-known remote etag (optional)
journal:                         # append-only per-op apply log
  - key: primary|standup|create|sha256:8c27a083…
    op: create                   # create | update | delete | replace | noop
    localId: standup
    appliedAt: 2026-10-29T04:53:57.308Z
    outcome:
      status: applied            # applied | failed | skippedAlreadyApplied
      remoteId: fake-2           # for applied / skippedAlreadyApplied
```

## Guarantees

- **Versioned** — `apiVersion` is probed before the document is
  interpreted; an unknown version is a `Fatal` error, never silent
  corruption. Migrations are explicit.
- **Atomic** — saves write a sibling temp file and rename over the
  target; a crash never leaves a torn document.
- **Locked** — `apply`/`refresh`/mutating MCP tools take an exclusive
  advisory `flock` on `state.yaml.lock` (a separate file, so atomic
  rename can't break the lock inode) for the whole run.
- **Committable** — intended to be committed with the calendar YAML;
  see [Keep state safe](../how-to/keep-state-safe.md).

## Journal outcomes

| `status` | Meaning |
|---|---|
| `applied` | The remote write landed (`remoteId` recorded) |
| `failed` | Op failed — `error` message plus `errorKind` (`transient`/`conflict`/`auth`/`fatal`); does not corrupt state |
| `skippedAlreadyApplied` | Provider already carried the `cicada:id` tag — adopted, not re-executed |

`apply --resume` skips ops journaled `applied`/`skippedAlreadyApplied`
by idempotency key (`calendarId|localId|opKind|contentHash`).

## Remote tags

Every event cicada writes carries provider metadata — on Google,
`extendedProperties.private` keys `cicada:id` (local id),
`cicada:managed = "1"`, and `cicada:state` (content hash). Tags enable
state rebuild, safe resume, and adoption-candidate reporting.
