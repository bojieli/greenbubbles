# Independent acquisition-chain audit

`greenbubbles-restore audit-acquisition-chain` verifies that an incremental or
integrity-scan snapshot is an exact continuation of a supplied authoritative
baseline:

```sh
cargo run --locked \
  --manifest-path Native/GreenBubblesRestore/Cargo.toml \
  --bin greenbubbles-restore -- \
  audit-acquisition-chain <previous-snapshot> <current-snapshot>
```

Both snapshot directories and manifests must be owner-only and non-symlinked.
Every copied DB/WAL/SHM entry is independently digest-verified before the two
complete source inventories are compared. The command then requires:

- the integrity-bound selected-account evidence to be exactly unchanged at both
  endpoints (legacy unbound chains remain explicitly unbound);
- the current baseline fingerprint to equal the previous snapshot's source
  fingerprint;
- signed WeChat 4.1+ compatibility at both endpoints; exact build equality is
  reported but an ordinary compatible client update does not break the chain;
- reported deletions to exactly equal the source sets absent from the current
  inventory;
- for an incremental acquisition, reported changed source sets to exactly
  equal the sets whose file-role, identity, size, timestamp, or SHA-256 evidence
  changed;
- every reconciliation-only set to be byte-evidence-identical to its baseline;
  and
- the current manifest's selected copied entries to remain complete and
  consistent with its current source inventory.

The successful format-3 JSON result contains only aggregate counts, modes, and
booleans, including `accountBindingUnchanged`.
It emits no snapshot/account identifier, source-set identity, path, timestamp,
digest, database name, or content.

## Current local evidence

On 2026-08-27, an owner-authorized bootstrap and a subsequent incremental from
the exact pinned WeChat 4.1.12 build were independently audited:

- both inventories contained 25 current source sets;
- the bootstrap contained 75 copied DB/WAL/SHM entries;
- the incremental reported 9 changed sets and copied 27 entries;
- independent inventory comparison reproduced exactly 9 content-changed sets;
- no reconciliation-only or deleted set was reported or independently found;
- all 9 copied databases remained in the expected encrypted WCDB/SQLCipher
  family; and
- the client build was unchanged in this observed run and baseline-fingerprint
  continuity passed.

This proves real change-proportional passive acquisition and exact manifest
classification. It does not prove which messages changed, decoded message
latency, replica publication, edits/recalls/deletions, or the 60-second p95
objective because database contents were not decrypted. Those claims require
the owner to enter the stable database passphrase locally through stdin.
