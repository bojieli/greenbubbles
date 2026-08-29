# Recoverable snapshots

A copy of WeChat's encrypted files is not a backup. It still needs WeChat's
key, and that key lives inside a running application you do not control. If the
application changes, the account is lost, or the key becomes unavailable, an
otherwise intact copy becomes an unopenable pile of bytes.

A GreenBubbles snapshot is designed against exactly that failure. It is
re-encrypted under a key of its own, recoverable from 24 words you hold
somewhere else, and its verification step proves this by opening it with **no
WeChat key at all**.

This is the operator reference. For the graphical walkthrough, start with the
[user guide](USER_GUIDE.md).

## Two different things called "snapshot"

| | Acquisition snapshot | Recoverable snapshot |
| --- | --- | --- |
| What it is | a short-lived consistent filesystem capture of WeChat's `.db`, WAL and SHM | a logical SQLite backup re-encrypted under a GreenBubbles key |
| Encrypted with | WeChat's key | a fresh random key of its own |
| Good for | forensic restoration, exact source evidence | durable backup, repeatable queries |
| Survives losing WeChat | no | yes |

They compose. Acquire a stable filesystem capture first, then convert that
complete capture with `snapshot create-capture` — no plaintext at any point in
between.

## The recovery model

The portable protector is a 24-word English BIP-39 mnemonic generated from 256
random bits. The words encode recovery entropy plus a checksum; they are
generated, not chosen, and must not be edited into something memorable. That
entropy wraps a *separate* random SQLCipher database key — which is why adding
or changing protectors never requires decrypting a database or rewriting its
pages.

BIP-39 is used here only as a well-reviewed, checksummed human encoding.
**A GreenBubbles recovery phrase is not a wallet seed.** Never reuse a wallet
phrase here, and never import a GreenBubbles phrase into a wallet.

Create the kit in an owner-only directory *before* creating the snapshot:

```sh
umask 077
mkdir -m 700 -p /private/greenbubbles-recovery
greenbubbles snapshot recovery-kit create \
  /private/greenbubbles-recovery/family-a.txt
```

The CLI creates the file exclusively with mode `0600`, validates its BIP-39
checksum, fsyncs it, and prints a content-free JSON report. It does not print
the words. Read the file yourself, copy the words to an independent location —
password manager, encrypted removable medium, another owner-controlled recovery
system — and run `snapshot recovery-kit validate` against the copy you intend
to rely on before you rely on it.

The ordering here is deliberate: creation accepts an already-created kit, so
the words exist before a potentially long database conversion starts and
survive a cancelled or failed run.

Do not commit the kit, paste it into an issue or a model prompt, put it in a
command argument, or store it beside the only copy of the snapshot.

The History app performs this as a ceremony: it creates the owner-only kit
first, shows all 24 words once, asks for four randomly chosen word positions,
and refuses to begin conversion until the answers match and you confirm an
independent copy exists. Displayed words, answers, the WeChat key and any
snapshot passphrase are cleared from view state the moment conversion begins.

### Convenience protectors

A local-unlock credential lets the same account reopen a snapshot without
reading the words every time:

```sh
greenbubbles snapshot local-credential create \
  /private/greenbubbles-local/.family-a-unlock
```

This mode-`0600`, single-link, current-user-owned file holds a distinct random
wrapping credential. It contains neither the database key nor the 24 words; the
manifest holds only its public identifier and an authenticated wrapped key.
Deleting it disables convenient local unlock and nothing else.

On macOS the graphical flow instead stores that same random credential as a
generic-password Keychain item scoped to the snapshot identity and marked
`kSecAttrAccessibleWhenUnlockedThisDeviceOnly` — not synchronized to any other
device. When opening the snapshot, the app writes the credential into a new
mode-`0600` file in an owner-only temporary session directory, passes only that
path to the CLI, and removes the directory when the source closes. The
hidden-file option remains as a cross-platform fallback; the app remembers its
path, never its contents.

An optional passphrase can wrap the same database key using Argon2id v1.3
(64 MiB, time cost 3, parallelism 1) and XChaCha20-Poly1305 with authenticated
snapshot, protector and KDF metadata. The app never persists it; the CLI accepts
one UTF-8 line of 12–1,024 bytes.

**None of these replace the words.** GreenBubbles refuses to create a
device-local-only or passphrase-only backup, and removing the last portable
protector is forbidden. A convenience credential is a convenience.

