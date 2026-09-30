# Recoverable snapshots (backups)

A plain copy of WeChat's files is not a real backup. It can only be opened with
WeChat's key, and that key lives inside an app you don't control. If WeChat
changes, you lose the account, or the key stops working, the copy can't be
opened.

A GreenBubbles snapshot solves this. It is encrypted again with its own key,
which you can always recover from 24 words you keep somewhere safe. Checking a
snapshot opens it with **no WeChat key at all**, which proves you could recover
it without WeChat.

This page covers the command line. For the app, see the
[user guide](USER_GUIDE.md).

## Back up in four steps

1. **Create a recovery phrase first.** Make a private folder and create the
   recovery kit, a file holding 24 words:

   ```sh
   umask 077
   mkdir -m 700 -p /private/greenbubbles-recovery
   greenbubbles snapshot recovery-kit create \
     /private/greenbubbles-recovery/family-a.txt
   ```

2. **Copy the 24 words somewhere else.** Open the file, copy the words to a
   password manager, an encrypted USB drive, or another place you control, then
   check the copy you'll rely on:

   ```sh
   greenbubbles snapshot recovery-kit validate <your-copy-of-the-kit>
   ```

3. **Optionally, create an unlock file for this Mac,** so you don't need the
   words every time you open the snapshot:

   ```sh
   mkdir -m 700 -p /private/greenbubbles-local
   greenbubbles snapshot local-credential create \
     /private/greenbubbles-local/.family-a-unlock
   ```

4. **Create the snapshot.** Send WeChat's key on stdin:

   ```sh
   cat wechat-key.txt | \
     greenbubbles snapshot create \
     <WeChat-db_storage-folder> <new-snapshot-folder> \
     --source-passphrase-stdin \
     --snapshot-recovery-kit /private/greenbubbles-recovery/family-a.txt \
     --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock
   ```

   The parent of `<new-snapshot-folder>` must already exist and be private to
   you, and `<new-snapshot-folder>` itself must not exist yet.

