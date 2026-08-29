# Independently recoverable snapshots

This is the detailed operator reference for creation, verification, protector
rotation, and retention. For a GUI-first walkthrough and ordinary browsing
commands, start with the [GreenBubbles user guide](USER_GUIDE.md).

GreenBubbles has two different snapshot concepts. They solve different
problems and should not be confused.

The Swift acquisition snapshot is a short-lived, consistent filesystem capture
of WeChat's encrypted database, WAL, and SHM files. It preserves exact source
evidence and is well suited to forensic restoration, but it remains encrypted
with WeChat's key.

The native recoverable snapshot is a logical SQLite backup. SQLite reads each
source through the authorized WeChat key and writes a new SQLCipher database
under a GreenBubbles recovery key. Its durable readability is independent of
the WeChat application and its key material.

The two stages can now be composed without plaintext: acquire a stable
filesystem capture first, then use `snapshot create-capture` to convert that
complete capture into the independently protected durable format.

## Recovery model

The portable recovery protector is a 24-word English BIP-39 mnemonic generated
from 256 random bits. The words encode the full recovery entropy plus a checksum;
they are not selected by the user and should not be edited into a memorable
sentence. That recovery entropy wraps a separate random SQLCipher database key,
so adding or changing protectors does not require decrypting databases to
plaintext or rewriting their pages.

BIP-39 is used only as a well-reviewed, checksummed human encoding. A
GreenBubbles recovery phrase is not a cryptocurrency wallet seed: never reuse a
wallet phrase here, and never import the GreenBubbles phrase into a wallet.

Create a recovery kit in an owner-only directory before creating the snapshot:

```sh
umask 077
mkdir -m 700 -p /private/greenbubbles-recovery
greenbubbles snapshot recovery-kit create \
  /private/greenbubbles-recovery/family-a.txt
```

Store a second copy offline in a password manager, encrypted removable medium,
or another recovery system controlled by the owner. A device-only Keychain copy
is convenient but is not, by itself, a portable backup. Do not commit the key,
paste it into an issue or model prompt, put it in a command argument, or store it
beside the only copy of the snapshot.

The CLI creates the file exclusively with mode `0600`, validates its BIP-39
checksum, synchronizes it, and prints only a content-free JSON report. It does
not print the words or a base64 key. Read the file yourself and copy the words
to the independent recovery location before beginning the database conversion.
Snapshot creation accepts an already-created kit, so the words exist before
the potentially long database copy starts. Run `snapshot recovery-kit validate`
against the intended recovery copy before relying on it.

The History app's **Create Recoverable Snapshot** flow performs this ceremony:
it creates the owner-only kit first, displays all 24 words once, asks for four
randomly selected word positions, and refuses to begin conversion until those
answers match and the owner confirms an independent copy. The kit remains on
disk if the owner cancels or conversion fails. Displayed words, confirmation
answers, the WeChat key, and any snapshot passphrase are cleared from view state
when conversion begins.

The CLI can additionally create a separate local-unlock protector. This lets
the same account open the snapshot without reading the recovery words on every
launch:

```sh
greenbubbles snapshot local-credential create \
  /private/greenbubbles-local/.family-a-unlock
```

This mode-`0600`, current-user-owned, single-link file contains a distinct
random wrapping credential. It contains neither the SQLCipher database key nor
the 24 words, and the manifest contains only its public identifier and an
authenticated wrapped key. It must never be the only recovery copy. Deleting it
only disables convenient local unlock; the 24 words remain sufficient.

On macOS the graphical flow defaults to storing that same random convenience
credential as a generic-password Keychain item scoped to the snapshot identity
and marked `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`. It is not synchronized
to another device. When opening the snapshot, the app writes the credential to
a newly created mode-`0600` file in an owner-only temporary session directory,
passes only that path to the CLI, and removes the directory when the source is
closed. The hidden-file choice remains available as a cross-platform fallback;
the app remembers its path, never its contents.