Format-1 snapshots protected directly by a raw 256-bit key remain supported for
compatibility; new work uses the format-2 hierarchy.

## Create from an encrypted source

Standard input carries the 32-byte WeChat key, and — when enabled — the
snapshot passphrase on the next line:

```sh
{ cat wechat-key.txt; cat snapshot-passphrase.txt; } | \
  greenbubbles snapshot create \
  <WeChat-db_storage-root> <new-snapshot-directory> \
  --source-passphrase-stdin \
  --snapshot-recovery-kit /private/greenbubbles-recovery/family-a.txt \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock \
  --snapshot-passphrase-stdin
```

Every credential file must be a current-user-owned, mode-`0600`, single-link
file inside an owner-only directory. The output parent must already be an
owner-only real directory, and the output path must not exist.

For an explicitly plaintext source no source key is read, so the passphrase (if
enabled) becomes stdin line 1:

```sh
cat snapshot-passphrase.txt | \
  greenbubbles snapshot create \
  <plaintext-db_storage-root> <new-snapshot-directory> \
  --source-decrypted \
  --snapshot-recovery-kit /private/greenbubbles-recovery/family-a.txt \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock \
  --snapshot-passphrase-stdin
```

## Create from a stable capture

When conversion should not span a changing live directory, capture first with
the Swift snapshotter — APFS copy-on-write cloning, or its verified read-only
byte-copy fallback — then convert the preserved generation:

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
opens only captured files read-only, performs a direct
SQLCipher-to-SQLCipher logical backup, and validates the whole capture again
before publishing. Incremental fragments are rejected: conversion requires every
current database set. The manifest labels the result
`stableAcquisitionSnapshotConversion`.

`crossDatabaseAtomic` stays false even here. A stable capture stops files from
moving during conversion; it does not manufacture a shared transaction across
WeChat's independent databases, and claiming otherwise would be a lie the
manifest is not willing to tell.

## What creation actually does

Creation inventories current-user-owned regular `.db` files beneath the source
root and requires `contact/contact.db` and `session/session.db`. For each file:

1. open the source read-only with `query_only` enabled;
2. create a new mode-`0600` SQLCipher destination in an owner-only staging
   sibling;
3. copy with SQLite's online backup API — logical decrypted pages, not WeChat's
   ciphertext;
4. checkpoint and select delete-journal mode so no WAL or SHM sidecar is
   required;
5. close and reopen the result using **only** the recovery key;
6. reject a plaintext `SQLite format 3` header;
7. run `PRAGMA integrity_check`, hash the closed file, record size and page
   count;
8. fsync files and directories, then atomically rename the verified generation
   into place.

No plaintext SQLite destination is ever created; the temporary destinations are
already SQLCipher-encrypted. A failure before publication removes its private
staging directory.

An online backup is consistent within one source database. Direct live
conversion across several databases therefore reports `perDatabaseOnlineBackup`
and `crossDatabaseAtomic: false`.

## Verify without WeChat

Run this after the snapshot reaches its intended recovery location, not just
where it was made:

```sh
greenbubbles snapshot verify <snapshot-directory> \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock

greenbubbles snapshot verify <snapshot-directory> \
  --snapshot-recovery-kit /private/greenbubbles-recovery/family-a.txt

cat snapshot-passphrase.txt | \
  greenbubbles snapshot verify <snapshot-directory> \
  --snapshot-passphrase-stdin
```

Run at least the first two. The local one proves convenient reopening; **the
portable one is the actual drill** — it stays valid after you delete the local
credential, forget the passphrase, and lose every WeChat key. Passphrase
verification is an extra drill, not a substitute.

The verifier accepts no WeChat key and checks owner-only permissions; manifest
schema and exact database inventory; path confinement and absence of symlinks;
closed database files needing no `-wal`, `-shm` or `-journal`; encrypted rather
than plaintext headers; opening every database with the recovery key; and
SQLite integrity, page counts, byte sizes and SHA-256 hashes.

Success returns a content-free report with
`recoveryVerifiedWithoutWechatKey: true`. Treat that as evidence from one
specific drill — not as a reason to discard your only recovery copy.

## Query a snapshot

The same bounded adapter serves live and snapshot sources:

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
  --conversation <wxid-or-chatroom-id> --message <opaque-id>
