# Connect Google Calendar

This guide shows you how to authenticate the `google` provider so
`--provider google` works on `plan`, `apply`, `refresh`, `drift`,
`range`, `show`, and `cicada mcp`. Two flows exist — pick one.

::: warning Live writes are credential-gated
Everything in the [tutorial](../tutorials/getting-started.md) works
against `fake` with no credentials. These steps touch your real
Google account; cicada needs the `calendar` scope (read/write events
and free/busy).
:::

## Option A — OAuth installed app (your own calendar)

Best for a calendar you own. You need an OAuth **Desktop app** client
from Google Cloud:

1. Google Cloud console → enable the **Google Calendar API**.
2. Credentials → create an **OAuth client ID**, type *Desktop app*.
   Note the client id and secret.

Then run:

```bash
export CICADA_GOOGLE_CLIENT_ID=…      # or pass --client-id
export CICADA_GOOGLE_CLIENT_SECRET=…  # or pass --client-secret
cicada provider add google
```

cicada prints a consent URL. Open it, approve, and either the local
loopback listener captures the redirect automatically, or you paste
the authorization code (or the whole redirect URL) at the prompt. In
headless shells, pre-paste with `--code`.

The exchanged **refresh token** is stored at
`~/.local/state/cicada/secrets/google%2Frefresh-token`, mode `0600` —
never commit it. From then on, env credentials are optional;
`--provider google` just works.

## Option B — service account (bots, agents, CI)

Best for a dedicated or shared calendar:

```bash
cicada provider add google --service-account key.json
```

The key JSON (a `type: service_account` file from the Cloud console)
is validated and stored at the secrets location
(`google/service-account`). Then:

- **Share the target calendar** with the service account's
  `client_email` (Calendar settings → *Share with specific people*),
  or point cicada at a calendar the account owns. cicada never
  provisions calendars.
- For domain-wide delegation, export `CICADA_GOOGLE_SUBJECT=<user>`.

## Option C — environment variables only

Skip `provider add` entirely — `GoogleAuthConfig::from_env` resolves
credentials in this order:

| Priority | Variables | Mode |
|---|---|---|
| 1 | `CICADA_GOOGLE_SERVICE_ACCOUNT_JSON` or `GOOGLE_APPLICATION_CREDENTIALS` → key file path | Service account |
| 2 | `CICADA_GOOGLE_ACCESS_TOKEN` | Static token (expires ~1h, no refresh) |
| 3 | `CICADA_GOOGLE_CLIENT_ID` + `CICADA_GOOGLE_CLIENT_SECRET` (+ `CICADA_GOOGLE_REFRESH_TOKEN`, else the stored token) | OAuth installed app |
| 4 | the persisted `google/service-account` key | Service account |

`CICADA_GOOGLE_TOKEN_URL` overrides the token endpoint (tests).
See the [providers reference](../reference/providers.md) for the full
list.

## Bind the calendar in YAML

Set the provider and remote calendar on the logical calendar:

```yaml
metadata:
  name: work
  timeZone: America/Sao_Paulo
  provider: google
  calendarId: primary        # your calendar's id — "primary", or the
                             # calendar id from its settings page
```

`calendarId` is required for Google — cicada reconciles *existing*
remote calendars; calendar provisioning is not yet implemented. For a
dedicated agent calendar, use its full calendar id
(`…@group.calendar.google.com`).

## Reconcile

```bash
cicada plan --provider google
cicada apply --provider google
cicada drift --provider google
```

The lifecycle is identical to the fake provider — same plan/apply/
drift semantics, plus Google's etag conditional writes and
`sendUpdates` notification control. If an op conflicts with an
out-of-band edit, see [Reconcile a calendar](reconcile-a-calendar.md).
