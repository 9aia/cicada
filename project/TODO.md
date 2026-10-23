# TODO

Build order per [ADR-0001 §10](adrs/0001-foundational-architecture.md); semantics per [ADR-0002](adrs/0002-reconciliation-contracts.md):

1. [x] `cicada-core` — domain types + the 7 port traits (pure)
2. [x] `cicada-schema` — YAML → domain (`cicada validate`), + canonical emit (`cicada fmt`)
3. [x] planner + `PlanCalendar` (`cicada plan` vs fake provider)
4. [x] `cicada-state` + apply/refresh/drift lifecycle
5. [x] `cicada-mcp` — stdio server, read + `plan.preview`/`plan.apply` (gated) tools
6. [x] `cicada-google` — auth + calendar + meet; HTTP-fixture tested. Live Google E2E is credential-gated (needs `cicada provider add google` / env creds)
7. [x] CLI/MCP polish — `init`, `fmt`, `provider add google`, `show`/`range --provider` live views, `plan.apply` gating
8. [x] Golden plan tests (README example → `plan-v1` golden) + MCP `tools/list` golden + HTTP fixture tests

Deferred beyond v0.1 (ADR backlog): calendar provisioning, `cicada adopt`, templating/`$ref`/`groups.yml`, remote state backends, live Google E2E run.
