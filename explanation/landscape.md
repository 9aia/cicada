# The landscape

Existing calendar tooling covers slices of the space cicada occupies
— nobody has shipped the full combination. Four camps:

| | Declarative DSL | Plan/Apply | Stable IDs + state | Drift | Multi-provider | Reusable core |
|---|:-:|:-:|:-:|:-:|:-:|:-:|
| **cicada** | YAML | yes | yes | yes | via port | yes (DDD) |
| Terraform providers | HCL | yes | yes | yes | no | no |
| YAML → ICS tools | YAML/text | no | no | no | n/a | no |
| Unified SDKs | imperative | no | no | no | yes | partial |
| Calendar MCP servers | agent tools | no | no | no | yes | no |

## Terraform providers for Google Calendar

*sethvargo/terraform-provider-googlecalendar*,
*chewnoill/terraform-provider-google-calendar* — closest in spirit:
they apply IaC plan/apply to calendars with stable resource ids and
recorded state. But they're HCL-coupled, Google-only, their core
can't be reused outside Terraform's plugin model, and they're
effectively dormant. cicada borrows the lifecycle and rejects the
host coupling.

## YAML → ICS export tools

*yaml2ical*, *scientific-python/yaml2ics*, *gnomon*, *timeblok* —
declare meetings in YAML or a custom DSL, compile to `.ics`. Elegant
ergonomics (gnomon's recurrence and import/merge ideas inform
cicada's structured form), but **export-only**: no sync with a live
provider, no plan/apply, no recorded state, no drift. Without stable
ids mapped to remote ids, re-running creates duplicates — the
fundamental failure mode.

## Unified calendar SDKs

*calendar-sdk*, *recal*, *OneCal* — one imperative API across Google,
Microsoft Graph, CalDAV. Their `core` + `providers/*` + `mock` split
validates cicada's layout, but the API is CRUD-oriented: you call
`sdk.events.create(...)` rather than declaring desired state and
reconciling. No DSL, no plan/apply/drift lifecycle.

## Calendar MCP servers

*calendar-mcp*, *calendar-mcp* (rauf543), *hasna/calendar*,
*mcp-calendars*, *caldav-mcp* — expose calendars as agent tools, but
with provider logic entangled in the transport, no declarative
desired state, and no reconciliation lifecycle. cicada serves MCP as
a thin adapter over the same pure core the CLI drives — the interface
is abstracted too, not just the provider.

## The gap

No single primitive in cicada is novel. The value is the
**integration**: a declarative YAML DSL, a plan/apply/drift lifecycle
with stable ids and recorded state, and a DDD core reusable across
CLI and MCP — provider-agnostic by construction through first-class
capabilities. That combination is the white space.
