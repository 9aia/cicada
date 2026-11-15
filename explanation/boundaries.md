# Boundaries

Cicada owns the **desired-state lifecycle** — declare, plan, apply,
drift — and the ports everything crosses. Neighbors own theirs:

- **Providers** (Google Calendar API, future Teams/CalDAV) — remote
  truth; cicada reads/writes through `CalendarProvider`, never
  talks HTTP itself.
- **Secrets** — `token_env`/credential names only; values resolve
  through env or [cuora](https://9aia.com/cuora/), never config.
- **Distribution** — the `cicada` command is a tamias `bins[]`
  pack; `tamias-driver-cicada` is also the `type: cicada` content
  driver. [tamias](https://9aia.com/tamias/) owns fetch/install.
- **Orchestration** — campaigns that schedule reconciles are
  [peregrin](https://9aia.com/peregrin/)'s; cicada exposes bounded
  ops they can call.
