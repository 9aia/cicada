# Keep state safe

This guide shows you how to manage `.cicada/state.yaml` — what to
commit, how locking works, and how to recover a lost file.

## What the state file is

Recorded state maps your stable local ids to the remote ids the
provider assigned, plus the apply journal. It is the **managed
boundary**: cicada only deletes or overwrites what this file records.
Without it, plans are all-`create` and drift detection has nothing to
compare.

Default location: `.cicada/state.yaml` beside the manifest. Override
with `--state <path>` or `CICADA_STATE`.

## Commit it

Commit `state.yaml` alongside your calendar YAML. cicada's scaffolded
`.cicada/.gitignore` already excludes the runtime artifacts:

```txt
fake-provider.json
*.lock
```

- `state.yaml` — **commit.** It is the shared record of what has been
  applied; losing it means re-adopting remote events.
- `fake-provider.json` — never commit; it's the demo backend.
- `*.lock` — lock files; never commit.
- Secrets (`~/.local/state/cicada/secrets/`) — **never** inside the
  repo; OAuth refresh tokens and service-account keys live there at
  mode `0600`.

State files are a real dependency — treat them like Terraform state.
Multi-user teams should have one canonical copy.

## Locking

`apply`, `refresh`, and the MCP `state.refresh`/`plan.apply` tools
take an exclusive advisory `flock` on `state.yaml.lock` for their
whole run. A concurrent `cicada` process or a long-running MCP server
cannot interleave writes — a second process simply waits.

Writes are atomic (temp file + rename), so a crash leaves either the
old or the new document, never a torn one.

## Recover a lost state file

Every event cicada writes carries provider-side tags
(`cicada:id`, `cicada:managed`, `cicada:state` hash). If `state.yaml`
is deleted:

```bash
cicada plan    # warns: live events already carry cicada:id tags
cicada apply   # adopts tagged events back into recorded state —
               # no duplicates
```

The plan lists each `create` with a warning that resume
reconciliation will adopt the already-tagged remote event. Apply
journals them `skippedAlreadyApplied` and rebuilds the recorded map.

`cicada refresh` also reports **adoption candidates** — live events
carrying cicada tags that recorded state doesn't know (e.g. applied
by another machine sharing the calendar). They are reported, never
auto-adopted.