An optional passphrase can wrap the same database key. It uses Argon2id v1.3
with 64 MiB memory, time cost 3, parallelism 1, then XChaCha20-Poly1305 with
authenticated snapshot/protector/KDF metadata. The app never persists this
passphrase. The CLI accepts one UTF-8 line of 12–1,024 bytes. A passphrase is a
secondary convenience/recovery mechanism and cannot replace the mandatory
24-word protector.

Format-1 snapshots protected directly by a raw 256-bit key remain supported for
compatibility. New operational examples use the format-2 recovery hierarchy.

## Create from an encrypted WeChat source

With format 2, standard input contains the 32-byte WeChat key and, when enabled,
the optional snapshot passphrase on the next line. The CLI reads the recovery
and optional local protector from their private files, generates a distinct
random database key, and wraps it under every selected protector.

```sh
{ cat wechat-key.txt; cat snapshot-passphrase.txt; } | \
  greenbubbles snapshot create \
  <WeChat-db_storage-root> <new-snapshot-directory> \
  --source-passphrase-stdin \
  --snapshot-recovery-kit /private/greenbubbles-recovery/family-a.txt \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock \
  --snapshot-passphrase-stdin
```

All credential files must be current-user-owned mode `0600`, single-link files
inside owner-only directories. The output parent must already be an owner-only
real directory, and the final output path must not exist. A local credential or
passphrase is accepted only together with a portable recovery kit;
GreenBubbles refuses a device-local-only or passphrase-only backup. With an
encrypted source, stdin line 1 is the WeChat key and line 2 is the optional
snapshot passphrase.

For an explicitly plaintext source, no source key is read. If a snapshot
passphrase is enabled, it is stdin line 1; otherwise stdin is empty:

```sh
cat snapshot-passphrase.txt | \
  greenbubbles snapshot create \
  <plaintext-db_storage-root> <new-snapshot-directory> \
  --source-decrypted \
  --snapshot-recovery-kit /private/greenbubbles-recovery/family-a.txt \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock \
  --snapshot-passphrase-stdin
```

## Create from a stable filesystem capture

When conversion should not span a changing live directory, first use the Swift
snapshotter to capture each database and its present WAL/SHM with APFS
copy-on-write cloning or its verified read-only byte-copy fallback. Convert the
complete preserved generation with only the source key on standard input and
the independent protector files named explicitly:

```sh
{ cat wechat-key.txt; cat snapshot-passphrase.txt; } | \
  greenbubbles snapshot create-capture \
  <stable-acquisition-snapshot> <new-snapshot-directory> \
  --source-passphrase-stdin \
  --snapshot-recovery-kit /private/greenbubbles-recovery/family-a.txt \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock \
  --snapshot-passphrase-stdin
```

The converter validates the acquisition manifest and every captured file hash,
opens only the captured files read-only, performs direct SQLCipher-to-SQLCipher
logical backup, and validates the entire capture again before publishing.
Incremental acquisition fragments are rejected: conversion requires every
current database set. The manifest labels this
`stableAcquisitionSnapshotConversion`; `crossDatabaseAtomic` remains false for
multi-database sources because a stable capture does not manufacture a shared
transaction across WeChat's independent databases.

## What creation guarantees

Creation inventories current-user-owned regular `.db` files beneath the source
root and requires `contact/contact.db` and `session/session.db`. For each file it:

1. opens the source read-only and enables SQLite `query_only`;
2. creates a new mode-`0600` SQLCipher destination in an owner-only sibling
   staging directory;
3. uses SQLite's online backup API, which copies logical decrypted pages rather
   than WeChat's ciphertext;
4. checkpoints and selects delete-journal mode so no WAL/SHM file is required;
5. closes and reopens the result with only the recovery key;
6. rejects a plaintext `SQLite format 3` header;
7. runs `PRAGMA integrity_check`, hashes the closed file, and records its size
   and page count;
8. synchronizes files and directories and atomically renames the verified
   generation into place.

No plaintext SQLite destination is created. Temporary destination databases are
already SQLCipher-encrypted. A failed pre-publication operation removes its
private staging directory.

