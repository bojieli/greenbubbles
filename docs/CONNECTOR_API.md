# Versioned local connector API

GreenBubbles serves the encrypted canonical replica through one deterministic
policy boundary. CLI scripts and the owner-only Unix socket use the same
`greenbubbles.connector.v1` request and response types. The service has no
WeChat database passphrase, source snapshot root, active-read adapter, or write
adapter; those privileges cannot be reached by changing request content.

## Starting the service

Create a mode-`0600` policy with `tool-policy`, then start the serving process:

```text
printf '%s\n' "$REPLICA_KEY" |
  greenbubbles-restore connector-serve \
    private/replica.db private/policy.json private/audit.ndjson \
    private/drafts private/connector.sock --replica-key-stdin
```

`REPLICA_KEY` is illustrative shell state, not a recommended persistent key
store. The key must be a distinct high-entropy 32-byte secret and is consumed
from standard input before the server begins accepting requests. It never
appears in a process argument, request, response, or audit event.
The socket parent directory must already be owner-only. GreenBubbles refuses to
replace an existing socket path and creates the socket with mode `0600`.

`connector-call` accepts a request only from an owner-only JSON file so private
queries and draft bodies do not appear in process arguments:

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

```text
greenbubbles-restore connector-call private/connector.sock private/request.json
```

Requests and responses reject an unsupported API version. Replica message
cursors bind the query, account, random replica generation, source fingerprint,
and exact checkpoint revision; any committed reconciliation invalidates them.
Change cursors bind the account and replica generation but remain resumable
across synchronization checkpoints.

Every non-empty underlying change page returns a new durable cursor, even when
policy filtering omits all of that page's records. A null cursor means the call
examined no later underlying record, so consumers retain the previously stored
cursor. This permits safe high-water capture and avoids replaying a final
partial page. The complete bootstrap/resume protocol is documented in
`SOURCE_CONNECTOR_CONTRACT.md`.

## Stable operations

The version-1 union contains:

```text
capabilities                 status
coverage                     getChanges
getCachedMoments
listConversations            searchMessages
getMessages                  getMessage                      getArtifact
resolveContact               resolveConversation
createMessageDraft           createReplyDraft
createAttachmentDraft        previewAction
bootstrap                    synchronize                    refresh
```

`bootstrap`, `synchronize`, and `refresh` have typed, machine-readable
unavailable responses in the serving process. Their actual passive acquisition
workflow remains isolated in the CLI because it requires source snapshots and,
for encrypted stores, an owner-supplied WeChat database passphrase. This keeps
serving a replica from implicitly granting source acquisition.

The separate `replica-publish`/`replica-follow` operator path can continuously
apply already restored authoritative generations to the encrypted replica. It
does not make these agent-facing operations available and cannot acquire a
source passphrase or accept an incremental fragment. See `REPLICA_FOLLOW.md`.

`capabilities` reports local passive read, passive cached Moments,
authenticated active read, drafts, text send, reply send, and file send
independently. Cached-Moments permission does not inherit from conversation
reads, and neither passive tier enables authenticated active reads. Send and active-read
capabilities remain unavailable until their controlling Phase 0.5 gates pass.
`status` includes exact client-build compatibility, authoritative checkpoint
and revision, acquisition mode, decoder version, latest synchronization and
integrity-scan timing, media phase, replica coverage health, and the enabled
conversation/operation scope.

## Deterministic authorization and minimization

Every content-returning operation intersects these policy dimensions before
reading or releasing a record:

- replica account;
- opaque conversation identity;
- operation capability;
- inclusive message time range;
- allowed normalized fields;
- local versus remote-model destination;
- configured result and byte limits.

`getArtifact(conversationId, artifactId)` is the only connector operation that
reveals an artifact path. It requires `readRecentMessages` plus the
`attachments` field for that exact conversation, proves that at least one
referencing message falls inside the inclusive policy time range, and is
unconditionally denied to the `remoteModel` destination even when message text
is remotely enabled. Immediately before release, the service descriptor-reads
and digest-verifies every recorded source/materialized/decoded file. The result
contains the explicit availability and decode states, source and decoded file
roles, absolute path, optional account-relative path, byte count, SHA-256, and
format. Missing/remote/deleted artifacts return their explicit state without a
fabricated file. A stale, replaced, missing, symlinked, or archive-escaping
file fails with an integrity error and releases no path.

`getCachedMoments` uses its own policy scope with independent allowed fields,
inclusive time range, local/remote destination permission, result limit, and
text-byte limit. Its response contains only selected normalized fields. Raw
columns, source identifiers, XML, pack-info blobs, local paths, and interaction
records are never included in the AI-facing view. The cache observation time
and `partialLocalCache` label remain visible so an agent cannot mistake a local
cache for complete server history.
The serving process also enforces a fixed rolling limit of 60 cached-Moments
requests per minute; denied requests are recorded in the same body-free audit
log.

For example, a cached-only local policy can be created without granting any
conversation reads:

```text
greenbubbles-restore tool-policy <archive> private/policy.json \
  --enable-cached-moments \
  --cached-fields author,created-at,type,content,title,url,media-count \
  --cached-not-before-unix 1700000000
```

Remote release remains denied unless
`--allow-cached-remote-model` is supplied separately.

The server never accepts SQL, source paths, replica paths, arbitrary internal
function names, or destination changes from message content. A remote-model
request fails unless that exact conversation explicitly permits remote release.
The remote/local destination is an explicit request-envelope property, not a
value inferred by a model from retrieved messages.

`getChanges` releases only events already carrying an authorized conversation
identity. Participant, artifact, and checkpoint events without such an identity
are omitted. The returned raw cursor still advances over them, allowing safe
resumption without exposing cross-scope entity identifiers.

## Immutable drafts and previews

A draft is a mode-`0600`, non-executing record. Its SHA-256 identity binds:

- the account and stable conversation identity;
- human-readable conversation kind, participant names/roles, group size, and
  owner evidence;
- optional reply target and the target canonical-record digest;
- exact rendered UTF-8 text and its digest;
- each attachment's artifact identity, kind, role, exact verified digest,
  byte count, and display filename;
- connector/API version and authoritative source checkpoint;
- deterministic policy-decision identity and requester identity;
- creation and expiration timestamps.

An attachment must be referenced by a message in the drafted conversation and
must have a valid verified SHA-256. A reply target must belong to the same
conversation and authorized time range. Any JSON modification, policy
replacement, connector upgrade, replica synchronization, recipient change,
reply change, attachment change, or expiry requires a new draft.
`previewAction` returns the exact text and recipient evidence, labels the record
non-executable, and never invokes WeChat.

The append-only audit JSONL is owner-only and body-free. It records requester,
request ID, operation, conversation, local/remote destination, outcome, counts,
draft ID, and policy-decision identity. Format-2 events digest their complete
contents and bind the predecessor digest under an exclusive append lock. The
service verifies the whole journal before startup, and
`audit-connector-log` independently returns an aggregate-only chain report.
Any format-1 prefix is retained but explicitly reported as unchained. Preview
operations provide the review transition available in the draft-only phase;
approval, attempt, and reconciliation transitions will be introduced only with
a gated action layer. See `CONNECTOR_AUDIT.md`.

The separate local `audit-connector-state` command requires the replica key
through stdin and verifies every draft filename, immutable content binding,
current/stale/expired state, and one-to-one completed request linkage against
the chained journal. It also resolves every completed review back to a draft
and rejects gated lifecycle stages. Its output is aggregate-only and it is not
an agent-facing connector operation.
