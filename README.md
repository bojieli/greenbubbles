<p align="center">
  <img src="assets/greenbubbles-icon.svg" width="132" alt="GreenBubbles icon">
</p>

<h1 align="center">GreenBubbles</h1>

<p align="center">
  <strong>Read your own WeChat history from the command line, and give an AI only the parts you choose.</strong><br>
  A Mac app and CLI. Everything stays on your machine.
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#getting-your-database-key">Get your key</a> ·
  <a href="#usage">Usage</a> ·
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

I have 1,855,548 WeChat messages on this Mac, and my AI tools cannot read one
of them. They sit as 2.98 GB of SQLCipher databases including 6,292 message
tables, with about 59 GB of images, voice notes and documents beside them.
WeChat can open all of it. It does not offer that context to other tools; the
matching database key lives inside the running client rather than in an export
you can request.

The export tools that exist solve the wrong half of this. They decrypt the
database and write the whole corpus to JSON — which is exactly the artifact you
should least want sitting on disk, and exactly the wrong thing to hand a model.
A full dump is not context. It is a liability with a search box.

GreenBubbles takes the opposite approach: leave everything where it is, and open
it read-only.

<p align="center">
  <img src="assets/how-it-works.svg" alt="WeChat's encrypted databases sit on your Mac. GreenBubbles, a command-line tool and Mac app, reads them read-only and passes on only the chats you pick to your AI tool. It can also write an encrypted backup that opens without WeChat." width="820">
</p>

## Features

- **Read your history from the shell.** List chats, page through messages,
  search, fetch one message, pull out one photo or file.
- **Browse it in a Mac app.** Native, read-only, works on live data or a backup.
- **Give an AI a slice, not the lot.** Pick the chats, the fields and the dates
  it may see. Everything else never leaves your machine.
- **Back it up so it outlives WeChat.** Encrypted with its own key and 24
  recovery words, so a backup still opens if you lose the app or the account.
- **Turn your history into a living knowledge project.** Extract into a
  git-versioned Python or Markdown project with executable constraints that
  proactively alert you to cross-domain conflicts — passport expiry vs. upcoming
  trip, allergy vs. new prescription, conflicting instructions across sessions.
- **Turn a million messages into a wiki.** Coding agents you already pay for
  read your history in parallel and write cited Markdown, one message at a
  time, never a summary of a summary.
- **Never writes.** WeChat's own files are opened read-only, always.

## Install

macOS 14 or later, Apple silicon.

