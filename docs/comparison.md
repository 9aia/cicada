# Competitive Comparison

A survey of existing calendar tooling, and where cicada fits.

## Summary

Nobody has built cicada's full combination: a declarative YAML DSL plus a
plan/apply/drift lifecycle with stable IDs and recorded state, on a DDD core
reusable across CLI and MCP. Existing work covers slices of that space but
not the integration.

| | Declarative DSL | Plan/Apply | Stable IDs + State | Drift | Multi-provider | Reusable core |
|---|:---:|:---:|:---:|:---:|:---:|:---:|
| **cicada** | YAML | yes | yes | yes | via port | yes (DDD) |
| Terraform providers | HCL | yes | yes | yes | no | no (Terraform-coupled) |
| YAML → ICS tools | YAML/text | no | no | no | n/a (file output) | no |
| Unified SDKs | no (imperative) | no | no | no | yes | partial (library) |
| Calendar MCP servers | no (agent tools) | no | no | no | yes | no (transport-entangled) |

## Category 1: Terraform providers for Google Calendar

**Projects:** [sethvargo/terraform-provider-googlecalendar](https://github.com/sethvargo/terraform-provider-googlecalendar), [chewnoill/terraform-provider-google-calendar](https://github.com/chewnoill/terraform-provider-google-calendar)

Closest in spirit to cicada — they apply the IaC plan/apply workflow to
calendars and use stable resource IDs with recorded state.

**Why not enough:**
- HCL, not YAML — tied to Terraform's configuration language.
- Google-only.
- Core is coupled to Terraform's plugin RPC model; it cannot be reused outside
  Terraform (no CLI or MCP reuse).
- Go, not Rust.
- Effectively dormant.

**What cicada borrows:** the plan/apply/drift-with-stable-IDs lifecycle. What
it rejects: coupling the core to a host tool's plugin model.

## Category 2: YAML → ICS export tools

**Projects:** [yaml2ical](https://pypi.org/project/yaml2ical/), [scientific-python/yaml2ics](https://github.com/scientific-python/yaml2ics), [eikopf/gnomon](https://github.com/eikopf/gnomon), [JettChenT/timeblok](https://github.com/jettchent/timeblok)

Declare meetings in YAML or a custom plaintext DSL, compile to `.ics` files for
import into any calendar app. gnomon is particularly elegant — custom language
with imports, merge, and recurrence.

**Why not enough:**
- **Export-only.** They generate files; they don't sync with a live provider.
- No plan/apply, no recorded state, no drift detection.
- **Cannot update or delete.** Without stable IDs mapped to remote IDs,
  re-running creates duplicates. This is the fundamental failure mode.
- No multi-provider abstraction — output is a generic ICS file.

**What cicada borrows:** the ergonomics of declaring meetings as code (gnomon's
recurrence syntax and import/merge ideas inform our structured recurrence and
`$ref` composability). What it rejects: the export-only model entirely.

## Category 3: Unified calendar SDKs

**Projects:** [nandan-varma/calendar-sdk](https://github.com/nandan-varma/calendar-sdk), [recal.dev](https://recal.dev/), [OneCal](https://github.com/OneCal/unified-calendar-api-node-sdk)

TypeScript SDKs providing one API across Google Calendar, Microsoft Graph, and
CalDAV. calendar-sdk in particular has a clean monorepo split (`core` +
`providers/*` + `mock`) with a `StorageAdapter` port — architecturally the
closest to cicada's core.

**Why not enough:**
- **Imperative libraries**, not declarative. You call `sdk.events.create(...)`;
  you don't declare desired state and reconcile.
- No plan/apply/drift lifecycle.
- No DSL.
- Their API is oriented around CRUD, not desired-state reconciliation — building
  cicada's planner on top would mean fighting their model.

**What cicada borrows:** the `core` + `providers/*` + `mock` package split
validates our layout. What it rejects: the imperative API as the whole story.

## Category 4: Calendar MCP servers

**Projects:** [MarimerLLC/calendar-mcp](https://github.com/MarimerLLC/calendar-mcp), [rauf543/calendar-mcp](https://github.com/rauf543/calendar-mcp), [hasna/calendar](https://github.com/hasna/calendar), [lucasheight/mcp-calendars](https://github.com/lucasheight/mcp-calendars), [madbonez/caldav-mcp](https://github.com/madbonez/caldav-mcp)

MCP servers that expose calendars as tools for AI agents — list events, create
events, find free time, across Google/Microsoft/CalDAV.

**Why not enough:**
- **Agent tooling, not declarative-as-code.** There's no YAML desired state.
- Provider logic is entangled with the MCP transport — no reusable core.
- No plan/apply/drift lifecycle.
- No stable IDs or recorded state.

**What cicada borrows:** the intent to serve MCP as an interface — but as a thin
adapter over the same pure core that powers the CLI, not as the home of business
logic. This is exactly the "abstract the interface too" requirement: cicada's
DDD core makes CLI and MCP interchangeable.

## The gap cicada fills

| Capability | Terraform providers | YAML→ICS | Unified SDKs | MCP servers | **cicada** |
|---|:---:|:---:|:---:|:---:|:---:|
| Declarative desired state | HCL | YAML/text | — | — | **YAML** |
| Plan / Apply | yes | no | no | no | **yes** |
| Drift detection | yes | no | no | no | **yes** |
| Stable IDs + recorded state | yes | no | no | no | **yes** |
| Update / Delete (not just create) | yes | no | yes (imperative) | yes (imperative) | **yes (declarative)** |
| Multi-provider | no | n/a | yes | yes | **yes (via port)** |
| Provider capabilities model | no | n/a | implicit | no | **yes (first-class)** |
| Reusable core (CLI/MCP) | no | no | partial | no | **yes (DDD)** |
| Pluggable state backend | Terraform backends | n/a | via adapter | no | **yes (port)** |

No single primitive in cicada is novel. The value is the **integration**: a
declarative YAML DSL plus a plan/apply/drift lifecycle with stable IDs and
recorded state, on a DDD core reusable across interfaces. That combination is
the white space.
