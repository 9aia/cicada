# MCP tools

`tamias-driver-cicada mcp` serves MCP over **stdio** — newline-
delimited JSON-RPC. The surface is generated: the driver's ops
manifest (every bounded `cicada` verb as a `cli.<verb>` wire query)
is rendered as MCP tools by pavo-driver-kit, so **tool names are
the verb names** and `tools/call` arguments are the verb's params
verbatim. Errors arrive as tool results (`isError: true`), never
protocol errors.

There is no `cicada-mcp` binary and no `cicada mcp` verb — both were
deleted in T-019 when the preview→apply gate moved engine-side
(`planDigest`, see below).

## Server setup

```json
{
  "mcpServers": {
    "cicada": {
      "command": "tamias-driver-cicada",
      "args": ["mcp"],
      "cwd": "/path/to/project"
    }
  }
}
```

- **cwd** — ops inherit the server process's working directory:
  `cicada.yaml` discovery, `.cicada/` state, and `CICADA_*` env
  resolve exactly as for the `cicada` binary. Point `cwd` at the
  project root, or pass calendar file paths per call.
- **Per-call params** — `provider` (`fake`|`google`), `fake_file`,
  `state`, and each verb's own flags map to the same-named tool
  arguments. `provider.add` is a declared *surface*, never a tool —
  OAuth needs stdin/browser.

Every tool call re-reads the YAML, the state file, and the provider
— a long-running server never serves stale data. Window bounds
(`from`/`to` on `range`) accept `today`, `+Nd`, `+Nw`,
`YYYY-MM-DD`, civil datetimes, or RFC 3339 instants, resolved in
each calendar's `timeZone`.

## The tools

Reads are always safe:

| Tool | Params of note | Returns |
|---|---|---|
| `list` | `files` | declared calendars + bindings + counts |
| `range` | `from`, `to`, `live` | expanded occurrences in a window |
| `show` | event id | one event's detail + occurrences |
| `drift` | — | recorded vs live mismatches (exit nonzero on drift) |
| `validate` | `files` | schema + domain-invariant check |

Mutating (all confirmation-gated, see below):

| Tool | Params of note | Effect |
|---|---|---|
| `init` | — | scaffold `cicada.yaml` + starter calendar |
| `plan` | `notify`, `out` | diff desired/recorded/live → ops + `planDigest` |
| `apply` | `plan_digest`, `auto_approve`, `resume`, `force`, `notify` | execute a plan |
| `refresh` | — | fold live into recorded state |
| `fmt` | `files` | rewrite calendar YAML canonically |

## The write contract

There is **no `events.create`** — all mutations flow through
declared YAML → `plan` → `apply` (single write path). This keeps
every remote event managed and drift meaningful.

`apply` is gated — mutations are never implicit:

1. `plan` prints a `planDigest` (`sha256:…`), persisted beside the
   state file — issued digests survive process boundaries.
2. `apply` with `plan_digest` must name an issued, unspent digest;
   the engine re-plans and refuses if YAML, live, or recorded
   state changed since the preview. Digests are single-use.
3. `apply` without `plan_digest` prompts on stdin — under MCP
   (null stdin) that fails closed and aborts, so agents must use
   the digest path. `auto_approve` skips both gates and is the
   caller's explicit choice (CI), never a default.

Typical agent flow:

```
list                 → see declared calendars
range {from, to}     → find a gap
(edit calendars/*.yaml) → declare the event
plan                 → read planDigest + the diff, show the user
apply {plan_digest}  → user approved
```

## Errors

Every error carries its [taxonomy](errors.md) class — `transient`
(retry), `conflict` (refresh + re-plan), `auth` (surface the token
flow), `fatal` (fix the input) — so the agent knows whether to
retry or ask a human.