Then [check the backup](#check-a-backup-without-wechat) from where you'll keep
it.

## Check a backup without WeChat

Run a check after the snapshot reaches the place you'll keep it, not just
where you made it:

```sh
# Check with the recovery phrase. This is the check that matters.
greenbubbles snapshot verify <snapshot-folder> \
  --snapshot-recovery-kit /private/greenbubbles-recovery/family-a.txt

# Check that this Mac's unlock file works.
greenbubbles snapshot verify <snapshot-folder> \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock

# If you added a passphrase, check it too.
cat snapshot-passphrase.txt | \
  greenbubbles snapshot verify <snapshot-folder> --snapshot-passphrase-stdin
```

The recovery-phrase check is the real recovery drill. It still works after you
delete the unlock file, forget the passphrase, and lose every WeChat key. The
other checks are extras, not substitutes.

The check takes no WeChat key. It confirms:

- files are private to you, with no symbolic links or files outside the
  snapshot folder;
- the manifest (the snapshot's index file) and its list of databases are exact;
- every database is closed, with no leftover `-wal`, `-shm`, or `-journal`
  file, and is encrypted, not plaintext;
- every database opens with the recovery key and passes SQLite's integrity
  check, with the expected page count, size, and SHA-256 hash.

A pass prints a report with `recoveryVerifiedWithoutWechatKey: true`. That
proves this one drill worked. It is not a reason to delete your only other
copy.

## Restore or browse a backup

You don't need to unpack a snapshot to use it. The normal read commands work on
it directly:

```sh
greenbubbles source status <snapshot-folder> \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock

greenbubbles conversations list <snapshot-folder> \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock \
  --limit 100

greenbubbles messages list <snapshot-folder> \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock \
  --conversation <wxid-or-chatroom-id> --limit 100

greenbubbles message get <snapshot-folder> \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock \
  --conversation <wxid-or-chatroom-id> --message <opaque-id>
```

- On a new Mac, or without the unlock file, use
  `--snapshot-recovery-kit <file>` instead.
- To use a passphrase, use `--snapshot-passphrase-stdin`. For a search, the
  passphrase is stdin line 1 and the search text is the rest. With a file-based
  option, stdin holds only the search text.
- Responses report the source as `snapshotEncrypted`.

In the app, choose **Browse Live or Snapshot…**. It runs these same commands,
and never puts a credential or passphrase in a command argument or in its
settings.

Browsing isn't a recovery drill. Keep running `snapshot verify`.

## The recovery phrase

The 24 words are generated for you from 256 random bits and include a
checksum. Don't change them or swap in words that are easier to remember; the
checksum will fail.

The words don't encrypt the database directly. They unlock a separate random
database key. That's why you can add or change ways to unlock a snapshot
without decrypting or rewriting the database.

The words use the BIP-39 word list because it's well reviewed and has a
checksum. **A GreenBubbles recovery phrase is not a cryptocurrency wallet
seed.** Never reuse a wallet phrase here, and never import a GreenBubbles
phrase into a wallet.

Make the recovery kit *before* the snapshot, as in step 1. Creating a snapshot
can take a long time, and this way the words already exist even if it's
cancelled or fails.

`recovery-kit create` writes the file with mode `0600`, checks its checksum,
and makes sure it's saved to disk. It prints a report, but never the words.

Don't commit the kit, paste it into an issue or an AI prompt, put it in a
command argument, or keep it next to the only copy of the snapshot.

The app does the same thing step by step. It creates the kit, shows all 24
words once, then asks you for four randomly chosen words. It won't start until
your answers match and you confirm you've saved a copy elsewhere. The words,
your answers, the WeChat key, and any passphrase are cleared from the screen as
soon as the backup starts.

## Other ways to unlock

These are conveniences. **None of them replace the 24 words.** GreenBubbles
won't create a backup that can only be opened on one Mac or only with a
passphrase, and it won't let you remove the last recovery phrase.

**Unlock file for this Mac.** `snapshot local-credential create` makes a
private file (mode `0600`, owned by you, a single hard link) holding a random
unlock credential. It contains neither the database key nor the 24 words.
Deleting it only turns off convenient unlocking.

In the app, the same credential is stored in your macOS Keychain instead. It's
tied to that snapshot and marked `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`,
so it never syncs to other devices. When the app opens the snapshot, it writes
the credential to a temporary private file, passes only that file's path to the
command-line tool, and deletes it when the snapshot closes. If you choose a
hidden unlock file instead, the app remembers its path, never its contents.

**Passphrase.** You can also add a passphrase of 12 to 1,024 bytes (one line
of UTF-8). It's protected with Argon2id v1.3 (64 MiB, time cost 3, parallelism
1) and XChaCha20-Poly1305. The app never saves it. To add one when creating a
snapshot, add `--snapshot-passphrase-stdin` and put the passphrase on the line
after WeChat's key:

```sh
{ cat wechat-key.txt; cat snapshot-passphrase.txt; } | \
  greenbubbles snapshot create \
  <WeChat-db_storage-folder> <new-snapshot-folder> \
  --source-passphrase-stdin \
  --snapshot-recovery-kit /private/greenbubbles-recovery/family-a.txt \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock \
  --snapshot-passphrase-stdin
```

Every credential file must be owned by you, mode `0600`, a single hard link,
and inside a folder only you can open.

## Change how a snapshot unlocks

To replace the unlock file, create a new recovery phrase, or add a passphrase, use
`snapshot rewrap`. It writes a new snapshot folder with the new unlock methods
and leaves the old one untouched:

```sh
cat new-snapshot-passphrase.txt | \
  greenbubbles snapshot rewrap \
  <snapshot-folder> <new-snapshot-folder> \
  --old-snapshot-local-credential /private/greenbubbles-local/.old-unlock \
  --new-snapshot-recovery-kit /private/greenbubbles-recovery/new-family.txt \
  --new-snapshot-local-credential /private/greenbubbles-local/.new-unlock \
  --new-snapshot-passphrase-stdin
```

- If the old unlock file is gone, use `--old-snapshot-recovery-kit` or
  `--old-snapshot-passphrase-stdin` instead.
- With both old and new passphrase options, stdin line 1 is the old passphrase
  and line 2 is the new one.
- A new recovery kit is always required.

The database key and every encrypted byte stay the same; nothing is decrypted
or re-encrypted. GreenBubbles checks the old snapshot fully, copies the
encrypted databases into a private temporary folder, checks every byte against
the manifest, gives the copy a new snapshot identity with the new unlock
methods, tests both new ways to unlock, and only then moves it into place in a
single step.

### Older snapshots with a raw key

Snapshots in the older format 1 are unlocked by a raw 256-bit key instead of
24 words. They still work. To change the key, use `snapshot rekey`, which also
writes a new folder and never changes files in place:

```sh
{ cat greenbubbles-recovery-key.txt; cat next-recovery-key.txt; } | \
  greenbubbles snapshot rekey \
  <snapshot-folder> <new-snapshot-folder> \
  --old-snapshot-key-stdin --new-snapshot-key-stdin
```

It checks the old snapshot with the old key, copies the data directly into
databases encrypted with the new key (never writing plaintext), checks the
result with only the new key, and moves it into place. Test the new key from
where you'll keep it before retiring anything. New snapshots use format 2, with
recovery phrase.

## Retire an old backup

Never edit a snapshot's databases or manifest. To replace a backup, create and
check a new one, then retire the old one.

Retiring moves the old snapshot into a quarantine folder instead of deleting
it:

```sh
greenbubbles snapshot retention quarantine \
  <old-snapshot> <newer-replacement> <private-quarantine-folder> \
  --retiring-local-credential /private/greenbubbles-local/.old-unlock \
  --replacement-recovery-kit /private/greenbubbles-recovery/new-family.txt
```

- The old snapshot can be unlocked with `--retiring-local-credential`,
  `--retiring-recovery-kit`, or `--retiring-snapshot-passphrase-stdin`.
- The replacement must pass the check with its **recovery phrase**. An unlock
  file or passphrase isn't enough, because the point is to prove you can
  recover after losing this Mac.
- The replacement must be newer and linked to the old one by its parent
  snapshot identity or the same source. The folders must be different and not
  inside each other.

Before moving anything, it checks both snapshots fully. It then moves the old
snapshot on the same disk, saves the change to disk, and checks it again in
the quarantine folder. If that last check fails, the move is undone. With the
wrong replacement kit, nothing is moved.

To bring a quarantined snapshot back:

```sh
greenbubbles snapshot retention restore \
  <quarantined-snapshot> <new-restored-folder> \
  --snapshot-recovery-kit /private/greenbubbles-recovery/old-family.txt
```

The retention commands never permanently delete a finished snapshot. Deleting
one is your decision. Do it only after the quarantine period, after checking
your off-device copy, and after one more recovery-phrase drill. Half-finished
temporary folders from a failed run may still be cleaned up automatically.

## Advanced: create from a saved copy of WeChat's files

The word "snapshot" means two different things in GreenBubbles:

| | Acquisition snapshot | Recoverable snapshot |
| --- | --- | --- |
| What it is | a short-lived copy of WeChat's `.db`, WAL, and SHM files, taken at one moment | a backup of the databases, encrypted again with a GreenBubbles key |
| Encrypted with | WeChat's key | its own new random key |
| Good for | exact evidence and offline restoration | lasting backups and repeated queries |
| Survives losing WeChat | no | yes |

They work together. If you don't want the backup to read from WeChat's live
folder while WeChat is changing it, first take an acquisition snapshot with the
Swift snapshotter. It uses APFS copy-on-write cloning, or a checked read-only
byte copy as a fallback. Then convert that saved copy:

```sh
cat wechat-key.txt | \
  greenbubbles snapshot create-capture \
  <acquisition-snapshot> <new-snapshot-folder> \
  --source-passphrase-stdin \
  --snapshot-recovery-kit /private/greenbubbles-recovery/family-a.txt \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock
```

The converter checks the saved copy's manifest and every file hash, opens only
those files, read-only, and copies directly from one encrypted database to
another, so no plaintext is ever written. It checks the whole copy again before
finishing. Partial, incremental copies are rejected; every current database
must be present. The manifest labels the result
`stableAcquisitionSnapshotConversion`.

`crossDatabaseAtomic` is still `false`. A saved copy stops files from changing
during conversion, but WeChat's databases were never written in one shared
transaction, so the manifest doesn't claim they're consistent with each other.

For a plaintext source, such as test data, use `--source-decrypted`. No WeChat
key is read, so a passphrase, if you add one, goes on stdin line 1:

```sh
cat snapshot-passphrase.txt | \
  greenbubbles snapshot create \
  <plaintext-db_storage-folder> <new-snapshot-folder> \
  --source-decrypted \
  --snapshot-recovery-kit /private/greenbubbles-recovery/family-a.txt \
  --snapshot-local-credential /private/greenbubbles-local/.family-a-unlock \
  --snapshot-passphrase-stdin
```

## How creating a snapshot works

GreenBubbles finds every `.db` file you own under the source folder, and
requires `contact/contact.db` and `session/session.db`. For each file it:

1. opens the source read-only, with `query_only` on;
2. creates a new encrypted SQLCipher database, mode `0600`, in a private
   temporary folder beside the destination;
3. copies the decrypted data with SQLite's online backup API (not WeChat's
   encrypted bytes);
4. finishes any pending writes and switches to delete-journal mode, so no WAL
   or SHM file is needed;
5. closes the result and reopens it using **only** the recovery key;
6. rejects the file if it has a plaintext `SQLite format 3` header;
7. runs `PRAGMA integrity_check`, hashes the closed file, and records its size
   and page count;
8. saves files and folders to disk, then moves the finished snapshot into place
   in a single step.

No unencrypted database is ever created; even the temporary copies are
encrypted. If it fails before the last step, it deletes its temporary folder.

An online backup is consistent within one database. So a backup made directly
from WeChat's live folder reports `perDatabaseOnlineBackup` and
`crossDatabaseAtomic: false`.

The manifest records database identities and total sizes. It contains no key,
no message text, no contact names, and no full source paths. It is still
private, and belongs with the snapshot it describes.
