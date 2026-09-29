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

Prefer the Mac app? Download the Developer ID signed and Apple notarized DMG from
[Releases](https://github.com/bojieli/greenbubbles/releases), verify it, and drag
**GreenBubbles** to Applications. The release also includes a standalone CLI ZIP.

See [CLI releases and Homebrew](docs/HOMEBREW.md) for download verification,
upgrades, and packaging details.

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

## Getting your database key

<p align="center">
  <img src="assets/key-flow.svg" width="900" alt="The account key is captured into a private file on your Mac. That key and the database file’s salt derive the key used for local read-only access to WeChat’s original database.">
</p>

WeChat stores your chats in encrypted SQLite database files on your Mac.
GreenBubbles needs the account key—the master secret from which each file's
key is derived—to read them. Keys apply to database files, each of which can
contain many tables.

Setup captures and verifies the account key from your own WeChat client, then
stores it in a private local file. It requires administrator access and re-signing
WeChat. Follow the [key acquisition guide](docs/PASSPHRASE_ACQUISITION.md) for
commands, compatibility, credential storage, and troubleshooting.

Next, [create and validate a query profile](docs/QUERY_PROFILES.md) that points
to your WeChat data and credential file. You can then query the original live
databases without exporting them first.

## Use your existing coding agent (recommended)

Use your existing Codex, Claude Code, OpenCode, Kimi Code, Gemini CLI, or Grok Build
session. An available subscription allowance can avoid an additional API bill;
check that the agent is signed in through your intended plan.

The agent uses GreenBubbles through **skills: Markdown instructions with
supporting references and optional helpers**. Reading a skill requires no installation.

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

Your agent reads selected message pages through the CLI and writes memory files
locally. If it uses a cloud model, that message text goes to its provider. Do not
paste database keys or recovery words into prompts.

See [the agent skills guide](docs/AGENT_SKILLS.md) for diagnostics, optional
discovery, and incremental updates.

## Use the embedded summarizer (optional)

GreenBubbles can also call Gemini directly through `ai-summarize-direct` and
save a cited summary as local `memory.md` and `memory.json` files. This route
requires a dedicated **`GEMINI_API_KEY`** and incurs separate API charges; your
coding-agent subscription does not cover it.

You choose the conversations through a policy that explicitly permits remote
model access. The summarizer sends selected message content to Gemini and saves
the result locally. It creates a new output generation each time.

Follow the [embedded summarizer guide](docs/AI_CONTEXT_CLI.md#model-generated-live-memory)
for the policy, API-key setup, and command. For ongoing edits to an existing
memory project, use the agent skills above.

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
| Understand memory formats and incremental updates | [Personal memory](docs/PERSONAL_MEMORY.md) |
| Restrict access through a policy-scoped connector | [AI context CLI](docs/AI_CONTEXT_CLI.md) |
| Back up and recover your history | [Recoverable snapshots](docs/RECOVERABLE_SNAPSHOTS.md) |
| Troubleshoot | [FAQ](docs/FAQ.md) and [known limitations](docs/KNOWN_LIMITATIONS.md) |
| Understand the design | [Architecture](docs/ARCHITECTURE.md) and [threat model](docs/THREAT_MODEL.md) |

The [documentation index](docs/README.md) includes replica operations, integration
contracts, measurements, and contributor references.

## Status and privacy

Your source databases, credential files, backups, and generated memory are
stored locally. Local database queries do not upload them. Cloud AI is an
explicit workflow: an external cloud agent sends the message text it reads to
its provider, and the embedded summarizer sends policy-approved content to
Gemini. GreenBubbles also has an optional public-article fetcher, so the project
as a whole is not network-free. See [privacy](PRIVACY.md) for the boundaries.


GreenBubbles is intended for technical users working with their own data.
WeChat's private formats change; unsupported data is reported as a coverage gap.
Public builds ship with sending disabled. See [known limitations](docs/KNOWN_LIMITATIONS.md).

A policy-scoped connector enforces its configured scope. An agent with general
shell and filesystem access has your local permissions; a skill is guidance,
not a sandbox. Prepared corpora and generated memory can contain private data.
See [privacy](PRIVACY.md) and [the agent skills guide](docs/AGENT_SKILLS.md) before
processing a large history.

## Contributing

Contributions are welcome, especially:

- Improve skills and prompts for accurate summaries, citations, and memory updates.
- Fix CLI bugs and make commands easier for people and agents to use.
- Report key-capture failures and compatibility problems with WeChat updates.
- Add CLI options and features that help agents understand message history.

See [CONTRIBUTING.md](CONTRIBUTING.md) to get started. Use synthetic examples in
reports and tests; leave out real messages, databases, keys, and account paths.
Report security issues through [SECURITY.md](SECURITY.md).

## License

MIT — see [LICENSE](LICENSE). Binary releases include
[third-party notices](THIRD_PARTY_NOTICES.md).

GreenBubbles is an independent project, not affiliated with or endorsed by
Tencent. WeChat and other product names are trademarks of their respective owners.
