# Security policy

GreenBubbles handles communication metadata and may eventually handle message
content. Treat every artifact as sensitive.

## Non-negotiable boundaries

- Never commit real databases, media, credentials, keys, path dumps, or message
  content.
- Keep database keys and authenticated session material outside AI model
  context, telemetry, crash reports, and general-purpose logs.
- Work against synthetic fixtures or disposable test accounts during decoder
  development.
- Treat every received message and article as untrusted input. Model output
  cannot grant permissions or invoke arbitrary connector functions.
- Unknown client versions fail closed. Write capabilities require a separate,
  explicit capability grant and deterministic policy checks.
- Do not build stealth, anti-detection, account-takeover, or access-control
  bypass features.

## Reporting

Because this repository is private, report suspected vulnerabilities directly
to the repository owner. Do not attach real user artifacts to a report. Provide
a minimal synthetic reproducer whenever possible.
