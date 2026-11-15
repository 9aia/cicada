# Command-Line Help for `cicada`

This document contains the help content for the `cicada` command-line program.

calendars as code — declare calendars in YAML, reconcile like infrastructure

**Command Overview:**

* [`cicada`↴](#cicada)
* [`cicada init`↴](#cicada-init)
* [`cicada validate`↴](#cicada-validate)
* [`cicada list`↴](#cicada-list)
* [`cicada plan`↴](#cicada-plan)
* [`cicada apply`↴](#cicada-apply)
* [`cicada refresh`↴](#cicada-refresh)
* [`cicada drift`↴](#cicada-drift)
* [`cicada range`↴](#cicada-range)
* [`cicada show`↴](#cicada-show)
* [`cicada fmt`↴](#cicada-fmt)
* [`cicada provider`↴](#cicada-provider)
* [`cicada provider add`↴](#cicada-provider-add)

## `cicada`

calendars as code — declare calendars in YAML, reconcile like infrastructure

**Usage:** `cicada [COMMAND]`

###### **Commands:**

* `init` — Scaffold a cicada.yaml manifest and a starter calendar
* `validate` — Validate calendar YAML: schema shape plus domain invariants
* `list` — List the declared calendars: bindings plus declared/recorded event counts
* `plan` — Diff desired vs recorded vs live into an ordered plan — and issue the `planDigest` `apply --plan-digest` requires
* `apply` — Reconcile: execute the plan against the provider
* `refresh` — Pull live state into recorded state
* `drift` — Show recorded-vs-live drift
* `range` — Expand events over a time range (local; --provider for live)
* `show` — Show one event by stable local id
* `fmt` — Normalize calendar YAML formatting in place
* `provider` — Manage provider registrations (`provider add google`)

###### **Options:**

* `-h, --help` — Print help
* `-V, --version` — Print version

## `cicada init`

Scaffold a cicada.yaml manifest and a starter calendar

**Usage:** `cicada init [OPTIONS] [DIR]`

###### **Arguments:**

* `[DIR]` — Directory to scaffold into (default: current directory)

###### **Options:**

* `--force` — Overwrite existing `cicada.yaml` / `calendars/work.yaml`
* `-h, --help` — Print help

## `cicada validate`

Validate calendar YAML: schema shape plus domain invariants. With no files, discovers the `cicada.yaml` manifest in the current directory and validates every calendar it lists. A `kind: Manifest` file passed explicitly expands the same way.

**Usage:** `cicada validate [FILES]...`

###### **Arguments:**

* `[FILES]...` — Calendar or manifest YAML files. Defaults to the `cicada.yaml` manifest in the current directory

###### **Options:**

* `-h, --help` — Print help (see a summary with '-h')

## `cicada list`

List the declared calendars: bindings plus declared/recorded event counts

**Usage:** `cicada list [OPTIONS] [FILES]...`

###### **Arguments:**

* `[FILES]...` — Calendar or manifest YAML files. Defaults to the `cicada.yaml` manifest in the current directory

###### **Options:**

* `--provider <PROVIDER>` — Provider name used for calendars without `provider:` (fake | google; default fake)
* `--state <STATE>` — Recorded-state file (default `.cicada/state.yaml`; also `CICADA_STATE`)
* `-h, --help` — Print help

## `cicada plan`

Diff desired vs recorded vs live into an ordered plan — and issue the `planDigest` `apply --plan-digest` requires

**Usage:** `cicada plan [OPTIONS] [FILES]...`

###### **Arguments:**

* `[FILES]...` — Calendar or manifest YAML files. Defaults to the `cicada.yaml` manifest in the current directory

###### **Options:**

* `--provider <PROVIDER>` — Provider adapter to plan against (fake | google)
* `--fake-file <FAKE_FILE>` — Fake provider backend file (test/demo only; default `.cicada/fake-provider.json` beside the manifest)
* `--state <STATE>` — Recorded-state file (default `.cicada/state.yaml` beside the manifest; `CICADA_STATE` also overrides)
* `--out <OUT>` — Write the plan JSON here (`cicada/plan-v1`); a JSON array when more than one calendar is planned
* `--notify <NOTIFY>` — Attendee notifications: none | externalOnly | all — must match the choice `apply` uses or the plan digest goes stale
* `-h, --help` — Print help

## `cicada apply`

Reconcile: execute the plan against the provider. `--plan-digest <sha256:…>` applies only the exact plan a `cicada plan` run previewed — the digest is consumed on use and counts as the confirmation (no prompt).

**Usage:** `cicada apply [OPTIONS] [FILES]...`

###### **Arguments:**

* `[FILES]...` — Calendar or manifest YAML files. Defaults to the `cicada.yaml` manifest in the current directory

###### **Options:**

* `--provider <PROVIDER>` — Provider adapter to apply against (fake | google)
* `--fake-file <FAKE_FILE>` — Fake provider backend file (test/demo only; default `.cicada/fake-provider.json` beside the manifest)
* `--state <STATE>` — Recorded-state file (default `.cicada/state.yaml`; also `CICADA_STATE`)
* `--auto-approve` — Skip the confirmation prompt
* `--resume` — Resume after a crash — skip already-journaled ops
* `--force` — Overwrite out-of-band edits instead of failing on drift conflict (ADR-0002 §4)
* `--notify <NOTIFY>` — Attendee notifications: none | externalOnly | all
* `--plan-digest <DIGEST>` — Apply only the plan this digest previewed (printed by `cicada plan` as `planDigest sha256:…`). The digest must have been issued and not spent, and the re-planned digest must match — anything changed since preview refuses as stale. Providing it is the confirmation: the prompt is skipped
* `-h, --help` — Print help (see a summary with '-h')

## `cicada refresh`

Pull live state into recorded state

**Usage:** `cicada refresh [OPTIONS]`

###### **Options:**

* `--provider <PROVIDER>` — Provider adapter to refresh from (fake | google)
* `--fake-file <FAKE_FILE>` — Fake provider backend file (test/demo only)
* `--state <STATE>` — Recorded-state file (default `.cicada/state.yaml`; also `CICADA_STATE`)
* `-h, --help` — Print help

## `cicada drift`

Show recorded-vs-live drift

**Usage:** `cicada drift [OPTIONS]`

###### **Options:**

* `--provider <PROVIDER>` — Provider adapter to check against (fake | google)
* `--fake-file <FAKE_FILE>` — Fake provider backend file (test/demo only)
* `--state <STATE>` — Recorded-state file (default `.cicada/state.yaml`; also `CICADA_STATE`)
* `-h, --help` — Print help

## `cicada range`

Expand events over a time range (local; --provider for live)

**Usage:** `cicada range [OPTIONS] [FILES]...`

###### **Arguments:**

* `[FILES]...` — Calendar or manifest YAML files. Defaults to the `cicada.yaml` manifest in the current directory

###### **Options:**

* `--calendar <CALENDAR>` — Limit to one declared calendar name; default all
* `--from <FROM>` — Range start (`today`, `+Nd`, a date, or a datetime)
* `--to <TO>` — Range end (`+7d`, a date, or a datetime)
* `--provider <PROVIDER>` — Expand the live provider's events instead of local YAML (fake | google)
* `-h, --help` — Print help

## `cicada show`

Show one event by stable local id

**Usage:** `cicada show [OPTIONS] <ID> [FILES]...`

###### **Arguments:**

* `<ID>` — Stable local id of the event
* `[FILES]...` — Calendar or manifest YAML files. Defaults to the `cicada.yaml` manifest in the current directory

###### **Options:**

* `--from <FROM>` — Occurrence window start (`today`, `+Nd`, a date, or a datetime). With `--to`, defaults to `today`
* `--to <TO>` — Occurrence window end (defaults to the window start + 30d)
* `--provider <PROVIDER>` — Show the live provider's version of the event (found via its `cicada:id` tag) instead of the declared YAML
* `-h, --help` — Print help

## `cicada fmt`

Normalize calendar YAML formatting in place

**Usage:** `cicada fmt [OPTIONS] [FILES]...`

###### **Arguments:**

* `[FILES]...` — Files to format; defaults to the manifest's calendars

###### **Options:**

* `--check` — Check only — exit nonzero when a file isn't canonical; writes nothing (CI-friendly)
* `--sort` — Sort events by stable id instead of preserving file order
* `-h, --help` — Print help

## `cicada provider`

Manage provider registrations (`provider add google`)

**Usage:** `cicada provider <COMMAND>`

###### **Commands:**

* `add` — Register a provider (OAuth flow / service account)

###### **Options:**

* `-h, --help` — Print help

## `cicada provider add`

Register a provider (OAuth flow / service account)

**Usage:** `cicada provider add [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>` — Provider name (`google`)

###### **Options:**

* `--service-account <KEY_JSON>` — Register a service-account key JSON instead of running the OAuth flow (non-interactive)
* `--client-id <CLIENT_ID>` — OAuth installed-app client id (else `CICADA_GOOGLE_CLIENT_ID`)
* `--client-secret <CLIENT_SECRET>` — OAuth installed-app client secret (else `CICADA_GOOGLE_CLIENT_SECRET`)
* `--code <CODE>` — Authorization code — skips the interactive prompt
* `-h, --help` — Print help
