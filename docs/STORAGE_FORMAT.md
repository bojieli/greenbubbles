# Pinned local-storage profile

This document records the evidence used by the current passive restoration
adapter. It is a compatibility profile, not a promise about untested WeChat
versions.

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

## Message and auxiliary stores

Message tables are discovered by both hashed `Msg_`/`Chat_` naming and required
column signatures, allowing ordinary, business, and chatbot schema variants to
be included. Field aliases are resolved dynamically. Every column is retained
using its original SQLite storage class and bytes.

Every table in every prepared database is also recorded in the schema coverage
ledger. Message-like tables that do not meet the supported adapter signature
remain explicit completion-blocking candidates until their role is proved and
an adapter or auxiliary classification is added.

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
