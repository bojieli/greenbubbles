<p align="center">
  <img src="assets/greenbubbles-icon.svg" width="132" alt="GreenBubbles icon">
</p>

<h1 align="center">GreenBubbles</h1>

<p align="center">
  <strong>Your WeChat history, searchable locally and organized by your own AI agent.</strong><br>
  A Mac app and CLI. Local storage; you choose what your AI sees.
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#getting-your-database-key">Get your key</a> ·
  <a href="#use-your-existing-coding-agent-recommended">Use with an agent</a> ·
  <a href="docs/README.md">Docs</a> ·
  <a href="CONTRIBUTING.md">Contribute</a>
</p>

<p align="center">
  <a href="https://github.com/bojieli/greenbubbles/actions/workflows/ci.yml"><img src="https://github.com/bojieli/greenbubbles/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/bojieli/greenbubbles/releases"><img src="https://img.shields.io/github/v/release/bojieli/greenbubbles?include_prereleases" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-22c55e" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/platform-macOS%2014%2B-black" alt="Platform: macOS 14+">
  <img src="https://img.shields.io/badge/status-research%20alpha-f59e0b" alt="Status: research alpha">
</p>

GreenBubbles is a macOS app and command-line toolkit for reading your own WeChat
history directly from WeChat’s original local databases. There is no export or
duplicate chat database to keep in sync for ordinary live queries. Browse and
search locally, create an encrypted backup, or ask your
existing coding agent to turn selected conversations into cited Markdown memory.

**Research alpha · Apple silicon · macOS 14+.** Live encrypted access requires
key capture from your own WeChat client. Local queries upload nothing. If you
use a cloud AI agent, the message pages it reads go to its model provider.

<p align="center">
  <img src="assets/how-it-works.svg" alt="GreenBubbles queries WeChat’s original encrypted databases locally and returns live results to your agent, without maintaining an exported chat database. A separate optional path creates an encrypted backup you can unlock and query later without WeChat." width="820">
</p>

## What you can do

- **Browse and search:** list conversations, page through messages, and retrieve attachments.
- **Build personal memory:** use Codex, Claude Code, OpenCode, Kimi Code, Gemini CLI,
  Grok Build, or another shell-capable agent to maintain cited Markdown or Python files.
- **Update incrementally:** process new messages and retain a reviewable Git history.
  Late-imported or edited older messages need a broader reconciliation pass.
- **Make recoverable backups:** create an independently encrypted snapshot with
  a 24-word recovery kit.

The query commands open WeChat's data read-only. Backups, prepared memory corpora,
and agent-written memory are separate outputs you choose to create.

## Install

macOS 14 or later, Apple silicon.

