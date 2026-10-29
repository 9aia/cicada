# Error taxonomy

Every port and use-case error classifies into one of four kinds. The
classification is a contract — MCP tools surface the *kind*, not just
the message, so agents can decide retry vs ask-human.

| Kind | Meaning | Retryable |
|---|---|---|
| `transient` | Provider throttling/outage: HTTP 429, 5xx, network failures. May carry a provider `Retry-After` hint. Retries with backoff happen in the adapter; a failed apply is resume-safe. | yes |
| `conflict` | Drift conflict: live state diverged from recorded state for an op the plan touches. Resolve with `refresh` + re-plan, or override with `--force`. | no |
| `auth` | Credential acquisition/refresh failed — surface the token flow to a human (`cicada provider add`, env credentials). | no |
| `fatal` | Validation, capability, or logic errors — bad YAML, unsupported features, malformed state. Fix the input; never retry blindly. | no |

Only `transient` is retryable. A `failed` journal entry records the
kind alongside the message so `--resume` and drift logic can reason
about it.

## On the wire

MCP tools return errors as tool results with the kind attached:

```json
{"error": {"kind": "transient", "message": "…"}}
```

The CLI renders the same classes in per-op apply lines and exits `1`
when any op fails — check the journal for `errorKind` when scripting
retries.
