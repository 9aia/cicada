# CLI

`cicada` — calendars as code. Declare calendars in YAML; plan, apply,
and detect drift against live providers.

**Global behaviour.** Verbs that read desired state take optional file
arguments; with none, the `cicada.yaml` (or `cicada.yml`) manifest in
the current directory is discovered and its `calendars:` list used. A
`kind: Manifest` path passed explicitly expands the same way. All
verbs exit `0` on success and `1` on failure.

**Shared flags.** Provider-facing verbs accept:

| Flag | Default | Meaning |
|---|---|---|
| `--provider <fake\|google>` | `fake` | Provider adapter |
| `--fake-file <path>` | `.cicada/fake-provider.json` beside the manifest | Fake backend file (test/demo) |
| `--state <path>` | `.cicada/state.yaml`; `$CICADA_STATE` overrides | Recorded-state file |

## `cicada init [dir]`

Scaffold `cicada.yaml`, `calendars/work.yaml`, and `.cicada/` in `dir`
(default: current directory). Refuses to overwrite existing files.

| Flag | Meaning |
|---|---|
| `--force` | Overwrite existing scaffolded files |

## `cicada validate [files…]`

Schema shape plus domain invariants per file; manifests expand.
Reports `✓`/`✗` per file with diagnostics, then `N valid, M failed`.
Exit `1` when any file errors.

## `cicada plan [files…]`

Diff desired vs recorded vs live into an ordered plan: `create`,
`update`, `delete`, `replace`, `noop` lines with field-level diffs,
drift entries, and capability findings. Exit `1` on errors or when
any plan carries error-severity findings.

| Flag | Meaning |
|---|---|
| `--out <path>` | Write the `cicada/plan-v1` JSON document (array when several calendars planned) |

## `cicada apply [files…]`

Plan then execute under the state-file advisory lock. Prints the plan,
prompts `[y/N]` unless `--auto-approve`, then applies ops in order,
journalling each into recorded state. Exit `1` when any op fails —
partial applies are honest.

| Flag | Meaning |
|---|---|
| `--auto-approve` | Skip the confirmation prompt |
| `--resume` | Skip ops already journaled as applied (crash recovery) |
| `--force` | Overwrite out-of-band edits instead of failing on drift conflict |
| `--notify <none\|externalOnly\|all>` | Attendee-notification override for this run |

## `cicada refresh`

Fold live provider state into recorded state under the lock: update
changed records, drop records whose live event is gone, report
cicada-tagged live events as adoption candidates. Writes only local
state — never the provider.

## `cicada drift`

Recorded-vs-live comparison. Read-only. Prints `missing`/`modified`
entries per calendar; exit `1` when any drift exists (the CI signal).

## `cicada range [files…]`

Expand declared events into occurrences over a window. Default window:
`today` → `+7d`. With `--provider`, lists the live remote calendar
instead (unmanaged events included).

| Flag | Meaning |
|---|---|
| `--from <bound>` | Window start |
| `--to <bound>` | Window end (exclusive) |

Bounds: `today`, `+Nd`, `+Nw`, `YYYY-MM-DD`, a civil datetime, or an
RFC 3339 instant — resolved in the calendar's `timeZone`.

## `cicada show <id> [files…]`

One event by stable local id: fields plus occurrences (default window
`today` → `+30d`, capped at 20 lines). With `--provider`, resolves the
live event by its `cicada:id` tag and expands its remote rule.

| Flag | Meaning |
|---|---|
| `--from`, `--to` | Occurrence window (same grammar as `range`) |

## `cicada fmt [files…]`

Canonical re-serialisation in place: stable field order, normalised
scalars, defaults elided. Comments are not preserved.

| Flag | Meaning |
|---|---|
| `--check` | Write nothing; exit `1` when any file isn't canonical (CI) |
| `--sort` | Sort events by stable id instead of preserving file order |

## `cicada provider add google`

Register Google credentials.

| Flag | Meaning |
|---|---|
| `--service-account <key.json>` | Non-interactive: validate + store a service-account key |
| `--client-id`, `--client-secret` | OAuth client credentials (else `CICADA_GOOGLE_CLIENT_ID`/`_SECRET`) |
| `--code` | Authorization code — skips the interactive prompt |

OAuth flow: prints a consent URL; a loopback listener on `127.0.0.1`
captures the code when it can bind, otherwise the code or redirect URL
is pasted. The refresh token lands at
`~/.local/state/cicada/secrets/` mode `0600`.

## `cicada mcp`

Serve MCP over stdio — same options as the `cicada-mcp` binary:
`--root`, `--provider`, `--fake-file`, `--state`. See
[MCP tools](mcp-tools.md).

## Environment variables

| Variable | Used by |
|---|---|
| `CICADA_STATE` | State file path (all provider-facing verbs) |
| `CICADA_ROOT` | MCP server working root |
| `CICADA_GOOGLE_*`, `GOOGLE_APPLICATION_CREDENTIALS` | Google auth — see [providers](providers.md) |
| `CICADA_SECRETS_DIR` | Secrets directory override |
