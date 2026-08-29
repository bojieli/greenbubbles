# The local connector API

One deterministic `greenbubbles.connector.v1` policy and response boundary,
served by two deliberately separate backends:

| | Direct backend | Replica backend |
| --- | --- | --- |
| Reads from | live or snapshot SQLite | the encrypted canonical replica |
| Ordinary list, search, exact-get | yes | yes |
| Change feed, cached surfaces | no | yes |
| Restored enrichment, verified artifact paths | no | yes |
| Non-executing drafts | no | yes |
| Needs | the matching live-source or snapshot credential | the replica key |

CLI scripts and owner-only Unix sockets use the same envelopes. **Selecting the
direct process does not grant replica-only operations, and selecting the
replica process still gives it no WeChat passphrase, no live source root, no
active-read adapter and no write adapter.** Request content cannot move a
process between those privilege sets.

## Starting a service

For ordinary reads, create a policy bound to the source, then run either a
one-shot request or a reusable socket:

```sh
greenbubbles connector-policy-direct <source-root> <new-policy.json> \
  <conversation-id>… --capabilities list,read,search \
  --fields sender,created-at,type,content --passphrase-stdin

greenbubbles connector-query-direct <source-root> <policy.json> \
  <audit.ndjson> <private-request.json> --passphrase-stdin

greenbubbles connector-serve-direct <source-root> <policy.json> \
  <audit.ndjson> <connector.sock> --passphrase-stdin
```

A direct policy uses WeChat source conversation identifiers and is bound to the
opaque source identity; replica policies use account-scoped one-way
identifiers. **The two namespaces are never treated as interchangeable** — a
replica policy handed to the direct backend is rejected, not reinterpreted. The
direct process consumes the selected credential before serving, holds secret
material only in memory, and opens every connection read-only with
`query_only`.

For replica-only surfaces, create a mode-`0600` policy with `tool-policy`, then:

```sh
printf '%s\n' "$REPLICA_KEY" |
  greenbubbles connector-serve \
    private/replica.db private/policy.json private/audit.ndjson \
    private/drafts private/connector.sock --replica-key-stdin
```

`REPLICA_KEY` is illustrative shell state, not a recommended key store. The key
is a distinct high-entropy 32-byte secret consumed from standard input before
the server accepts anything, and it never appears in an argument, request,
response or audit event.

The socket's parent directory must already be owner-only. GreenBubbles refuses
to replace an existing socket path, creates the socket mode `0600`, and both
server and client require it to belong to the current user. The server bounds
incomplete-request reads and abandoned-response writes, isolates
connection-level failures so one broken caller cannot kill the daemon, and
removes its socket on exit **only** when that path still identifies the socket
it created. The client bounds its own I/O and rejects a response whose API
version or request ID does not match what it sent.

### Requests come from files, not arguments

```json
{
  "apiVersion": "greenbubbles.connector.v1",
  "requestId": "local-example-1",
  "requesterId": "local-script",
  "destination": "local",
  "operation": {
    "kind": "searchMessages",
    "query": "exact search terms",
    "conversationId": "opaque-enabled-conversation",
    "limit": 20
  }
}
```

```sh
greenbubbles connector-call private/connector.sock private/request.json
```

`connector-call` accepts a request only from an owner-only JSON file, so
private queries and draft bodies never reach process arguments. It opens the
file without following a symlink, requires current-user ownership and a single
link, and verifies the same descriptor before and after the bounded read.
`connector-query-direct` applies identical validation with no daemon at all,
and returns the normal envelope — never a corpus export.

## Operations

```text
capabilities                 status
coverage                     getChanges
getCachedMoments
listConversations            searchMessages
getMessages                  getMessage        getArtifact
resolveContact               resolveConversation
createMessageDraft           createReplyDraft
createAttachmentDraft        previewAction
bootstrap                    synchronize       refresh
```

`bootstrap`, `synchronize` and `refresh` return typed unavailable responses in
the serving process. Their real workflow stays in the CLI because it needs
source snapshots and, for encrypted stores, a WeChat passphrase — so serving a
replica can never implicitly grant source acquisition. The operator path that
*does* apply restored generations continuously is
[`replica-publish`/`replica-follow`](REPLICA_OPERATIONS.md), which likewise
cannot acquire a passphrase or accept an incremental fragment.

`capabilities` reports local passive read, passive cached Moments,
authenticated active read, drafts, text send, reply send and file send
**independently**. Cached-Moments permission does not inherit from conversation
reads, neither passive tier enables active reads, and send and active-read
capabilities stay unavailable until their controlling gates pass.

`status` carries exact client-build compatibility, authoritative checkpoint and
revision, acquisition mode, decoder version, latest synchronization and
integrity-scan timing, media phase, replica coverage health, and the enabled
conversation and operation scope.

