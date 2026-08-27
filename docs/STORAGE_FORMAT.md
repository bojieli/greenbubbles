# Pinned local-storage profile

This document records the evidence used by the current passive restoration
adapter. It is a compatibility profile, not a promise about untested WeChat
versions.

## Supported client build

Snapshot manifest format 2 carries signed-client evidence gathered before any
database restoration. The only production-compatible profile currently is:

- bundle/signing identifier `com.tencent.xinWeChat`;
- marketing version `4.1.12`, build `269365`;
- Team ID `5A4RE8SF68`;
- architectures `arm64` and `x86_64`;
- Hardened Runtime and a valid strict code signature;
- executable SHA-256
  `2c61ba7f64c2b98e897553cd226364642a1eb213b5b7f74556c6fc2efc363e32`;
- full SHA-256 CodeDirectory hash
  `fa11b242567cbe161e2b332139dbc459c534b85f3855a8603614252bf908106e`.

The snapshotter opens the executable read-only without following symlinks,
hashes it while checking file identity for mutation, invokes `codesign` and
`lipo` directly without a shell, and stores the evidence in the snapshot
manifest. Restoration classifies it as `supportedPinned`, `unsupported`, or
`missing` and names every mismatched field. Old format-1 synthetic fixtures are
classified separately as `legacySyntheticFixture` and never establish
production support.

An unsupported or missing build may still be parsed to retain authorized raw
evidence, but format-2 output cannot set `fullRestorationAchieved` and no future
active-read or write adapter may be enabled from it. Malformed fingerprints are
rejected before database preparation.

## Supported encrypted database family

The adapter targets the WCDB/SQLCipher-4 profile observed in macOS WeChat
4.1.x:

- 4096-byte pages;
- AES-256-CBC page encryption;
- PBKDF2-HMAC-SHA512 with 256,000 iterations;
- HMAC-SHA512 page authentication;
- 80 reserved bytes per page;
- one 32-byte database passphrase shared by the account stores, with a distinct
  per-file salt;
- encrypted WAL frames applied to the decrypted copy before reading.

The profile is version-pinned in the restoration dependency. A plaintext
`SQLite format 3` header selects ordinary SQLite; any non-SQLite header is
treated as the pinned encrypted family and must decrypt successfully. It is not
guessed as another format after failure.

Passphrases enter through standard input and live only in zeroized process
memory. The engine has no runtime network client. Decrypted databases exist
only in an owner-only temporary directory and are removed when the catalog is
dropped.

Snapshot manifest format 3 separates the complete source-set inventory from
the database sets copied in a particular acquisition. Database, WAL, and SHM
presence and full file identity determine change selection. The format records
bootstrap, incremental, or integrity-scan mode, the bounded reconciliation
window, selected sets, deleted sets, and a verified SHA-256 for every current
source file. Rust validates that the selected entries exactly match this
inventory before opening a database.

Acquisition evidence format 2 records the last bootstrap/integrity-scan anchor.
The planner automatically selects every current set after the configured
maximum interval, while incrementals preserve the anchor. This scheduling
metadata is part of the manifest, not inferred from filesystem wake-up hints.

## Message and auxiliary stores

Message tables are discovered by both hashed `Msg_`/`Chat_` naming and required
column signatures, allowing ordinary, business, and chatbot schema variants to
be included. Field aliases are resolved dynamically. Every column is retained
using its original SQLite storage class and bytes.

Every table in every prepared database is also recorded in the schema coverage
ledger. Message-like tables that do not meet the supported adapter signature
remain explicit completion-blocking candidates until their role is proved and
an adapter or auxiliary classification is added.

Coverage format 3 fingerprints every table from ordered `PRAGMA table_xinfo`
metadata plus its related `sqlite_schema` table, index, and trigger objects. A
second digest binds the complete ordered logical-path/table profile. These
SHA-256 values expose exact schema drift without publishing the underlying SQL.
Row mutations do not affect them. Cached-surface coverage format 2 uses the same
fingerprints, and authoritative incremental merges recompute both profiles.
Older archives deserialize with absent fingerprint evidence and cannot be
silently upgraded to a claimed observed profile.

Known auxiliary chains include:

```text
message row
  -> MessageResourceInfo (local/server ID, packed-info bytes)
  -> MD5/title metadata
  -> account-scoped msg or business media tree

voice message
  -> VoiceInfo (server ID, then local-ID fallback)
  -> raw Tencent SILK payload
```

`MessageResourceInfo`, `VoiceInfo`, session, contact, and group columns are
matched through verified aliases instead of one fixed schema. Their source rows
are retained in the local archive where they contribute to a normalized
entity.

## Media variants

The adapter retains encrypted image sources and supports legacy single-byte XOR,
V1 fixed-key AES, and V2 per-account-key `.dat` variants. It verifies and
records images, stickers, video payloads and posters, documents, thumbnails,
and raw voice blobs. Voice transcoding to Ogg Opus is attempted without ever
replacing the SILK source.

Not-downloaded, remote-only, expired, deleted, corrupt, ambiguous, unsafe, and
key-unavailable media are distinct states. The current adapter does not infer
which remote state applies unless local metadata proves it; a generic local miss
therefore remains `notDownloaded`.

## Uncertainty and completion

Observed-but-unknown message types, generic app subtypes, undecoded nested
merged-message children, unresolved sender directions, failed group protobufs,
ambiguous relationships, and unavailable media decoders remain machine-readable
coverage gaps. They do not prevent raw retention, but they keep
`fullRestorationAchieved` false until the exact observed corpus is understood.