```

Substitute `--snapshot-recovery-kit <file>` for a portable drill, or
`--snapshot-passphrase-stdin` for passphrase access. For search in passphrase
mode, the passphrase is stdin line 1 and the query is the remaining UTF-8
input; file-backed modes read only the query. No protector content or unwrapped
key is ever copied into the pipe.

Responses report the source mode as `snapshotEncrypted`. No restoration,
canonical JSONL, replica or media materialization is involved.

The History app runs these same commands: it measures storage, loads 100-item
keyset pages, and requests an exact message only after a search hit is
selected. It never puts a credential or passphrase in process arguments or
preferences. Keychain failure *after* publication is non-destructive — the app
reports it and the snapshot remains recoverable from its words.

Browsing is a query interface, not a drill. Keep running `snapshot verify`.

## Rotate protectors without touching ciphertext

A protector change creates a new immutable generation while preserving the
existing random database key and every encrypted byte. This is how you replace
a local credential, rotate recovery words, or add convenience to a
words-only snapshot:

```sh
cat new-snapshot-passphrase.txt | \
  greenbubbles snapshot rewrap \
  <snapshot-directory> <new-snapshot-directory> \
  --old-snapshot-local-credential /private/greenbubbles-local/.old-unlock \
  --new-snapshot-recovery-kit /private/greenbubbles-recovery/new-family.txt \
  --new-snapshot-local-credential /private/greenbubbles-local/.new-unlock \
  --new-snapshot-passphrase-stdin
```

Use `--old-snapshot-recovery-kit` or `--old-snapshot-passphrase-stdin` if the
old local credential is gone. With both old and new passphrase flags present,
stdin line 1 is the old and line 2 is the new. A new recovery kit is mandatory
regardless of what else the new generation has.

GreenBubbles verifies the source fully, byte-copies the already-encrypted
databases into a new owner-only staging generation, checks every copied byte
against the manifest hash, binds the new protectors to a new snapshot identity,
verifies both new unlock paths, and publishes atomically. The source generation
is untouched, and no SQLCipher page is decrypted or re-encrypted.

### Legacy raw-key rekey

Format-1 rotation also creates a new generation and never rekeys files in
place:

```sh
{ cat greenbubbles-recovery-key.txt; cat next-recovery-key.txt; } | \
  greenbubbles snapshot rekey \
  <snapshot-directory> <new-snapshot-directory> \
  --old-snapshot-key-stdin --new-snapshot-key-stdin
```

It fully verifies the source with the old key, copies logical pages directly
between old-key and new-key SQLCipher databases with no plaintext staging,
verifies the new generation with only the new key, and publishes atomically.
Test the new key from its intended offline location before retiring anything.

## Retention: quarantine, never delete

Each snapshot is an immutable generation. Never edit a database or manifest in
place. Create and verify a new generation before retiring an old one.

Retention is deliberately a recoverable quarantine, not an age-based delete:

```sh
greenbubbles snapshot retention quarantine \
  <retiring-snapshot> <newer-replacement> <owner-only-quarantine-directory> \
  --retiring-local-credential /private/greenbubbles-local/.old-unlock \
  --replacement-recovery-kit /private/greenbubbles-recovery/new-family.txt
```

Before moving anything it fully verifies the retiring generation *and* proves
the replacement through its portable words. The replacement must be newer and
linked by parent snapshot identity or stable source identity; the directories
must be distinct and non-nested. It then renames the whole generation on the
same filesystem, fsyncs both parents, verifies it again at the quarantine
location, and rolls back automatically if that final check fails. A wrong
replacement kit leaves the source untouched.

The retiring generation may use `--retiring-snapshot-passphrase-stdin` instead.
The replacement must still pass the *portable* drill — local or
passphrase-only verification of a replacement is intentionally insufficient,
because the whole point is proving you can still recover after losing this
machine.

To bring one back during the cooling period:

```sh
greenbubbles snapshot retention restore \
  <quarantined-snapshot> <new-restored-directory> \
  --snapshot-recovery-kit /private/greenbubbles-recovery/old-family.txt
```

The retention commands never purge or recursively delete a completed
generation. Permanent removal is a separate explicit operator decision, and
the right time for it is after the quarantine period, an off-device backup
check, and one more portable recovery drill. Failed unpublished staging
generations may still be cleaned up automatically.

The manifest holds database identities and aggregate sizes — no key, no message
body, no contact name, no absolute source path. It is private metadata and
belongs with the protected snapshot.
