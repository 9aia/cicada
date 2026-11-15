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
    details: tamias-driver-cicada mcp renders the engine ops manifest as MCP tools — agents read, preview, and reconcile calendars through the same verbs, gated by the planDigest contract.
  - title: Provider-agnostic by construction
    details: "Ports-and-adapters: Google Calendar today (Meet included), more providers behind the same CalendarProvider port. Capabilities are checked at plan time, never silently dropped."
---

## What it is

Cicada is calendars as code: declare calendars in YAML
(`apiVersion: cicada/v1`), then `plan`/`apply`/`drift` against live
providers the way you manage infrastructure. The `cicada` command is
a tamias pack; `tamias-driver-cicada` exposes the same engine as
ops, a `cli` shim, and an MCP surface for agents.

## Why

Calendar APIs aren't transactional and export-only tools can't map
stable ids to remote ids — re-runs duplicate. Cicada keeps three
states (desired YAML, recorded `.cicada/state.yaml`, live provider)
and reconciles them with ordered, idempotent, resumable ops under a
managed boundary that never touches events it didn't create.

## Boundary

- **Owns:** the desired-state lifecycle — declare, plan, apply,
  drift, refresh — and the provider/state/auth ports.
- **Doesn't own:** the calendar providers themselves (Google
  Calendar API), secrets ([cuora](https://9aia.com/cuora/)),
  distribution ([tamias](https://9aia.com/tamias/)), orchestration
  ([peregrin](https://9aia.com/peregrin/)).

## Where to go next

| You want to… | Go to |
| --- | --- |
| Reconcile a calendar end to end | [Tutorial](/tutorials/getting-started) |
| Do a task (Google auth, recurrence, CI, agent use) | [How-to](/how-to/) |
| Understand the design (ports, reconciliation, landscape) | [Explanation](/explanation/) |
| Look up a verb, YAML field, or state shape | [Reference](/reference/) |
