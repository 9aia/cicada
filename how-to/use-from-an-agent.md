# Use cicada from an agent

This guide shows you how to expose cicada to an MCP-capable agent so
it can read, plan, and reconcile calendars through the same engine
the CLI drives — with every mutation gated by the planDigest
contract.

## The server

There is no `cicada-mcp` binary (deleted in T-019): the MCP surface
is `tamias-driver-cicada mcp`, installed by `cargo xtask install`
alongside the driver. It speaks MCP over **stdio** and renders the
engine ops manifest as tools — one tool per bounded verb
(`plan`, `apply`, `drift`, `range`, …), params are the verb's own
flags.

```json
{
  "mcpServers": {
    "cicada": {
      "command": "tamias-driver-cicada",
      "args": ["mcp"]
    }
  }
}
```

Ops inherit the server process's **cwd** — `cicada.yaml` manifest
discovery, `.cicada/` state, and `CICADA_*` env apply exactly as
for the `cicada` binary. Launch the server with the project root as
its working directory (most MCP clients support a `cwd` field), or
pass calendar file paths via each tool's `files` argument.

Per-call provider selection rides the `provider` param
(`fake`/`google`); `fake_file`, `state`, and the `CICADA_*` env
vars work the same as on the CLI. `provider.add` is deliberately
**not** a tool — the OAuth flow needs stdin/browser, so it stays an
interactive surface (`cicada provider add google`, see
[Connect Google Calendar](connect-google.md)).

## The tool surface

Reads are always safe — `list`, `range`, `show`, `drift`, `validate`
only read. Mutating verbs are `init` (scaffolds files), `plan`
(can write `--out`), `apply`, `refresh`, `fmt`. There is **no
`events.create`**: desired state lives in YAML — to add an event
the agent edits the calendar file, plans, and applies. See the
[MCP tools reference](../reference/mcp-tools.md).

## The write contract

`apply` is gated — mutations are never implicit:

1. `plan` prints a `planDigest` (`sha256:…`), persisted beside the
   state file — issued digests survive process boundaries.
2. `apply --plan-digest <digest>` must name an issued, unspent
   digest; the engine re-plans and refuses if YAML, live, or
   recorded state drifted since the preview. Digests are
   single-use.
3. Without `--plan-digest`, `apply` falls back to the interactive
   confirmation prompt — under MCP (null stdin) that fails closed
   and aborts, so agents must go through the digest path.
   `--auto-approve` exists for CI but is the caller's explicit
   choice, not a default.

Typical agent flow:

```
1. list                              → see declared calendars
2. range {from, to}                  → find a gap
3. (edit calendars/*.yaml)           → declare the event
4. plan                              → read planDigest + the diff,
                                        show the user the plan
5. apply {planDigest, autoApprove?}  → user approved
```

Errors carry a class — `transient` (retry), `conflict` (refresh +
re-plan), `auth` (surface the token flow), `fatal` (fix the input) —
so the agent knows whether to retry or ask a human.
