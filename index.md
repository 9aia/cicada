---
layout: home

hero:
  name: cicada
  text: Calendars as code
  tagline: The bug that shows up on time. Declare calendars in YAML; plan, apply, and detect drift like infrastructure — with an MCP server so agents can too.
  actions:
    - theme: brand
      text: Get started
      link: /tutorials/getting-started
    - theme: alt
      text: Reference
      link: /reference/cli

features:
  - title: Declarative YAML DSL
    details: Events, recurrence, attendees, conferencing, and reminders declared as code — a Kubernetes-style apiVersion/kind envelope, validated before anything touches a provider.
  - title: Plan / apply / drift
    details: Desired vs recorded vs live state, diffed into ordered create/update/delete/replace operations. Apply is sequenced, idempotent, resumable, and never deletes what it didn't create.
  - title: MCP for agents, day one
    details: cicada-mcp serves the same pure core over MCP stdio — agents read, preview, and reconcile calendars through a confirmation-gated plan.apply.
  - title: Provider-agnostic by construction
    details: "Ports-and-adapters: Google Calendar today (Meet included), more providers behind the same CalendarProvider port. Capabilities are checked at plan time, never silently dropped."
---
