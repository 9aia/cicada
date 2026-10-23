# cicada-mcp

MCP server over **stdio** exposing cicada's plan/apply/drift
lifecycle to agents (ADR-0001 §8a). Thin adapter — every tool decodes
args, calls a `cicada-core` use case, and renders the result as text
**plus** `structuredContent` JSON. No business logic lives here.

Spawn it per session:

```bash
cicada-mcp --root <dir>                  # root holding cicada.yaml
cicada-mcp --provider fake --fake-file /tmp/fake.json
CICADA_ROOT=<dir> cicada mcp             # same server via the CLI
```

Working root resolution: `--root` > `CICADA_ROOT` > cwd. The manifest
(`cicada.yaml`/`cicada.yml`) is discovered in the root; `.cicada/`
beside it holds `state.yaml` and the default `fake-provider.json`.

## Tool surface

| Tool | Args | Result (`structuredContent`) | Mutates |
| --- | --- | --- | --- |
| `cicada.calendar.list` | — | `{calendars: [{name, timeZone, provider, calendarId, declaredEvents, recordedEvents}]}` | — |
| `cicada.events.range` | `calendar?`, `from?`, `to?`, `live?` | `{calendars: [{calendar, live, occurrences: […]}], totalOccurrences}` | — |
| `cicada.events.show` | `id`, `from?`, `to?` | `{calendar, event, occurrences}` | — |
| `cicada.state.drift` | — | `{calendars: [{calendar, calendarId, drifted, drift}], totalDrifted}` | — |
| `cicada.state.refresh` | — | `{calendars: [{calendar, unchanged, updated, removed, reconciled, adoptionCandidates}]}` | local state (locked) |
| `cicada.plan.preview` | `notify?` | `{planDigest, hasErrors, remoteOps, plans: [cicada/plan-v1]}` | issues digest |
| `cicada.plan.apply` | `planDigest`, `confirm`, `force?`, `resume?`, `notify?` | `{calendars: [{calendar, applied, skipped, failed, outcomes}], digest}` | remote + state (locked) |

`from`/`to` accept `today`, `+Nd`, `+Nw`, ISO dates, civil datetimes,
or RFC 3339 instants, resolved in each calendar's `timeZone`.

Errors come back as tool results (`isError: true`), never protocol
errors, with the ADR-0002 §8 class attached:

```json
{"error": {"kind": "fatal|conflict|auth|transient", "message": "…"}}
```

## The write contract

- **No `events.create`** (ADR-0002 §10). Desired state lives in YAML;
  to change events the agent edits the calendar files, calls
  `cicada.plan.preview`, shows the user the plan, then applies.
- **`cicada.plan.apply` is gated** (ADR-0001 §8a), in this order:
  1. `confirm: true` required — mutating ops are never implicit.
  2. `planDigest` must have been issued by `cicada.plan.preview` in
     this session — proves a preview happened.
  3. The server re-plans under the state lock and re-derives the
     digest; any change to desired/live/recorded state since preview
     makes the digest **stale** and the apply refuses.
  4. Digests are single-use — a second apply must re-preview.
- `state.refresh`/`plan.apply` run under the state file's advisory
  lock (ADR-0002 §7) so a concurrent `cicada` CLI can't interleave.
- Every tool call re-reads YAML, state, and the provider file — a
  long-running server never serves stale input or hides drift.

## Providers

`--provider fake` (default) uses the file-backed `FakeFileProvider`
— the full lifecycle is testable/demoable without Google.
`--provider google` fails with a clear not-implemented error until
the adapter lands.
