# GreenBubbles documentation

GreenBubbles reads your own WeChat history on your own Mac and releases bounded,
cited slices of it to tools you approve. The [repository
README](../README.md) is the introduction; this page is how you find the right
document next.

## Start here

| If you want to… | Read |
| --- | --- |
| Install it and browse your history | [User guide](USER_GUIDE.md) |
| Understand what it will and will not do before installing | [Known limitations](KNOWN_LIMITATIONS.md) and [threat model](THREAT_MODEL.md) |
| Fix something that is not working | [FAQ](FAQ.md) |
| Give an AI access to some of your history | [AI context CLI](AI_CONTEXT_CLI.md), then [AI tool boundary](AI_TOOL_BOUNDARY.md) |
| Build a cited personal wiki from a large corpus | [Personal memory and Pi](PERSONAL_MEMORY.md) |
| Keep a backup that survives losing WeChat | [Recoverable snapshots](RECOVERABLE_SNAPSHOTS.md) |
| Decide whether to acquire the database key | [Passphrase acquisition](PASSPHRASE_ACQUISITION.md) |
| Understand the design | [Architecture](ARCHITECTURE.md), then [storage format](STORAGE_FORMAT.md) |
| Check a claim before believing it | [Measurements](MEASUREMENTS.md) |
| Compare it with an exporter | [Comparison](COMPARISON.md) |
| Contribute | [Contributing](../CONTRIBUTING.md) |

## Use it

- [User guide](USER_GUIDE.md) — first run, choosing a source, browsing live
  history, creating and reopening snapshots, and what to do when a database
  will not open.
- [FAQ](FAQ.md) — the questions people actually ask, including the ones whose
  answer is "that is a real limitation."
- [Command-line reference](CLI_REFERENCE.md) — every command family, the access
  modes, and how secrets reach a process.
- [Query profiles](QUERY_PROFILES.md) — running repeated queries without
  retyping a source path or re-entering a key.
- [History browser](HISTORY_BROWSER.md) — the native macOS app: what it shows,
  what it stores, and what it deliberately cannot do.
- [Recoverable snapshots](RECOVERABLE_SNAPSHOTS.md) — the key hierarchy, the 24
  recovery words, protector rotation, retention, and the recovery drill.
- [Acquiring your database key](PASSPHRASE_ACQUISITION.md) — capturing the key
  from your own running client, verifying it, and the failure modes.

## Give an AI access

- [AI context CLI](AI_CONTEXT_CLI.md) — the one-shot query surface, the
  policy-scoped connector, and the static export bundle.
- [AI tool boundary](AI_TOOL_BOUNDARY.md) — what a tool is permitted to ask
  for, and what it can never reach.
- [AI memory integration](AI_MEMORY_INTEGRATION.md) — citation-preserving
  projections into local memory and retrieval systems.
- [Personal memory and Pi](PERSONAL_MEMORY.md) — canonical preparation,
  composable command-line conversation/kind/sender filters, RFC 3339 time
  bounds, compact durable batches, and agent-refined Markdown.
- [Connector API](CONNECTOR_API.md) — the versioned local request/response
  contract, the source connector requirements, and the resumable change
  consumer.

## Understand the design

- [Architecture](ARCHITECTURE.md) — why bounded live queries replaced full
  restoration, with the measurements that forced the decision.
- [Storage format](STORAGE_FORMAT.md) — what WeChat 4.1 actually writes to
  disk, how much of it is understood, and how gaps are reported.
- [Restoration specification](RESTORATION_SPEC.md) — the lossless archive
  format and the offline publication pipeline.
- [Replica specification](REPLICA_SPEC.md) — the encrypted canonical serving
  replica and its schema history.
- [Replica operations](REPLICA_OPERATIONS.md) — following a source, and
  preparing a recovery candidate without cutting over.
- [Public article fetch](PUBLIC_ARTICLE_FETCH.md) — the narrow boundary around
  fetching a publicly linked article.

## Verify and operate

- [Measurements](MEASUREMENTS.md) — every performance number in this project,
  with the machine, the date, the protocol, and what it does not establish.
- [Auditing](AUDITING.md) — independently verifying an archive, a serving
  replica, a pre-migration backup, an acquisition chain, and the connector's
  audit journal.
- [Known limitations](KNOWN_LIMITATIONS.md) — scope, platform, format,
  performance and evidence limits, in one place.
- [Operational response plan](OPERATIONAL_RESPONSE_PLAN.md) — what to do when a
  key, a snapshot, or a machine is compromised.
- [Distribution inventory](DISTRIBUTION_INVENTORY.md) · [public release
  checklist](PUBLIC_RELEASE_CHECKLIST.md) — dependency and licence accounting,
  and the gates a release has to pass.

## Boundaries

- [Threat model](THREAT_MODEL.md) — assets, adversaries, trust boundaries, and
  explicit non-goals.
- [Action safety contract](ACTION_SAFETY_CONTRACT.md) — the rules any
  outward-visible action would have to satisfy.
- [Send adapter](SEND_ADAPTER.md) — the experimental send path, and why public
  builds cannot leave dry-run mode.

## Where this is going

- [Roadmap](ROADMAP.md) — what is next and the gate each step must pass.
- [Comparison](COMPARISON.md) — how GreenBubbles differs from WeChat exporters
  and forensic tools, including where they are the better choice.

## Historical record

[`archive/`](archive/) holds superseded plans, feasibility studies and
development evidence. It is kept for provenance, not as a second documentation
set — see [its own README](archive/README.md) before citing anything in it.