An online backup is consistent within one source database. WeChat divides state
across multiple databases, so direct live conversion reports
`perDatabaseOnlineBackup` and `crossDatabaseAtomic: false` when it copies more
than one database. Stable-capture conversion prevents source files from moving
during the longer logical conversion, while preserving the same honest
cross-database atomicity limitation.

## Verify without WeChat

Run verification after copying the snapshot to its intended recovery location:

```sh
greenbubbles snapshot verify <snapshot-directory> \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock

greenbubbles snapshot verify <snapshot-directory> \
  --snapshot-recovery-kit /private/greenbubbles-recovery/family-a.txt

cat snapshot-passphrase.txt | \
  greenbubbles snapshot verify <snapshot-directory> \
  --snapshot-passphrase-stdin
```

Run the local and portable commands at least once. The first proves convenient
local reopening; the second is the portable recovery drill that remains valid
after deleting the local credential, forgetting the passphrase, and losing
every WeChat key. Passphrase verification is an additional drill, not a
substitute for the 24-word drill.

The verifier accepts no WeChat key. It checks:

- owner-only directory and file permissions;
- manifest schema and exact database inventory;
- path confinement and absence of symbolic links;
- closed database files with no required `-wal`, `-shm`, or `-journal` file;
- encrypted rather than plaintext SQLite headers;
- opening every database with the recovery key;
- SQLite integrity, page counts, byte sizes, and SHA-256 hashes.

Success returns a content-free report with
`recoveryVerifiedWithoutWechatKey: true`. Treat that report as evidence from a
specific drill, not a reason to discard the only recovery key.

## Query a snapshot

The same bounded adapter serves live and snapshot data:

```sh
greenbubbles source status <snapshot-directory> \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock

greenbubbles conversations list <snapshot-directory> \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock \
  --limit 100

greenbubbles messages list <snapshot-directory> \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock \
  --conversation <wxid-or-chatroom-id> --limit 100

greenbubbles message get <snapshot-directory> \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock \
  --conversation <wxid-or-chatroom-id> \
  --message <opaque-id-from-messages-list>
```

Use `--snapshot-recovery-kit <file>` instead when performing a portable
recovery drill. Use `--snapshot-passphrase-stdin` for passphrase access. For
search, passphrase mode reads the passphrase line first and the query as the
remaining UTF-8 input; either file-backed mode reads only the query. No
protector-file content or unwrapped key is copied into the pipe.

Responses identify the source mode as `snapshotEncrypted`. No restoration,
canonical JSONL, full replica, or media materialization is required.

### Browse with the native History app

Build and run the local browser:

```sh
swift build --product greenbubbles-history
swift run greenbubbles-history
```

Choose **Create Recoverable Snapshot** to run the word-confirmation ceremony and
conversion. The convenience choices are macOS Keychain, an owner-only hidden
file, or none; an independent 24-word kit is mandatory in every case. Keychain
failure after snapshot publication is non-destructive: the app reports the
failure and the snapshot remains recoverable with its words and optional
passphrase.

To browse, choose **Browse Live or Snapshot**, then select **Snapshot unlock in
macOS Keychain**, **Snapshot hidden-file unlock**, **Snapshot passphrase
(Argon2id)**, or **Snapshot recovery words (portable)**. The UI invokes the same
bounded commands above: it measures database and sidecar storage, loads
100-item keyset pages, and requests an exact message only after a search result
is selected. It never puts a credential or passphrase in process arguments or
preferences. The Keychain credential exists as an owner-only temporary file
only for the open session; the passphrase remains only in memory.

This is a query interface, not a substitute for the recovery drill. Continue to
run `snapshot verify` after creating, copying, or rotating a generation.

## Add or rotate protectors without rewriting SQLCipher

Format-2 protector changes create a new immutable generation while preserving
the existing random database key and every encrypted database byte. This is the
normal way to replace a local credential, rotate recovery words, or add local
convenience to a snapshot that currently has only recovery words:

