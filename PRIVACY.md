# Privacy

GreenBubbles collects nothing. There is no telemetry, no analytics, no crash
reporting, no update check, and no license check. Ordinary live/snapshot reads,
replica queries, exports and memory projections have no network client. The
explicit `ai-summarize-direct` command is the one exception: after policy and
destination checks, it sends only its compact authorized model input to the
Gemini API. Nothing about your use reaches the project author.

That is the easy half. The rest of this page is the part that actually matters:
what stays on your machine, what can leave it, and who decides.

## What lives on your machine

| Thing | Where | Sensitivity |
| --- | --- | --- |
| WeChat's own databases | untouched, where WeChat put them | your entire history |
| Snapshots | wherever you created them | your entire history, re-encrypted |
| The database key | wherever you stored it | opens everything, cannot be rotated |
| Recovery kit (24 words) | wherever you put it | opens a snapshot forever |
| Replica key | wherever you stored it | opens the serving replica |
| Query profiles | `~/.greenbubbles/` | paths, not secrets — still revealing |
| Audit journals | wherever you configured them | operations and counts, no bodies |
| Progress and evidence reports | wherever you wrote them | aggregates, sometimes schema paths |
| Generated-memory generations | wherever you wrote them | compact source text, model output, and private citation mapping |
| Personal-memory corpus indexes | wherever you wrote them | potentially every eligible chat message plus complete private citation/contact provenance |
| Personal-memory wiki and run state | wherever you wrote them | inferred facts, relationships, citations and processing progress |

GreenBubbles reads WeChat's files read-only and never writes to them. Every
private file it creates is mode `0600` in a mode-`0700` directory, and it
refuses to operate on files that are group- or world-accessible, symlinked, or
owned by another account.

One deliberate exception is a completed personal-memory corpus generation:
its files are finalized read-only (`0400`) and its directories traversal-only
(`0500`) so an agent cannot accidentally rewrite its evidence while updating
the separate wiki. The wiki and run state remain owner-only and writable.

## What can leave, and how

Content crosses GreenBubbles' AI boundary only when **you invoke a query or
export and choose its destination.**

- A **local model** receives whatever a policy you wrote permits — specific
  conversations, specific fields, a specific time range.
- A **remote model** receives nothing unless you explicitly enable remote
  release for that specific conversation. `destination: remote` is a property
  of the request envelope, never something a model can infer or assert for
  itself.
- **`ai-summarize-direct`** performs that remote release itself, only for
  conversation scopes marked `allowRemoteModel`. It sends short aliases and
  compact actor/time/type/text fields to Gemini 3.7 Flash; exact canonical
  message IDs, sender IDs, database metadata, policy and audit records stay
  local. `GEMINI_API_KEY` is read from the environment, not an argument.
- **`getArtifact`** — the only operation that reveals a file path — is
  unconditionally denied to a remote destination, even when message text for
  that conversation is remotely enabled.
- **`memory prepare/next/page/acknowledge/commit`** make no network request.
  A v2 canonical corpus can duplicate every eligible message into its private
  read-only index, so protect it like the source database. `memory next` prints
  only a delivery envelope; the Pi agent sees the selected chat text in
  deterministic at-most-49,152-byte `memory page` responses. Empty scope filters
  deliberately select the whole hydrated corpus. Personal-memory pages also
  include the real account/contact/conversation source IDs, names, aliases, and
  group titles needed for a faithful private wiki; only verbose canonical
  message provenance and database metadata remain in sidecars. If Pi uses a
  remote model, both page text and these identities leave the machine under
  that provider's terms even though GreenBubbles itself makes no request.

Every one of those decisions, allowed or denied, is appended to a hash-chained,
body-free journal you can verify with `audit-connector-log`.

### The boundary GreenBubbles cannot enforce

A local-first tool leaks everything if what is behind it is remote. Once a page
leaves this process, its privacy depends on:

- the model — local weights, or somebody's API;
- the embedder, if you build a memory index;
- the vector store;
- the agent framework's transcript and log retention;
- log collectors and crash reporters on the machine.

GreenBubbles marks a remote destination explicitly and refuses to release
artifact paths to one. It cannot audit what happens next. Approving each of
those is your job, and it is the part people get wrong. See
[THREAT_MODEL.md](docs/THREAT_MODEL.md).

## Other people

Your WeChat history is not only yours. It contains what other people said to
you, in confidence, without any expectation that it would be readable by a
language model years later.

Nothing technical resolves this. What the design gives you is the ability to be
narrow: policy scopes to conversations, fields and time ranges, so releasing
one project thread does not release the rest of your life or theirs. The
minimized view omits raw columns, source paths, packed metadata and raw XML;
exported bundles carry opaque participant IDs rather than WeChat identifiers.

Use the narrow scope. It exists for this.

## What the project sees

Nothing, unless you send it.

If you file an issue, send only content-free reports — most commands emit one
specifically for this purpose. Never attach a database, key, recovery phrase,
message, media file, account identifier, absolute path or memory dump. Security
reports follow [SECURITY.md](SECURITY.md).

The repository contains no real user data, and CI enforces that: a pre-commit
secret guard, `scripts/check-secret-hygiene.swift`, and a CI step that asserts
no release key or private material is present. Every test runs on synthetic
fixtures.

## Retention

GreenBubbles never age-purges or automatically discards a completed snapshot,
archive, replica, audit journal or query profile. Retention operations move
retired snapshots and archives into quarantine by atomic rename and leave them
there until you decide.

It does remove its own unpublished staging directories after a failed or
cancelled operation, and it removes session-only scratch files when a session
closes. Those cleanup paths cannot select a completed backup generation for
deletion.

That is deliberate — losing a backup silently is worse than keeping one too
long — but it means **removal is your responsibility.** If you want an old
generation gone, delete it yourself, after a recovery drill on the one you are
keeping.

## Uninstalling

Remove the application and the CLI binaries, then remove what you created:
snapshots, recovery kits, key files, replicas, audit journals, query profiles
under `~/.greenbubbles/`, any `~/.greenbubbles-acquire/passphrase.txt`, and
derived history indexes under
`Application Support/GreenBubbles/HistoryIndexes`.

If you installed the send helper, `greenbubbles-send uninstall-helper`
unregisters the login item and prints the `tccutil reset` commands that revoke
its Accessibility and Screen Recording grants. Nothing third-party was
installed, so nothing third-party remains.

Your WeChat data is untouched by all of this, because GreenBubbles never wrote
to it.
