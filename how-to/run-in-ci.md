# Run cicada in CI

This guide shows you how to wire cicada into a pipeline: format
checks, validation, machine-readable plans, and drift gates. All
verbs exit `0`/`1`, so shell `&&`/`-e` handling is enough.

## Check formatting and validity

```bash
cicada fmt --check     # exit 1 if any file isn't canonical
cicada validate        # exit 1 on schema or invariant errors
```

`fmt --check` writes nothing — it reports files that would be
reformatted. Run `cicada fmt` locally to fix.

## Produce a machine-readable plan

```bash
cicada plan --provider google --out plan.json
```

`plan.json` is a `cicada/plan-v1` document (a JSON array when several
calendars were planned): the ordered op list with diffs, capability
findings, and a drift summary. It's a stable contract — gate deploys
on it, archive it, or feed it to review tooling. `plan` exits non-zero
when a plan carries error-severity findings, so a job can stop there.

## Gate on drift

```bash
cicada drift --provider google   # exit 1 when any drift exists
```

The non-zero exit is the signal. Typical cron-style job: run `drift`
nightly to alert on out-of-band edits to managed calendars.

## Apply unattended

```bash
cicada apply --provider google --auto-approve --notify none
```

- `--auto-approve` skips the `[y/N]` prompt (required in CI).
- `--notify none` suppresses attendee emails for churn-heavy runs
  (`externalOnly` is the YAML/default policy).
- Exit `1` means at least one op failed — check the per-op lines
  (`✗ create …`) and the journal for error kinds; `transient`
  failures are safe to retry with `--resume`.

## Credentials in CI

Prefer service-account auth — no interactive flow:

```yaml
env:
  CICADA_GOOGLE_SERVICE_ACCOUNT_JSON: ${{ runner.temp }}/sa.json
steps:
  - run: echo "$SA_KEY_JSON" > "$CICADA_GOOGLE_SERVICE_ACCOUNT_JSON"
```

Or store the key once per runner via
`cicada provider add google --service-account key.json`. See
[Connect Google Calendar](connect-google.md) and the
[providers reference](../reference/providers.md).

## Example: validate + drift gate

```yaml
- run: cargo xtask install            # or fetch a prebuilt cicada binary
- run: cicada fmt --check
- run: cicada validate
- run: cicada drift --provider google
  env:
    CICADA_GOOGLE_SERVICE_ACCOUNT_JSON: sa.json
```
