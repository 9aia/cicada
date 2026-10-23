# TODO

Build order per [ADR-0001 §10](adrs/0001-foundational-architecture.md); semantics per [ADR-0002](adrs/0002-reconciliation-contracts.md):

1. [ ] `cicada-core` — domain types + the 7 port traits (pure)
2. [ ] `cicada-schema` — YAML → domain (`cicada validate`)
3. [ ] planner + `PlanCalendar` (`cicada plan` vs fake provider)
4. [ ] `cicada-state` + apply/refresh/drift lifecycle
5. [ ] `cicada-mcp` — stdio server, read + `plan.preview` tools
6. [ ] `cicada-google` — auth + calendar + meet (real E2E)
7. [ ] CLI/MCP polish — show, range, fmt, `provider add`, `plan.apply` gating
8. [ ] Golden plan tests + HTTP fixture tests