Download the latest `GreenBubbles-*-macos-arm64.dmg` from
[Releases](https://github.com/bojieli/greenbubbles/releases), verify it, then
drag **GreenBubbles** to Applications:

```console
grep ' GreenBubbles-0.2.0-macos-arm64.dmg$' SHA256SUMS-0.2.0.txt | \
  shasum -a 256 -c -
xcrun stapler validate GreenBubbles-0.2.0-macos-arm64.dmg
```

Every executable is Developer ID signed and Apple notarized. The same release
ships `greenbubbles-*-macos-arm64.zip` with the full command-line tool set.

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

WeChat encrypts its databases with a key it derives at login and keeps to
itself, so the first step is capturing a copy from your own running client.
Three commands, about a minute.

```console
# 1. Let a debugger attach to your copy of WeChat, then restart WeChat.
sudo codesign --force --deep --sign - /Applications/WeChat.app

# 2. Check everything the capture needs is in place.
sudo greenbubbles-acquire preflight

# 3. Arm the capture, then log out of WeChat and log back in.
sudo greenbubbles-acquire capture
```

The logout in step 3 is the trick: WeChat derives its database keys only while
opening them, which happens at login. `capture` waits for that moment, reads
the key, and checks it against every database before saving it — on a recent
run, 26 of 26 in 45 seconds.

The key lands in `~/.greenbubbles-acquire/passphrase.txt`. Point a
[query profile](docs/QUERY_PROFILES.md) at it once and you never type it again.

Two things worth knowing: step 1 replaces Apple's signature on WeChat until you
reinstall or it updates, so repeat it after an update; and the key is stable,
so you only capture once.

For more detail — the full mechanism, every failure mode, and what to do when
preflight blocks — see
[acquiring your database key](docs/PASSPHRASE_ACQUISITION.md).

## Usage

```console
# what conversations are there?
greenbubbles conversations list --limit 20

# read one
greenbubbles messages list --conversation <id> --limit 50

# search everything (the term goes in on stdin, so it stays out of your history)
greenbubbles messages search --query-stdin

# who is in your address book?
greenbubbles contacts list --limit 50

# one exact message, and a photo from it
greenbubbles message get --conversation <id> --message <id>
greenbubbles attachment inspect <account-root> \
  --conversation <id> --message <id>    # see what attachments a message has
greenbubbles attachment materialize <account-root> \
  --conversation <id> --message <id> --kind image \
  --attachment <id> --output ~/photo.jpg
```

These local query commands print JSON and exit. They need no daemon or index and
upload nothing. Results are paged, so a query returns one screenful rather than your
whole history, and each result carries an id you can look up again.

Prefer a window? Open the app and choose **Browse Live or Snapshot…**, then
pick the `db_storage` folder for your account. Not sure where that is:

```console
greenbubbles-discover accounts --include-paths
```

### Giving an AI access

Write a policy that names the conversations and fields an assistant may see,
then let it query through that:

```console
greenbubbles connector-policy-direct <db_storage> policy.json <chat-id>... \
  --capabilities list,read,search --fields sender,created-at,content \
  --allow-remote-model --passphrase-stdin

greenbubbles connector-query-direct <db_storage> policy.json audit.ndjson \
  request.json --passphrase-stdin

GEMINI_API_KEY=... greenbubbles ai-summarize-direct \
  <db_storage> policy.json audit.ndjson new-memory-generation \
  --requester my-memory-agent --passphrase-stdin
```

Anything outside that policy is refused, every request is logged, and text
inside a message can never widen what the assistant is allowed to read.
The summary command invokes Gemini 3.7 Flash and publishes actual structured
and readable memory, not merely a transcript export. Its model input uses
compact `M###` evidence aliases; exact canonical message IDs remain in a
private sidecar for citation verification.

There is an agent skill in [`skills/`](skills/greenbubbles-context/SKILL.md) that
teaches a compatible assistant to use this properly.

See the [AI context guide](docs/AI_CONTEXT_CLI.md) for the full surface.

### Querying the replica

GreenBubbles exposes a separate replica query family for AI tools that need
structured, policy-scoped access without touching the live database.

```console
# Apply a scope policy to the replica, list recent messages, and search
greenbubbles tool-policy replica.db policy.json <chat-id>...  # set scope
greenbubbles tool-list replica.db policy.json                 # list conversations
greenbubbles tool-recent replica.db policy.json               # recent messages
greenbubbles tool-search replica.db policy.json               # search
greenbubbles tool-draft replica.db policy.json                # non-executing draft
```

`ai-query getChanges` provides a change-feed for incremental consumer sync —
useful when an AI tool needs to stay current with the replica without polling
the full history. `ai-export` produces static interchange and audit bundles for
offline analysis or archival. `ai-memory-export` produces QMD/Mem0 projections
with checkpoint IDs and `greenbubbles:message:<id>` citations that link every
inferred fact back to its source message. The replica tool policy uses
account-scoped one-way hashes that are distinct from the source-bound direct
connector policy, so replica access can be granted and revoked independently.

See [docs/AI_CONTEXT_CLI.md](docs/AI_CONTEXT_CLI.md) for the full surface.

### Turning your history into a living knowledge project

GreenBubbles extracts your message history into a self-evolving software project
— typed Python dataclasses with executable constraints, or structured Markdown
— following the UserAsCode methodology. Memory is organized by life domain
(identity, travel, finance, health, and more) and CRUD-patched incrementally:
new facts are added, changed facts are corrected in place, unchanged facts are
skipped. The project is git-versioned so every update is diffable and
reversible.

```console
# Prepare the corpus once (local, no API cost)
greenbubbles memory prepare corpus-v2 \
  --selection-policy selection-policy.json --profile live-account

# Extend it incrementally when new messages arrive
greenbubbles memory prepare corpus-v3 \
  --extend corpus-v2 \
  --selection-policy selection-policy.json --profile live-account

# Run an incremental extraction pass into a Python knowledge project
python3 scripts/personal-memory-parallel.py tick \
  --corpus corpus-v2 \
  --user-project ~/memory/me \
  --format python \
  --agent claude
```

The first `tick` creates `~/memory/me/` as a git repo and processes the full
corpus. Subsequent ticks process only new messages since the last run. Cadence
is user-configured — see [docs/PERSONAL_MEMORY.md](docs/PERSONAL_MEMORY.md) for
a cost table. Gemini 3.8 Flash is the recommended model.

The Python project looks like this after the first pass:

```python
# ~/memory/me/manifest.py
DOMAINS = {
    "identity":  "Name, DOB, passport | updated 2026-01-20",
    "travel":    "2 upcoming trips; passport expires 2026-06-01 | updated 2026-01-20",
    "health":    "Allergies: peanuts; Rx: cetirizine | updated 2025-12-01",
}
ACTIVE_ALERTS: list[str] = [
    "[CRITICAL] travel_readiness: Passport expires 2026-06-01, "
    "Singapore trip departs 2026-06-15 (only 14 days validity)",
]
```

The equivalent Markdown manifest:

```markdown
# Personal Memory Manifest
## Active Alerts
- [CRITICAL] travel_readiness: Passport expires 2026-06-01, Singapore trip departs 2026-06-15
```

See [docs/PERSONAL_MEMORY.md](docs/PERSONAL_MEMORY.md),
[format-python reference](skills/greenbubbles-personal-memory/references/format-python.md),
and [format-markdown reference](skills/greenbubbles-personal-memory/references/format-markdown.md).

## Documentation

| | |
| --- | --- |
| [User guide](docs/USER_GUIDE.md) | Setup, browsing, backups, recovery |
| [FAQ](docs/FAQ.md) | What goes wrong, and why |
| [CLI reference](docs/CLI_REFERENCE.md) | Every command |
| [Giving an AI access](docs/AI_CONTEXT_CLI.md) | Policies, exports, memory tools |
| [Living knowledge project](docs/PERSONAL_MEMORY.md) | UserAsCode extraction: corpus, formats, tick, manifest-refresh, revise |
| [Replica operations](docs/REPLICA_OPERATIONS.md) | Replica lifecycle, sync, and recovery |
| [AI memory integration](docs/AI_MEMORY_INTEGRATION.md) | QMD/Mem0 projections, citations, change-feed |
| [Backups](docs/RECOVERABLE_SNAPSHOTS.md) | The 24 words, rotation, recovery drills |
| [Architecture](docs/ARCHITECTURE.md) | How it works inside, and why |
| [Known limitations](docs/KNOWN_LIMITATIONS.md) | What is unproven or broken |

Everything else is indexed in [docs/](docs/README.md).

## Status

A research alpha for technical users. Reading, searching, browsing, backups and
the AI boundary all work and are tested. WeChat's format is closed and changes,
so anything GreenBubbles cannot decode is reported as a gap rather than
guessed at. Only Apple silicon is released. Sending is in the source but ships
closed and cannot be switched on in a public build.

Details in [known limitations](docs/KNOWN_LIMITATIONS.md), with every
performance number and its evidence in [measurements](docs/MEASUREMENTS.md).

## Contributing

Yes please — see [CONTRIBUTING.md](CONTRIBUTING.md). The most useful report is
a message type or table GreenBubbles reads incompletely, described structurally
with no message content attached. Security issues go to
[SECURITY.md](SECURITY.md), never a public issue.

Tests run on synthetic data only. Never add a real database, message, key or
path to this repository.

## License

MIT — see [LICENSE](LICENSE). Binary releases include
[third-party notices](THIRD_PARTY_NOTICES.md).

GreenBubbles is an independent project, not affiliated with or endorsed by
Tencent. WeChat and other product names are trademarks of their respective
owners.