Download the latest `GreenBubbles-*-macos-arm64.dmg` from
[Releases](https://github.com/bojieli/greenbubbles/releases), verify it, then
drag **GreenBubbles** to Applications:

```console
grep ' GreenBubbles-0.4.0-macos-arm64.dmg$' SHA256SUMS-0.4.0.txt | \
  shasum -a 256 -c -
xcrun stapler validate GreenBubbles-0.4.0-macos-arm64.dmg
```

Every executable is Developer ID signed and Apple notarized. The same release
ships `greenbubbles-*-macos-arm64.zip` with the full command-line tool set.
[v0.4.0](https://github.com/bojieli/greenbubbles/releases/tag/v0.4.0) includes
portable agent skills, the optional skill installer, and the incremental-memory
driver alongside the CLI binaries.

Install the command-line tools through the upstream Homebrew tap:

```sh
brew tap bojieli/greenbubbles https://github.com/bojieli/greenbubbles.git
brew install bojieli/greenbubbles/greenbubbles
```

If Homebrew reports an untrusted formula, explicitly trust this formula and retry
the tap/install commands:

```sh
brew trust --formula bojieli/greenbubbles/greenbubbles
```

Older Homebrew versions without `brew trust` do not need this step.

See [CLI releases and Homebrew](docs/HOMEBREW.md) for publication status,
checksums, upgrades, and maintainer setup.

<details>
<summary><strong>Build from source</strong></summary>

Needs Swift 6, the macOS developer tools, and Rust:

```console
git clone https://github.com/bojieli/greenbubbles.git
cd greenbubbles
cargo build --locked --release --manifest-path Native/GreenBubbles/Cargo.toml
swift build --product greenbubbles-history
swift run greenbubbles-history
```

The CLI lands at `Native/GreenBubbles/target/release/greenbubbles`. Point the
app at it when it asks.
</details>

## Use your existing coding agent (recommended)

If you already have a coding-agent subscription with available usage, start there.
The agent can use GreenBubbles through **skills: Markdown instructions with
supporting references and optional helpers**. Reading a skill requires no installation.

| Route | What you need | Billing |
| --- | --- | --- |
| Your existing agent + skills | GreenBubbles CLI and an agent with shell access | Your agent's subscription allowance or configured API provider |
| Embedded summarizer | GreenBubbles CLI and a dedicated `GEMINI_API_KEY` | Separate API usage, not covered by your coding-agent subscription |
| Scripted memory driver | GreenBubbles CLI and an installed coding-agent CLI | The launched agent's configured billing |

Using an existing subscription allowance can avoid another API bill. Costs still
depend on plan limits, model choice, and workload; check how your agent is signed in.

For a Homebrew installation, find the skill to give your agent:

```sh
echo "$(brew --prefix greenbubbles)/libexec/skills/greenbubbles-personal-memory/SKILL.md"
```

Then ask it:

> Read the GreenBubbles personal-memory SKILL.md at that path. Use my
> GreenBubbles profile to organize the selected conversations into Markdown at
> ~/memory/me. Work in this session, preserve source citations, and update
> existing memory incrementally. Follow the setup skill if access is not ready.

In a source checkout or extracted CLI ZIP, the same file is
`skills/greenbubbles-personal-memory/SKILL.md`. Keep the skill's references and
helpers beside it. For automatic discovery in future sessions, an optional
installer copies the package into your agent's skills directory:

```sh
# Homebrew example; replace codex with claude, opencode, kimi, gemini, or grok.
python3 "$(brew --prefix greenbubbles)/libexec/scripts/install-skills.py" --agent codex
```

See [the agent skills guide](docs/AGENT_SKILLS.md) for setup diagnostics,
project-scoped discovery, updates, and the differences between direct sessions
and scripted runs. Key capture is an owner-operated setup step; do not paste
keys or recovery words into an agent prompt.

## Getting your database key

WeChat stores messages, contacts, and other chat information in SQLite database
files on your Mac, under your account's `db_storage` directory. Those files
contain tables, but they are encrypted with SQLCipher: an ordinary SQLite reader
cannot open them just because it can access the files.

On the supported WeChat clients, a 32-byte **account key** acts as the master
secret. WeChat combines it with each database file's salt to derive that file's
encryption key. An account can have dozens of database files and thousands of
tables; the derived encryption keys are per **file**, not per table.

GreenBubbles needs that account key to authenticate and decrypt database pages
locally as you query them. The acquisition helper captures it during login,
verifies it against the discovered databases, and saves it in an owner-only file
on your Mac. A query profile references that file. Derived keys are calculated
locally as needed; you do not need to store a separate list of table keys.
The saved account key is a secret protected by file permissions, not an encrypted
backup of your chats. Keep it out of prompts and shared folders.

With access configured, each live query reads WeChat's original databases rather
than a previous export. New messages become available when WeChat writes them to
its local storage, without an export-and-sync cycle. Separate queries can observe
different moments while WeChat is changing; this does not promise one atomic view
across every database. Optional snapshots and prepared memory corpora are separate
point-in-time copies and must be refreshed when you want newer data.

<p align="center">
  <img src="assets/key-flow.svg" width="900" alt="The account secret is captured into a private local credential file. The database file’s salt combines with that secret to derive its key locally. GreenBubbles reads the original encrypted file and returns selected messages, not keys, to your agent.">
</p>

Capture requires administrator access and re-signing your installed copy of
WeChat. Read the [acquisition guide](docs/PASSPHRASE_ACQUISITION.md) before starting.

```sh
# Re-sign your own copy, then restart WeChat.
sudo codesign --force --deep --sign - /Applications/WeChat.app

# Check prerequisites, then arm capture before logging out and back in.
sudo greenbubbles-acquire preflight
sudo greenbubbles-acquire capture
```

Re-signing replaces WeChat's original code signature until it is reinstalled or
updated. Capture depends on the client and local permissions; its duration and
coverage are not guaranteed. Follow the helper's verification result and reported
credential-file path. Reuse a verified credential while it continues to authenticate.

Next, [create a query profile](docs/QUERY_PROFILES.md) that names your account's
`db_storage` directory and private credential file. Validate it before querying:

```sh
greenbubbles profile validate <profile-name>
greenbubbles source status --profile <profile-name>
```

## Usage

With a validated default profile:

```sh
# List conversations, then use an ID from the result.
greenbubbles conversations list --limit 20
greenbubbles messages list --conversation <conversation-id> --limit 50

# Enter the search text on stdin, then press Control-D.
greenbubbles messages search --query-stdin

greenbubbles contacts list --limit 50
greenbubbles message get --conversation <conversation-id> --message <message-id>
```

These commands return paginated JSON. Follow continuation cursors to retrieve
more results, and check coverage fields for skipped or unsupported data.
For attachment retrieval and access modes, see the [CLI reference](docs/CLI_REFERENCE.md).

Prefer a window? Install the app, choose **Browse Live or Snapshot…**, and select
your account's `db_storage` directory. To locate account directories with the CLI:

```sh
greenbubbles-discover accounts --include-paths
```

Keep that output private: account paths can contain identifiers.

## Choose your next step

| Goal | Guide |
| --- | --- |
| First-time setup and browsing | [User guide](docs/USER_GUIDE.md) |
| Use your own agent to organize memory | [Portable agent skills](docs/AGENT_SKILLS.md) |
| Schedule extraction or choose a memory format | [Personal memory](docs/PERSONAL_MEMORY.md) |
| Restrict access through a policy-scoped connector | [AI context CLI](docs/AI_CONTEXT_CLI.md) |
| Back up and recover your history | [Recoverable snapshots](docs/RECOVERABLE_SNAPSHOTS.md) |
| Troubleshoot | [FAQ](docs/FAQ.md) and [known limitations](docs/KNOWN_LIMITATIONS.md) |
| Understand the design | [Architecture](docs/ARCHITECTURE.md) and [threat model](docs/THREAT_MODEL.md) |

The [documentation index](docs/README.md) includes replica operations, integration
contracts, measurements, and contributor references.

## Status and privacy

GreenBubbles is intended for technical users working with their own data.
WeChat's private formats change; unsupported data is reported as a coverage gap.
Public builds ship with sending disabled. See [known limitations](docs/KNOWN_LIMITATIONS.md).

A policy-scoped connector enforces its configured scope. An agent with general
shell and filesystem access has your local permissions; a skill is guidance,
not a sandbox. Prepared corpora and generated memory can contain private data.
See [privacy](PRIVACY.md) and [the agent skills guide](docs/AGENT_SKILLS.md) before
processing a large history.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Useful bug reports describe unsupported
message types or failing operations without including real messages, databases,
keys, or account paths. Report security issues through [SECURITY.md](SECURITY.md).
Tests use synthetic data.

## License

MIT — see [LICENSE](LICENSE). Binary releases include
[third-party notices](THIRD_PARTY_NOTICES.md).

GreenBubbles is an independent project, not affiliated with or endorsed by
Tencent. WeChat and other product names are trademarks of their respective owners.
