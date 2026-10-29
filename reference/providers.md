# Providers

`--provider` selects the adapter: `fake` (default) or `google`.
Both sit behind the same `CalendarProvider` port; the planner checks
each provider's declared capabilities at plan time and emits warnings
or errors for unsupported desired states.

## `fake`

A file-backed fake for tests, demos, and development — **test/demo
only**. Serialises its whole backend to `.cicada/fake-provider.json`
after every mutation, so separate `cicada` invocations share one
"provider" and hand-edits simulate out-of-band changes. Reports
permissive capabilities (everything supported).

## `google`

Google Calendar API v3, with Google Meet as its conferencing kind —
`conferencing: meet` is an intent the planner resolves, not a URL.

### Capabilities

| Capability | Value |
|---|---|
| `maxAttendees` | 200 |
| `maxReminders` | 5 |
| `reminderKinds` | `popup`, `email` |
| Recurrence frequencies | all seven (`DAILY`…`SECONDLY`) |
| Ordinal `byDay`, `byMonthDay`, `byMonth`, raw RRULE | yes |
| `conferencingKinds` | `meet` |
| `can_suppress_notifications` | yes (`sendUpdates`) |
| `supports_free_busy` | yes |
| `supports_tagging` | yes (`extendedProperties.private`) |
| `supports_conditional_writes` | yes (etag → `If-Match`) |

Retried with exponential backoff at the HTTP layer; 429/5xx surface
as `Transient` errors.

### Auth

Credentials resolve in this order (first match wins):

| Priority | Source | Variables |
|---|---|---|
| 1 | Service-account key file path | `CICADA_GOOGLE_SERVICE_ACCOUNT_JSON` or `GOOGLE_APPLICATION_CREDENTIALS` |
| 2 | Static access token (no refresh; ~1h expiry) | `CICADA_GOOGLE_ACCESS_TOKEN` |
| 3 | OAuth installed-app client + refresh token | `CICADA_GOOGLE_CLIENT_ID` + `CICADA_GOOGLE_CLIENT_SECRET`; refresh from `CICADA_GOOGLE_REFRESH_TOKEN` or the stored token |
| 4 | Persisted service-account key | `google/service-account` in the secrets store (written by `cicada provider add google --service-account`) |

Optional:

| Variable | Meaning |
|---|---|
| `CICADA_GOOGLE_SUBJECT` | Domain-wide delegation `sub` for service-account JWT |
| `CICADA_GOOGLE_TOKEN_URL` | Token endpoint override (tests) |
| `CICADA_SECRETS_DIR` | Secrets directory override |

### `cicada provider add google`

- **OAuth** (no flag): needs a client id/secret (flags or env).
  Prints the consent URL (`calendar` scope, offline access); a
  loopback listener captures the code, or the code/redirect URL is
  pasted. The refresh token is stored at
  `google/refresh-token` in the secrets store.
- **`--service-account key.json`**: validates and persists the key at
  `google/service-account` — the priority-4 fallback above. Share the
  target calendar with the account's `client_email`.

Secrets live at `$CICADA_SECRETS_DIR`, else
`$XDG_STATE_HOME/cicada/secrets`, else `~/.local/state/cicada/secrets`
— directory `0700`, files `0600`, key names URL-encoded
(`google%2Frefresh-token`).

### Binding

`metadata.calendarId` in YAML wins, then the recorded binding in
state, then `primary`. cicada reconciles existing remote calendars —
it does not provision them.

## Future providers

Teams, CalDAV and others arrive via the same `CalendarProvider` port
(list/get/create/update/delete, `findByTag`, `freeBusy`,
capabilities) — adapters, not core changes.
