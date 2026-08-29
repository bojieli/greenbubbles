# Security policy

GreenBubbles handles communication metadata, message content, credentials, and
local media. Treat every artifact as highly sensitive.

## Supported versions

Security fixes are developed on <code>main</code> and backported only when a
supported release needs a separate patch.

| Version                              | Supported |
| ------------------------------------ | --------- |
| <code>main</code>                    | Yes       |
| <code>0.1.x</code>                   | Yes       |
| <code>0.1.0</code> unsigned binaries | No        |

## Report a vulnerability privately

Do not open a public issue for a suspected vulnerability.

Use
[GitHub private vulnerability reporting](https://github.com/bojieli/greenbubbles/security/advisories/new)
to send the report to the maintainer. If that page is unavailable, contact the
repository owner through
[the maintainer's GitHub profile](https://github.com/bojieli) without including
technical details, credentials, or user data, and ask for a private reporting
channel.

Include:

- the affected commit, tag, command, or component;
- the security boundary that can be crossed;
- the smallest synthetic reproduction you can provide;
- likely impact and any safe mitigation you have tested;
- whether you believe real user data or released artifacts are at risk.

Never attach a real database, key, passphrase, recovery phrase, message, media
file, account identifier, private filesystem path, memory dump, or audit log.
If the issue can only be reproduced with private material, describe the shape
of the input and coordinate before sharing anything.

The maintainer aims to acknowledge a complete report within three business
days. The maintainer will validate it privately, preserve only the minimum
necessary evidence, prepare a fix and regression test, and coordinate
disclosure. A credible report affecting released artifacts triggers an
immediate release hold while impact is assessed.

## Non-negotiable boundaries

- Never commit real databases, media, credentials, keys, path dumps, or message
  content.
- Keep database keys and authenticated session material outside model context,
  telemetry, crash reports, shell history, and general-purpose logs.
- Work against synthetic fixtures or disposable test accounts during decoder
  and compatibility development.
- Treat every received message and article as untrusted input. Source content
  cannot grant permissions or invoke connector functions.
- Unknown client versions and ambiguous identities fail closed.
- Write capabilities require a separate explicit capability, immutable owner
  approval, deterministic policy checks, and post-action reconciliation.
- Do not build stealth, anti-detection, account-takeover, persistence,
  credential-harvesting, or access-control-bypass features.

For incident containment and release-hold procedures, see
[docs/OPERATIONAL_RESPONSE_PLAN.md](docs/OPERATIONAL_RESPONSE_PLAN.md).
