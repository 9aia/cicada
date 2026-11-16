# Security

- Tokens and refresh tokens live behind the `SecretStore` port — never
  commit credentials or state files containing tokens.
- `plan.apply` mutates live calendars — it is confirmation-gated in CLI
  and MCP surfaces.
- Report vulnerabilities privately by emailing
  luisfuturist+9aia@gmail.com with "SECURITY" in the subject — do not
  open public issues or disclose details publicly; contact us first.
