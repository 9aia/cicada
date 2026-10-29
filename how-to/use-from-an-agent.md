# Use cicada from an agent

This guide shows you how to expose cicada to an MCP-capable agent so
it can read, plan, and reconcile calendars through the same core the
CLI drives — with every mutation confirmation-gated.

## The server

Two equivalent entry points — same options, same stdio transport:

```bash
cicada-mcp --root /path/to/project --provider fake
CICADA_ROOT=/path/to/project cicada mcp --provider google
```

| Flag | Default | Purpose |
|---|---|---|
| `--root` | `$CICADA_ROOT`, then cwd | Directory `cicada.yaml` is discovered in |
| `--provider` | `fake` | `fake` (demo) or `google` (live API + env credentials) |
| `--fake-file` | `.cicada/fake-provider.json` | Fake backend file |
| `--state` | `.cicada/state.yaml`, `$CICADA_STATE` | Recorded-state file |

One server process = one working root. Every tool call re-reads the
YAML, the state file, and the provider, so a long-running server
never serves stale data.

## Client configuration

Register it like any stdio MCP server — for example:

```json
{
  "mcpServers": {
    "cicada": {
      "command": "cicada-mcp",
      "args": ["--root", "/home/you/calendars", "--provider", "google"]
    }
  }
}
```

`cicada-mcp` is installed by `cargo xtask install` alongside `cicada`.

## The tool surface

Seven tools (`cicada.*`). Reads are always safe:

- `cicada.calendar.list` — declared calendars + bindings + counts
- `cicada.events.range` — expand occurrences in a window
  (`live: true` for the provider's view)
- `cicada.events.show` — one event by stable local id
- `cicada.state.drift` — recorded vs live
- `cicada.state.refresh` — fold live into recorded (local state write,
  locked)
- `cicada.plan.preview` — dry-run diff, returns a `planDigest`
- `cicada.plan.apply` — execute a previewed plan (gated, locked)

There is **no `events.create`**. Desired state lives in YAML — to add
an event the agent edits the calendar file, previews, and applies.
See the [MCP tools reference](../reference/mcp-tools.md) for
argument and result shapes.

## The write contract

`cicada.plan.apply` is gated — mutations are never implicit:

1. `confirm: true` required — the agent should show the preview to the
   user and ask first.
2. `planDigest` must come from `cicada.plan.preview` *in this session*.
3. The server re-plans under the state lock and re-derives the digest;
   any change to YAML, live, or recorded state since preview makes the
   digest stale and apply refuses. Digests are single-use.

Typical agent flow:

```
1. cicada.calendar.list            → see declared calendars
2. cicada.events.range {from,to}   → find a gap
3. (edit calendars/*.yaml)         → declare the event
4. cicada.plan.preview             → show the user the plan
5. cicada.plan.apply {confirm: true, planDigest} → user approved
```

Errors carry a class — `transient` (retry), `conflict` (refresh +
re-plan), `auth` (surface the token flow), `fatal` (fix the input) —
so the agent knows whether to retry or ask a human.