```sh
cat new-snapshot-passphrase.txt | \
  greenbubbles snapshot rewrap \
  <snapshot-directory> <new-snapshot-directory> \
  --old-snapshot-local-credential /private/greenbubbles-local/.old-unlock \
  --new-snapshot-recovery-kit /private/greenbubbles-recovery/new-family.txt \
  --new-snapshot-local-credential /private/greenbubbles-local/.new-unlock \
  --new-snapshot-passphrase-stdin
```

Use `--old-snapshot-recovery-kit <file>` or
`--old-snapshot-passphrase-stdin` instead if the prior local credential is
unavailable. If both old and new passphrase flags are present, stdin line 1 is
the old passphrase and line 2 is the new passphrase. The new recovery kit is
mandatory, even when the new generation also has a local credential or
passphrase. GreenBubbles fully verifies the source, byte-copies
the already encrypted databases into a new owner-only staging generation,
checks every copied byte against the manifest hash, binds new protectors to a
new snapshot identity, verifies both new unlock paths, and atomically publishes.
The source generation remains untouched. No SQLCipher page is decrypted,
reencrypted, or exposed as plaintext during this operation.

## Legacy raw-key database rekey

Format-1 raw-key rotation creates a new immutable generation; it never rekeys
the only database files in place. Supply the old key first and a distinct new
key second:

```sh
{ cat greenbubbles-recovery-key.txt; cat next-recovery-key.txt; } | \
  greenbubbles snapshot rekey \
  <snapshot-directory> <new-snapshot-directory> \
  --old-snapshot-key-stdin --new-snapshot-key-stdin
```

GreenBubbles first performs the complete source recovery verification with the
old key. It then copies logical pages directly from old-key SQLCipher databases
into new-key SQLCipher databases, creates no plaintext staging database,
verifies the new generation using only the new key, and publishes it atomically.
The old snapshot remains byte-for-byte untouched. Test the new key from its
intended offline recovery location before retiring either the old generation or
its protector.

## Retention and failure boundaries

Each snapshot is an immutable generation. Never edit database or manifest files
in place. Create and recovery-verify a new generation before retiring an old
one. GreenBubbles retention is deliberately a recoverable quarantine operation,
not an age-based delete:

```sh
greenbubbles snapshot retention quarantine \
  <retiring-snapshot> <newer-replacement> <owner-only-quarantine-directory> \
  --retiring-local-credential /private/greenbubbles-local/.old-unlock \
  --replacement-recovery-kit /private/greenbubbles-recovery/new-family.txt
```

Before moving anything, the command fully verifies the retiring generation and
proves the replacement through its portable recovery words. The replacement
must be newer and linked either by parent snapshot identity or stable source
identity. The directories must be distinct and non-nested. GreenBubbles then
renames the entire retiring generation on the same filesystem, fsyncs both
parents, verifies it again at the quarantine location, and automatically rolls
back if that final check fails. A wrong replacement kit leaves the source
untouched.

The retiring generation may instead use
`--retiring-snapshot-passphrase-stdin`, with its passphrase on stdin line 1. The
replacement must still pass the portable recovery-kit drill; local or
passphrase-only replacement verification is intentionally insufficient.

Restore during the cooling period with:

```sh
greenbubbles snapshot retention restore \
  <quarantined-snapshot> <new-restored-directory> \
  --snapshot-recovery-kit /private/greenbubbles-recovery/old-family.txt
```

Restore also accepts `--snapshot-passphrase-stdin` with the passphrase on stdin
line 1, or the local credential file. None of these alternatives weakens the
portable-recovery requirement imposed before quarantine.

GreenBubbles intentionally provides no automatic purge or recursive delete.
Permanent removal remains a separate, explicit owner operation after the
quarantine period, off-device backup check, and another portable recovery drill.

The manifest contains database identities and aggregate sizes but no key,
message body, contact name, or absolute source path. It remains private metadata
and should stay with the protected snapshot.
