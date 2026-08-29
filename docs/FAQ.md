# FAQ

The questions people actually ask, including the ones whose honest answer is
"that is a real limitation."

## Getting it working

### I do not have the database key. Can I use this at all?

You capture it. That is the normal first step, and it takes about a minute:
re-sign your own copy of WeChat so a debugger can attach, run
`greenbubbles-acquire preflight`, then run `greenbubbles-acquire capture` and
log out of WeChat and back in. The helper reads the key as WeChat derives it,
verifies it against every database, and writes it to an owner-only file.
Walkthrough in the [README](../README.md#getting-your-database-key), full
detail in [PASSPHRASE_ACQUISITION.md](PASSPHRASE_ACQUISITION.md).

GreenBubbles has no decryption bypass and will not grow one — the helper reads
the key from your own client, it does not break the encryption. If you would
rather not run it, the other routes in are a snapshot someone already made you,
or a plaintext source.

### What exactly do I select as the source?

The directory literally named `db_storage`, containing at least `contact`,
`session` and `message`. On a current client it is under:

```text
~/Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files/<account>/db_storage
```

Not the WeChat container, not `xwechat_files`, and not an individual `.db`
file. If you do not know which account directory is yours:

```sh
swift run greenbubbles-discover accounts --include-paths
```

Path-bearing output contains a stable account identifier. Keep it private.

### It authenticated, but a database will not open

Individual shards can fail while the source as a whole works. A page across
shards will still return, but it names the skipped shard by opaque ID and marks
coverage incomplete — GreenBubbles will not silently drop it. If a *required*
database (contact, session, message) fails, the operation fails outright.

### How do I check it actually works against my real databases?

There is a developer sanity check that runs the bounded CLI against your own
live sources and emits a content-free report:

```sh
swift scripts/check-live-database.swift
```

It discovers readable accounts, tries your key against each, and for every
source it authenticates checks source status, a bounded conversation page,
message lookup across up to 20 real conversations, exact hydration of a list
identity and a search identity, and cursor continuation. It never accepts a
fixture database and is deliberately not a CI job — it needs your installed
storage and your key. Output is one JSON report with no paths, IDs, queries or
content in it, safe to paste into an issue.

## Behaviour that looks like a bug

### Why is search sometimes slow?

When WeChat's own full-text index is unusable, GreenBubbles falls back to
decrypting a fixed 500-message window and scanning it — about 246 ms p95 for
one conversation and 352 ms p95 across sixteen. That is the deliberate cost of
*not* maintaining a second encrypted copy of your messages on disk. The
alternative was a persistent text cache, and 350 ms did not justify one. See
[MEASUREMENTS.md](MEASUREMENTS.md).

### Search returned nothing, but I know the message exists

Three different causes:

1. **The fallback window has not reached it.** The fallback examines at most
   500 messages and 16 conversations per response. An empty window is *not* the
   end of the search — the response carries a continuation cursor, and
   GreenBubbles never claims an empty window means "no results" while one
   remains. Follow the cursor.
2. **The native index is stale.** WeChat maintains its own FTS. When it is used
   and its freshness could not be verified, the response says
   `nativeSearchIndexFreshnessUnverified`.
3. **The message is not local.** History that lives only on WeChat's servers,
   or only on your phone, is not on this Mac and nothing here can reach it.

### Contact names show as `wxid_…` instead of people

Name enrichment reads at most 500 unique IDs per request from `contact.db`. If
a row is missing or the contact schema is a variant that could not be read, the
response emits `contactDisplayNameUnresolved` or `contactEnrichmentUnavailable`,
keeps the raw identifier, and marks enrichment incomplete. The message read
itself never fails over a name. Group labels come from the group contact, never
from the last person who spoke.

### Two pages disagree, or a response says `crossDatabaseAtomic: false`

That is accurate reporting, not a fault. WeChat splits history across several
databases; a page touching four of them is four separate statements and cannot
be one atomic instant. If you need a stable view across pages or databases,
create a snapshot and query that generation instead of the live source.

### Does a long query interfere with WeChat?

No. Every statement is finalized before anything is serialized, so a read
transaction is never held open while a caller or a model is thinking, and
WeChat's WAL checkpointing is never pinned. This is why there is no operation
that streams the corpus.

## Safety

### Is my database key ever sent to a model?

No. Keys, passphrases, recovery words, search queries and draft text all arrive
on standard input and never appear in process arguments, responses, logs,
errors, manifests or cursors. Keys are zeroized after use. Nothing in the AI
tool boundary can request one.

### Can an AI read all my chats?

Only what a policy you wrote permits. Policy binds one account and grants each
conversation an independent set of operations, message fields, an optional time
range, and a local-versus-remote destination decision. Remote release is off
unless you explicitly enable it for that conversation. Everything allowed and
everything denied is appended to a hash-chained, body-free journal.

Without a policy, the CLI runs with your own filesystem authority — which is
appropriate for you at a terminal, and is exactly why the AI surface uses a
policy instead.

### What if a message in my history tells the AI to do something?

Nothing happens. A caller selects a typed operation, and that operation is
checked against policy *before* any body is returned. A message asking an agent
to open another conversation, enable a remote model, or send a reply stays
inert text. The agent cannot reach the send adapter at all, and the adapter
would refuse a recipient you never approved.

### Does anything get uploaded?

No. There is no network client in the read path, no background service, no
telemetry, and no cloud component. The important caveat is what happens *after*
GreenBubbles hands over a page: if your model, embedder, vector store, log
collector or crash reporter is remote, that page is now remote too.
GreenBubbles controls its own boundary and marks a remote destination
explicitly; approving it is your call.

### Can WeChat tell I am doing this?

The read path never contacts WeChat's servers, injects code, or calls private
APIs, so it produces nothing for them to observe. The optional acquisition
helper is different: it re-signs the client and requires a logout and login,
and a logout and login is obviously visible. See
[THREAT_MODEL.md](THREAT_MODEL.md).

### What about the other people in my conversations?

They are in your history and they did not choose your tooling. Nothing
technical can resolve that for you. What GreenBubbles gives you is the ability
to scope a policy to the threads you actually need rather than releasing
everything — using it is a judgment call, and worth making deliberately.

## Backups and recovery

### Is copying `db_storage` a backup?

No, and this is the most expensive mistake available here. Those files are
encrypted with WeChat's key, which lives in a running application and can become
unavailable to you. A copy of them is a backup only for as long as you still
have that key.

A GreenBubbles snapshot is re-encrypted under a fresh random key wrapped by 24
portable recovery words. `snapshot verify` proves it by opening the snapshot
with **no WeChat key at all**:

```sh
greenbubbles snapshot verify <snapshot-directory> \
  --snapshot-recovery-kit <owner-only-recovery-kit-file>
```

### I lost my Keychain entry / hidden credential file

Open the same snapshot with the recovery-kit file instead, then create a new
protector generation. Do not try to recreate the lost credential by guessing —
it was a random key, not something derived from a password.

### I lost the 24 words

If the Keychain entry or hidden credential still exists on the machine that
created the snapshot, open it with that and immediately create new recovery
material. If both are gone, the snapshot is unrecoverable. That is what the
encryption is for.

This is also why removing the last portable protector is forbidden, and why the
recovery kit is written *before* the long conversion begins rather than after.

### Where should I keep the recovery words?

Anywhere that is not beside the only copy of the snapshot. A backup needs an
intact snapshot generation *and* one working portable recovery copy, in
different places. Never reuse a cryptocurrency wallet phrase.

## Scope

### Can I send messages?

No. Experimental code exists and public builds ship cryptographically closed: a
default build has no pinned release verification key, so no rollout stage above
`dryRun` can open. It is not reachable from any AI tool call, and opening it is
an operator decision blocked on legal and account-safety questions rather than
on code. See [SEND_ADAPTER.md](SEND_ADAPTER.md).

### Windows? Linux? Android? iOS?

None, and none planned. macOS 14+ on Apple silicon for released binaries.

### Will it survive a WeChat update?

Sometimes. The compatibility profile tracks a signed 4.1+ client, and an
ordinary update does not break an acquisition chain — but the format is closed
and changes without notice. When decoding breaks, GreenBubbles reports gaps and
keeps the completion verdict false rather than guessing.

### Multiple WeChat accounts?

Discovery finds every readable account. Each has its own key, its own policies
and its own snapshots; a policy for one account is rejected for another rather
than reinterpreted. Run per-account commands separately.

### How much disk does this need?

Bounded queries need effectively none — that is the point of the architecture.
A snapshot is roughly the size of the source databases. An explicit full
restoration is the expensive path: in one recorded run, a 2.98 GB source
produced a ~13.5 GB text archive with a ~7.4 GB staging peak, and eager media
added roughly 30 GB. Numbers in [MEASUREMENTS.md](MEASUREMENTS.md).

### Something is wrong that is not listed here

Check [KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md) first — it may be a known
constraint rather than a bug. If it is a bug, describe it structurally with no
message content, identifiers or absolute paths. Security issues go to the
private path in [SECURITY.md](../SECURITY.md), never a public issue.
