# tamias-bundle — a `type: cicada` package example

A minimal tamias bundle whose payload is a cicada project:
`cicada.yaml` (`kind: Manifest`) + `calendars/*.yaml`. Installing it
through tamias dispatches lifecycle verbs to
[`tamias-driver-cicada`](../../crates/cicada-driver) — the content
driver this repo ships (`cargo build -p cicada-driver`, binary
`tamias-driver-cicada`).

## Layout

```
tamias.yaml        # package manifest: name, version, type: cicada
cicada.yaml        # cicada manifest (kind: Manifest)
calendars/work.yaml  # one calendar, two events
driver.yaml        # example --config for the driver (provider: fake)
```

## Try it end to end (fake provider — no credentials)

```sh
# 1. Build the driver and put it where tamias finds it.
cargo build -p cicada-driver
export PATH="$PWD/target/debug:$PATH"   # tamias-driver-cicada on PATH

# 2. Install the bundle — runs the driver's `install` verb
#    (payload validation; nothing is applied).
tamias install ./examples/tamias-bundle

# 3. Deploy — plan-only. Writes driver.json + plan.json under the
#    config's stateDir; never applies.
tamias deploy cicada-demo --config ./examples/tamias-bundle/driver.yaml

# 4. Inspect.
tamias driver exec cicada-demo status --config ./examples/tamias-bundle/driver.yaml
tamias driver exec cicada-demo uninstall --config ./examples/tamias-bundle/driver.yaml
cat examples/tamias-bundle/cicada-state/plan.json
```

`deploy` with `provider: fake` plans against
`cicada-state/fake-provider.json` (created on first *apply*, not by
plan). To exercise the real path, edit `driver.yaml`: `provider:
google`, drop `fakeFile`, and configure Google credentials the way
`cicada plan --provider google` does (`CICADA_GOOGLE_*` env /
`cicada provider add google`).

Applying the plan is deliberately **not** a driver verb — run
`cicada apply` in a checkout of the payload (or `tamias driver exec`
has no apply either; the confirmation gate is cicada's, ADR-0002).

## Driver verbs

| verb | what it does |
| ---- | ------------ |
| `install` | validate `cicada.yaml` + every calendar (schema + domain); JSON report |
| `deploy` | write `driver.json`, run plan → `plan.json` under `stateDir`; plan-only |
| `uninstall` | drift/managed-inventory JSON; deletes nothing remote |
| `status` | recorded-state summary; no provider calls |
| `config-schema` | JSON Schema (draft 2020-12) of `driver.yaml`'s format |
| `capabilities` | `{capabilities, verbs, types: ["cicada"]}` |
