# Changelog

## [0.2.0] - 2026-11-24

### Added

- **cli:** init, fmt, provider add google, live range/show (`fefeb41`)
- **planning:** OperationPlanner, plan/range/show use cases, fakes, CLI verbs (`ed60549`)
- **apply:** CLI apply/refresh/drift verbs + file-backed fake provider (`e91eb63`)
- **cicada-driver:** cli verb is an engine passthrough — the tamias bins[] shim contract (`e021208`)
- **core:** domain + ports — pure IR, 7 port traits, recurrence expander (`cf67349`)
- **mcp:** cicada-mcp stdio server — gated plan.apply + cicada mcp (`b9f8ded`)
- **google:** Google Calendar API v3 provider adapter — CalendarProvider + AuthProvider (`68309e5`)
- CLI-as-generated-surface — cicada engine lib + tamias-driver-cicada ops/pavo surface (P-021) (`2fb2e0f`)
- **schema:** YAML DSL parser, ValidateCalendar use case, cicada validate (`24efa2f`)
- calendars as code — cargo workspace scaffold, ADR-0001/0002 (`21064da`)
- move planDigest gate engine-side, delete cicada-mcp (T-019) (`171d0fd`)
- tamias-driver-cicada — type: cicada content driver + example bundle (`0ec23bf`)
