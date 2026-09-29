# GreenBubbles documentation

GreenBubbles reads your own WeChat history on your Mac and lets you choose
which parts, if any, an AI gets to see. Start with the
[repository README](../README.md). Use this page to find the right guide next.

## Start here

| I want to… | Read |
| --- | --- |
| Install the command-line tool or check a download | [CLI releases and Homebrew](HOMEBREW.md) |
| Set up the key GreenBubbles needs | [Key setup guide](PASSPHRASE_ACQUISITION.md) |
| Install and browse my history | [User guide](USER_GUIDE.md) |
| Have my coding agent write notes from my chats | [Agent skills](AGENT_SKILLS.md) |
| Understand how those notes are organized | [Personal memory](PERSONAL_MEMORY.md) |
| Give an AI access to only some chats | [AI context CLI](AI_CONTEXT_CLI.md), then [AI tool boundary](AI_TOOL_BOUNDARY.md) |
| Keep a backup that works without WeChat | [Recoverable snapshots](RECOVERABLE_SNAPSHOTS.md) |
| Fix something that isn't working | [FAQ](FAQ.md) |
| Know what it can't do before installing | [Known limitations](KNOWN_LIMITATIONS.md) and [threat model](THREAT_MODEL.md) |
| Understand how it works | [Architecture](ARCHITECTURE.md), then [storage format](STORAGE_FORMAT.md) |
| Check a performance claim | [Measurements](MEASUREMENTS.md) |
| Compare it with WeChat export tools | [Comparison](COMPARISON.md) |
| Contribute | [Contributing](../CONTRIBUTING.md) |

## Everyday use

- [User guide](USER_GUIDE.md): first run, finding your WeChat data, browsing,
  and making and opening backups.
- [FAQ](FAQ.md): common questions, including the ones where the answer is
  "that's a real limitation."
- [Command-line reference](CLI_REFERENCE.md): every command, and how keys are
  passed to it safely.
- [Query profiles](QUERY_PROFILES.md): saved settings for reading a second
  account or a backup without retyping paths and keys.
- [History browser](HISTORY_BROWSER.md): the Mac app, what it shows and stores,
  and what it deliberately can't do.
- [Recoverable snapshots](RECOVERABLE_SNAPSHOTS.md): encrypted backups, the 24
  recovery words, changing how a backup is unlocked, and testing recovery.
- [Key setup guide](PASSPHRASE_ACQUISITION.md): copying the database key from
  your own WeChat app, checking it, and fixing problems.

## Using GreenBubbles with AI

- [Agent skills](AGENT_SKILLS.md): use GreenBubbles from the coding agent you
  already have (Claude Code, Codex, and others), including keeping notes up to
  date.
- [Personal memory](PERSONAL_MEMORY.md): how the agent turns your chats into a
  set of notes, one article per area of your life.
- [AI context CLI](AI_CONTEXT_CLI.md): answering one AI request at a time,
  giving an AI access limited to chats you choose, and the built-in Gemini
  summarizer.
- [AI tool boundary](AI_TOOL_BOUNDARY.md): what an AI tool may ask for, and
  what it can never reach.
- [AI memory integration](AI_MEMORY_INTEGRATION.md): feeding cited messages into
  local memory and search systems.
- [Connector API](CONNECTOR_API.md): the request and response format for
  programs that integrate with GreenBubbles.

## How it works

- [Architecture](ARCHITECTURE.md): why GreenBubbles reads WeChat's data in
  place instead of exporting all of it, with the measurements behind that
  choice.
- [Storage format](STORAGE_FORMAT.md): what WeChat 4.1 writes to disk, how much
  of it GreenBubbles understands, and how gaps are reported.
- [Restoration specification](RESTORATION_SPEC.md): the full, lossless export
  format.
- [Replica specification](REPLICA_SPEC.md) and
  [replica operations](REPLICA_OPERATIONS.md): an optional encrypted local copy
  of your history that stays in sync with WeChat.

## Checking and operating

- [Measurements](MEASUREMENTS.md): every performance number, with the machine,
  date, and method, and what it doesn't prove.
- [Auditing](AUDITING.md): independently checking exports, replicas, backups,
  key captures, and the AI access log.
- [Known limitations](KNOWN_LIMITATIONS.md): everything GreenBubbles doesn't
  support or hasn't proven yet, in one place.
- [Operational response plan](OPERATIONAL_RESPONSE_PLAN.md): what to do if a
  key, a backup, or a Mac is compromised.
- [Distribution inventory](DISTRIBUTION_INVENTORY.md) and
  [public release checklist](PUBLIC_RELEASE_CHECKLIST.md): third-party
  licenses and the checks every release must pass.

## Safety boundaries

- [Threat model](THREAT_MODEL.md): what GreenBubbles protects, from whom, and
  what it doesn't try to do.
- [Action safety contract](ACTION_SAFETY_CONTRACT.md): the rules any action
  visible to other people would have to follow.
- [Send adapter](SEND_ADAPTER.md): the experimental message-sending code, and
  why public builds can't send.

## Where it's going

- [Roadmap](ROADMAP.md): what's next, and what each step must prove first.
- [Comparison](COMPARISON.md): how GreenBubbles differs from WeChat export and
  forensic tools, including when those are the better choice.

## Old documents

[`archive/`](archive/) keeps old plans, feasibility studies, and development
notes for the record. They may be out of date; read
[its README](archive/README.md) before relying on anything there.
