# Local passive-acquisition validation

Validation date: 2026-08-27

This is a content-free record of passive acquisition against the repository
owner's installed WeChat client. It contains no account path, account ID,
database digest, message, contact, media name, or secret. The private working
snapshots used for validation were connector-created temporary data and were
removed after the aggregate evidence below was recorded.

## Client and scope

The installed application matched the complete pinned profile:

- `com.tencent.xinWeChat` 4.1.12, build 269365;
- Team ID `5A4RE8SF68` and a valid deep signature;
- exact pinned executable and CodeDirectory hashes;
- universal `arm64` and `x86_64` application binary with Hardened Runtime.

Discovery found two readable account-scoped `db_storage` roots with distinct
attachment roots. Paths and stable account identifiers remained redacted. The
validation did not open any source for writing, launch or stop WeChat, invoke
its UI or IPC, access process memory or Keychain, inspect a reusable session
credential, or send a network/account operation.

## Bootstrap evidence

Read-only acquisition produced consistent owner-only bootstrap snapshots for
both discovered accounts:

| Snapshot | Current database sets | Storage result | Content access |
| --- | ---: | --- | --- |
| Larger/current account | 25 | 25 pinned WCDB/SQLCipher-family databases | None; passphrase required |
| Smaller/idle account | 15 | 15 pinned WCDB/SQLCipher-family databases | None; passphrase required |

The larger inventory contains local candidates for ordinary and business
message shards, message-resource and media stores, contacts, sessions, and the
passive SNS cache. The smaller inventory contains ordinary/business message,
message-resource, contact, session, and passive SNS candidates. A logical store
name and nonzero encrypted file size prove local persistence surfaces, not that
the database contains a useful row, that every message shard has been found, or
that an attachment file is downloaded.

An uncapped, metadata-only enumeration of the two `msg/attach` roots completed
with zero traversal issues and zero symbolic links. The larger root contains
136,741 regular-file candidates totaling 38,873,733,974 bytes; the smaller root
contains 87 totaling 1,469,805 bytes. Extension-only classification identified
43 documents, 4 audio candidates, and 1 video candidate in the larger root. The
remaining 136,693 larger-root files and all 87 smaller-root files use
unclassified names/extensions and were not guessed to be images or another
type. No filename, path, digest, timestamp, or file content was emitted or read.
This proves substantial locally persisted artifact candidates, not their
message linkage, format, decryptability, semantic usefulness, or completeness
relative to server history.

`greenbubbles-restore preflight` verified every copied database/WAL/SHM digest,
accepted the exact signed-client evidence, and classified every database from
its 16-byte header without decryption or schema enumeration. All 40 databases
were encrypted. Therefore real semantic restoration requires a stable 32-byte
owner-supplied database passphrase through standard input. No automated
passphrase-acquisition fallback was attempted or added.

## Incremental evidence

The first incremental attempt against the actively changing larger account was
correctly rejected because a selected source changed during a byte-copy
capture. This exposed that hashing large source files inline made the
consistency window unnecessarily long.

The snapshotter was changed to use `fclonefileat` with an already validated,
read-only, no-symlink source descriptor on APFS. It captures each file as an
atomic copy-on-write clone, keeps the database stable while its WAL/SHM group is
captured, and hashes the immutable clones afterward. The retried real
incremental then produced:

- 25 source sets in the authoritative current inventory;
- 7 changed sets selected and 0 reconciliation-only sets;
- 21 copied database/WAL/SHM entries;
- 21/21 entries marked `atomicCopyOnWriteClone`;
- 7/7 copied databases independently digest-verified and classified as the
  pinned encrypted storage family;
- a new source fingerprint derived from captured evidence.

The idle account produced a valid no-op incremental with 15 authoritative
source sets, zero changed/deleted/reconciliation sets, zero copied entries, and
the unchanged source fingerprint. This proves real changed-versus-idle source
selection and a resumable acquisition baseline. It does not prove decoded
message freshness or the 60-second p95 service objective.

A later retained baseline/current pair on the larger account provides an
independently replayable chain. The baseline held 25 sets and 75 DB/WAL/SHM
entries. The next incremental retained 25 sets, selected 9 changed sets, copied
27 entries, and reported no reconciliation-only or deleted sets. The offline
`audit-acquisition-chain` command digest-verified both snapshots and reproduced
exactly 9 content-changed sets from the complete inventories, with matching
baseline fingerprint and pinned client build. This proves classification and
copy proportionality, not decoded message semantics or latency.

## Remaining restoration gate

The snapshot command now records monotonic planning, descriptor-based
acquisition, and total durations for future controlled runs. The retained
validation snapshots predate those report fields, so no latency value is
retroactively inferred from them.

None of the encrypted database contents were read. Consequently this validation
does not close row accounting, logical-type coverage, relationship resolution,
attachment-path verification, playable media decoding, real-client replica
publication, edits/recalls/deletions, or crash-recovery evidence. Those items
must be evaluated together on a new immutable snapshot after the owner locally
supplies the correct passphrase through `--passphrase-stdin`. A passphrase must
never be committed, logged, placed on a command line, or sent through a model
prompt.