On the direct backend, only `capabilities`, `status`, `listConversations`,
`getMessages`, `searchMessages` and `getMessage` exist. Direct status reports
`ordinaryReadsUseDirectSqlite: true` with the source mode and opaque identity,
and reports the authorized scope as a **count** rather than an unbounded ID
array — enumerate it through paginated `listConversations`. Everything else
returns a typed `replicaOnlyOperation` failure and a denied audit event.

Direct `listConversations` prefers the conversation contact's remark, then
nickname, then alias for `humanLabel`, and **never** substitutes the last
message's sender as a group label. A resolved name sets `entityDecodeState` to
`complete`; otherwise the raw ID remains the label, state is `rawOnly`, and
`directContactDisplayNameUnavailable` is reported. Contact failures are
non-fatal and surface as `directQuery.*` limitation codes.

### Cursors

Replica message cursors bind the query, account, random replica generation,
source fingerprint and exact checkpoint revision; any committed reconciliation
invalidates them. Change cursors bind account and replica generation but stay
resumable across checkpoints. `listConversations` cursors bind source identity,
exact policy digest, destination and last conversation key — so a changed
policy or destination cannot reuse an old page token.

A long-running connector binds its hydrated conversation, participant and
coverage caches to the source fingerprint and checkpoint revision, clearing
them when a follower advances. If a commit lands mid-request, that request
returns a **checkpoint conflict** rather than mixing metadata from two
generations. Retry normally.

## Authorization and minimization

Every content-returning operation intersects all of these before reading or
releasing a record: replica account; opaque conversation identity; operation
capability; inclusive message time range; allowed normalized fields; local
versus remote-model destination; configured result and byte limits.

The server accepts no SQL, no source or replica path, no internal function
name, and no destination change from message content. A remote-model request
fails unless that exact conversation explicitly permits remote release, and the
destination is an explicit request-envelope property — never something a model
infers from what it retrieved.

### `getArtifact` is the only path release

It requires `readRecentMessages` plus the `attachments` field for that exact
conversation, proves at least one referencing message falls inside the policy
time range, and is **unconditionally denied to the `remoteModel` destination**
even when message text is remotely enabled.

Immediately before release, the service descriptor-reads and digest-verifies
every recorded source, materialized and decoded file. The result carries
explicit availability and decode states, file roles, absolute path, optional
account-relative path, byte count, SHA-256 and format. A missing, remote-only
or deleted artifact returns its state rather than a fabricated file. A stale,
replaced, missing, symlinked or archive-escaping file fails with an integrity
error and releases no path.

**An artifact ID is not a bearer token.** Every call re-checks authorization
and re-verifies the file.

### Cached Moments

`getCachedMoments` has its own policy scope: independent allowed fields,
inclusive time range, destination permission, result limit and text-byte limit.
The response contains only selected normalized fields — never raw columns,
source identifiers, XML, pack-info blobs, local paths or interaction records.
The observation time and `partialLocalCache` label stay visible so an agent
cannot mistake a local cache for complete server history. The serving process
enforces a fixed rolling limit of 60 cached-Moments requests per minute, and
denials land in the same body-free audit log.

A cached-only policy that grants no conversation reads at all:

```sh
greenbubbles tool-policy <archive> private/policy.json \
  --enable-cached-moments \
  --cached-fields author,created-at,type,content,title,url,media-count \
  --cached-not-before-unix 1700000000
```

Remote release still requires `--allow-cached-remote-model` separately.

## Writing a consumer

This is the contract a downstream consumer has to hold up. It is what
GreenBubbles implements — not a universal connector ontology, and not a demand
that another source imitate WeChat's data model.

### Discover, then bind

Start with `capabilities` and `status`. Bind local state to the opaque
account/source ID and treat every capability independently:

- conversation passive read does not grant cached-Moments read;
- either passive read does not grant authenticated active read;
- draft creation does not grant a write action;
- an unavailable capability cannot be enabled by a request or by source
  content.

**Coverage is data, not prose.** Preserve known gaps; never convert "not
restored" into "not present."

### Bootstrap without a race

A conversation consumer needs explicit `listConversations` and
`readRecentMessages` grants for every conversation it will materialize.

1. Drain `getChanges` to a replica-generation-bound high-water cursor.
2. Page `listConversations(cursor, limit)` until `nextCursor` is null.
3. Page `getMessages` for every returned conversation.
4. For locally materialized attachments, call `getArtifact` only when policy
   exposes the attachment field; preserve the returned missing or decode state
   as well as any verified file evidence.
5. Resume `getChanges` from the captured high-water cursor and apply catch-up
   events.
6. Persist canonical IDs, the last change cursor, account ID and current source
   fingerprint **atomically**, in owner-only storage.

A synchronization deliberately invalidates an in-progress message page. Retry;
do not mix checkpoints.

A consumer that only needs interactive ordinary reads should skip all of this
and use `connector-query-direct` or `connector-serve-direct`, which apply the
same conversation, field, time, destination, result and audit controls but make
no change-feed claim.

