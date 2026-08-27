# Conversation restoration specification

GreenBubbles restoration is lossless with respect to the authorized local
snapshot. A renderer may provide a simplified view, but the canonical restored
record must retain enough information to reproduce and audit every source row.

## Completeness contract

A restoration is complete only when all of the following are true:

1. Every message-bearing table in every supported message shard has been
   enumerated, including ordinary, group, business/public-account, and chatbot
   stores.
2. Every source row is represented exactly once in the canonical output, or is
   listed in a machine-readable rejection ledger with the database, table,
   stable row identity, and reason. Silent drops are forbidden.
3. Ordering uses the strongest source ordering available (`sort_seq`, server
   sequence, creation time, local ID, and source-row tie breakers) and remains
   deterministic when timestamps collide.
4. The original type value, low-16-bit logical type, compression markers,
   source fields, packed metadata, and undecoded payload bytes are retained.
5. Known message types receive a typed interpretation. Unknown types remain
   lossless opaque records and count as coverage gaps until documented.
6. Sender, conversation, quote/reply, recall/edit, reaction, and attachment
   relationships are resolved when the corresponding local source exists.
7. Each downloaded image, animation, audio item, video, document, thumbnail, or
   auxiliary artifact is linked to a canonical local artifact record containing
   an opaque path reference, size, digest, detected format, and verification
   state.
8. A remote-only, expired, deleted, corrupt, or not-downloaded attachment is
   represented explicitly. It is never reported as restored media.
9. Restoration never alters the source database or media tree and never places
   account secrets, plaintext histories, or stable identifiers in Git or normal
   logs.

## Canonical message envelope

Every message includes these source-preserving fields even if its typed decoder
also exposes friendlier data:

```text
account / conversation / sender opaque IDs
source database-set ID / table ID / row identity
local ID / server ID / sort sequence / timestamp
raw type / logical type / direction / status
content bytes / packed-info bytes / compression metadata
typed payload or explicit unknown-payload reason
zero or more relationship and artifact references
```

Raw payloads are held inside the local trust boundary. AI-facing tools receive
only policy-approved normalized fields, never the lossless archive by default.

## Integrity report

Each run produces counts that can be checked without printing message content:

- source tables and rows discovered;
- rows restored, rejected, duplicated, and unknown by logical message type;
- relationship references resolved and unresolved;
- attachment references resolved, missing, remote-only, and corrupt;
- source and output fingerprints;
- decoder and supported-client versions.

The completion invariant is:

```text
source rows = restored rows + rejected rows
restored rows = uniquely identified canonical messages
```

For a production-complete decoder, rejected rows must be zero. Unknown typed
payloads may be retained losslessly during development but prevent a claim of
full semantic restoration until each observed type is understood.

## Media path semantics

GreenBubbles records the verified location of the existing downloaded artifact;
it does not claim that a database reference proves a file exists. Paths are
redacted in default reports and can be revealed only by an explicitly
authorized local API. Symlinks escaping an authorized account root are rejected.

Encrypted `.dat` images and encoded voice data are separate from database
decryption. The resolver must retain the encrypted source, identify the decoder
version/key provenance, write any decoded derivative into connector-owned
storage, hash both, and avoid modifying the original.
