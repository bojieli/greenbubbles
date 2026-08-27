# Versioned local connector API

GreenBubbles serves the encrypted canonical replica through one deterministic
policy boundary. CLI scripts, the owner-only Unix socket, and MCP use the same
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
appears in a process argument, request, MCP process, response, or audit event.
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

Requests and responses reject an unsupported API version. Replica message and
change cursors retain their existing checkpoint, query, account, and random
replica-generation bindings.

## Stable operations

The version-1 union contains:

```text
capabilities                 status
coverage                     getChanges
listConversations            searchMessages
getMessages                  getMessage
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

`capabilities` reports local passive read, authenticated active read, drafts,
text send, reply send, and file send independently. Send and active-read
capabilities remain unavailable until their controlling Phase 0.5 gates pass.
`status` includes exact client-build compatibility, authoritative checkpoint
age, replica coverage health, and the enabled conversation/operation scope.

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

The server never accepts SQL, source paths, replica paths, arbitrary internal
function names, or destination changes from message content. A remote-model
request fails unless that exact conversation explicitly permits remote release.
The remote/local destination is an envelope property or an MCP startup choice,
not a value inferred by a model from retrieved messages.

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
draft ID, and policy-decision identity. Preview operations provide the review
transition available in the draft-only phase; approval, attempt, and
reconciliation transitions will be introduced only with a gated action layer.