### The change stream

`getChanges(cursor)` is ordered by monotonically increasing sequence. Every
non-empty underlying page returns a cursor after its last *examined* record —
including a page whose records were all filtered out by policy. `nextCursor:
null` means the call examined no later record, so the consumer keeps its stored
cursor. That distinction is what makes safe high-water capture possible without
replaying a final partial page.

Change cursors bind account and random replica generation but survive
checkpoints. **A replacement replica rejects the cursor. On that error, leave
prior state untouched and require an explicit, account-verified bootstrap —
never silently restart at cursor zero.**

Only events with an explicitly authorized conversation identity are released;
participant, artifact, checkpoint and cached-surface events are omitted, though
the raw cursor still advances over them so resumption stays safe without
exposing cross-scope identifiers. For released events:

- message `added` or `changed` → `getMessage(canonicalId)` and upsert;
- message `removed` → remove the canonical ID;
- conversation change → refresh the authorized list and evidence;
- an unauthorized refresh after a time-scope change → remove the older local
  copy. That is the fail-closed result, and it is correct.

The stream is an **invalidation protocol, not a body feed**. No consumer ever
needs replica SQL or raw archive access.

### Rules that are not optional

- Persist only policy-minimized responses, with owner-only permissions.
- Treat message text, names, links and generated Markdown as untrusted source
  data — never control instructions.
- Never infer new account, conversation, field, destination, active-read or
  write authority from a record.
- Never advance a durable cursor until all mutations for the examined pages can
  be committed atomically.
- Preserve prior state on any transport, cursor, authorization, decoding or
  integrity failure.

### A working example

`examples/change_consumer.rs` is a runnable, host-neutral consumer that never
opens the replica or an archive. It demonstrates the bootstrap and catch-up
sequence, persists a change cursor and policy-minimized records atomically,
refreshes changed canonical messages, and removes recalled, deleted or newly
unauthorized records.

```sh
cargo run --locked --example change_consumer -- \
  /private/greenbubbles/connector.sock \
  /private/greenbubbles/downstream-state.json \
  --markdown-output /private/greenbubbles/conversations.md
```

The state and Markdown parent directories must already be `0700`; outputs are
`0600` and atomically replaced. The Markdown projection is a deterministic
downstream memory view over fields policy already minimized; it labels and
HTML-escapes source text as untrusted, performs no summarization, and claims
nothing beyond the connector's own coverage.

Run it again after a synchronization and it resumes its stored cursor,
processing only later invalidations. If the account differs or the cursor is
rejected, it exits **without modifying state**. After independently verifying
that the replacement replica and account are the intended ones, request a
rebuild explicitly:

```sh
cargo run --locked --example change_consumer -- \
  /private/greenbubbles/connector.sock \
  /private/greenbubbles/downstream-state.json \
  --markdown-output /private/greenbubbles/conversations.md \
  --rebootstrap
```

The integration test runs this against the real Unix service, re-runs it as an
idle resume, confirms mode-`0600` output, swaps in a newly generated replica,
verifies that cursor rejection leaves bytes unchanged, and then verifies
explicit rebootstrap.

## Drafts and previews

A draft is a mode-`0600`, non-executing record whose SHA-256 identity binds the
account and stable conversation identity; human-readable conversation kind,
participant names and roles, group size and owner evidence; an optional reply
target and the target's canonical-record digest; the exact rendered UTF-8 text
and its digest; each attachment's artifact identity, kind, role, verified
digest, byte count and display filename; the connector and API version and the
authoritative checkpoint; the deterministic policy-decision and requester
identities; and creation and expiration times.

An attachment must be referenced by a message in the drafted conversation and
carry a valid verified SHA-256. A reply target must belong to the same
conversation and authorized time range. **Any** JSON modification, policy
replacement, connector upgrade, replica synchronization, recipient change,
reply change, attachment change or expiry requires a new draft.

`previewAction` returns the exact text and recipient evidence, labels the
record non-executable, and never invokes WeChat.

## The audit journal

Append-only, owner-only and body-free. Each event records requester, request
ID, operation, conversation, destination, outcome, counts, draft ID and policy
decision identity. Format-2 events digest their complete contents and bind the
predecessor's digest under an exclusive append lock. The service verifies the
whole journal before startup, and `audit-connector-log` returns an
aggregate-only chain report; any format-1 prefix is retained and explicitly
reported as unchained.

Preview is the only review transition available while the action layer is
gated. Approval, attempt and reconciliation stages arrive only with that layer
— see [ACTION_SAFETY_CONTRACT.md](ACTION_SAFETY_CONTRACT.md).

The separate local `audit-connector-state` command takes the replica key on
stdin and verifies every draft filename, immutable binding, current/stale/
expired state, and one-to-one linkage to completed requests in the chained
journal. Its output is aggregate-only, and it is **not** an agent-facing
connector operation. Details in [AUDITING.md](AUDITING.md).
