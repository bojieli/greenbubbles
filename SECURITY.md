# Security policy

GreenBubbles handles message content, communication metadata, credentials and
local media. Treat every artefact it touches as highly sensitive — including
the ones that look like metadata.

## Supported versions

Fixes are developed on `main` and backported only when a supported release
needs its own patch.

| Version | Supported |
| --- | --- |
| `main` | yes |
| `0.1.x` | yes |
| `0.1.0` unsigned binaries | no — withdrawn, use 0.1.1 or later |

## Reporting a vulnerability

**Do not open a public issue.**

Use [GitHub private vulnerability
reporting](https://github.com/bojieli/greenbubbles/security/advisories/new). If
that page is unavailable, contact the maintainer through
[their GitHub profile](https://github.com/bojieli) — without technical details,
credentials or user data — and ask for a private channel.

Include the affected commit, tag, command or component; the security boundary
that can be crossed; the smallest synthetic reproduction you can manage; likely
impact and any mitigation you have tested; and whether you believe real user
data or released artefacts are at risk.

**Never attach** a real database, key, passphrase, recovery phrase, message,
media file, account identifier, private filesystem path, memory dump or audit
log. If the issue only reproduces with private material, describe the *shape*
of the input and coordinate before sharing anything.

Expect acknowledgement of a complete report within three business days. The
report is validated privately, only the minimum necessary evidence is
preserved, a fix and regression test are prepared, and disclosure is
coordinated with you. A credible report affecting released artefacts triggers
an immediate release hold while impact is assessed.

## Boundaries that are not negotiable

These hold for maintainers and contributors alike:

- Never commit real databases, media, credentials, keys, path dumps or message
  content.
- Keep database keys and session material out of model context, telemetry,
  crash reports, shell history and general-purpose logs.
- Develop decoders and compatibility against synthetic fixtures or disposable
  test accounts.
- Treat every received message and fetched article as untrusted input. Source
  content can never grant a permission or invoke a connector function.
- Unknown client versions and ambiguous identities fail closed.
- A write capability requires a separate explicit capability, immutable owner
  approval, deterministic policy checks and post-action reconciliation.
- Do not build stealth, anti-detection, account-takeover, persistence,
  credential-harvesting or access-control-bypass features.

## Related documents

- [Threat model](docs/THREAT_MODEL.md) — assets, trust boundaries, what is
  defended and what explicitly is not.
- [Operational response plan](docs/OPERATIONAL_RESPONSE_PLAN.md) — incident
  containment and release holds.
- [Action safety contract](docs/ACTION_SAFETY_CONTRACT.md) — the rules any
  outward-visible action must satisfy.
- [Privacy](PRIVACY.md) — what this software collects, which is nothing.
