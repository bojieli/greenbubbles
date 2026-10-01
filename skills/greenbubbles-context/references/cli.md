# GreenBubbles AI CLI

Run any command below with `--help` or `-h` to inspect its exact invocation;
help exits without opening private inputs or reading a key.

## Bounded live or snapshot query

Ordinary live reads need no profile, source path, or access flag. They open
the newest installed WeChat `db_storage` directory and read
`~/.greenbubbles-acquire/passphrase.txt`:

```text
greenbubbles chats --limit 100
greenbubbles chats find "Alice"
greenbubbles messages list --conversation "Alice" --limit 100
greenbubbles messages recent --limit 50
greenbubbles messages search --query "keyword"
```

`messages list`, `messages recent`, and `messages search` print JSON Lines. The header has
`returned`, `hasMore`, `order`, `timezone`, and `nextCursor`. Every message
has `from` (never empty), `self` (`true` or `false`), `at` (local
`YYYY-MM-DD HH:MM`), and `text`. An image, video, or document adds `file`,
a local path you can open. Voice without a transcript stays `[voice]`.
A text body that is only identifiers becomes `[unknown]`. A text body that
is only bracketed emoji names, in English or Chinese, becomes `[emoji]`.
A short recall notice, including one that names who recalled it, becomes
`[revoked]`; the withdrawn words are not on the page. Phone numbers, email
addresses, identity numbers, links, and meeting invitations stay in the
text. `--redact` leaves out a resident identity number, a mainland mobile,
an email address, and an `http` or `https` link, including inside a quote,
a link title, a file name, a transfer note, and `from`, and it collapses a
video-meeting invitation to `[meeting]`. Words around a removed value stay.
A body that is only one of those becomes `[unknown]`. A bare hostname stays
either way.
Search lines name the chat and do not include `file`. `chats rank` prints
the same kind of page: `from`, `id`, `kind`, `selfCount`, and `last`.
Pass `--json` when you need the typed envelope or a message id for
`message get`. `[output] format` in the settings file selects the default.

`chats` is the short form of `conversations list`. Both accept the same
options. A custom database or passphrase path belongs in
`~/.greenbubbles/config.toml` under `[source]`, as `root` and
`passphrase_file`. `[output] format` is `brief` or `json`. Do not put those
paths on every command.

`chats find "name"` returns matching identities with their IDs, display names,
nicknames, remarks, and aliases. It matches partial names case-insensitively,
reads no messages, and asks for a narrower name if matches exceed `--limit`.
`--conversation` on list/search/get accepts IDs and names: exact IDs win,
then exact names, then unique partial names. Ambiguous names fail with IDs on
stderr; select one rather than guessing. Names are shell arguments; use IDs
when a name should stay out of command history.

`messages recent` reads message tables across identifiable chats, including
chats absent from the session list. It does not depend on native FTS. It
orders messages by creation time descending with deterministic ties. Each
reading line has `chat` when known and `conversationId`, plus ordinary message
fields and available attachment paths. `--json` has full IDs and coverage
warnings. Unidentifiable tables are omitted with incomplete coverage, and reads
across databases are not atomic.

Recent defaults to 100 messages (maximum 500); search defaults to 50 hits
(maximum 200). `--since`/`--until` are inclusive Unix seconds. Recent cursors
page older messages and must use the same source and time window. For new
arrivals, poll without a cursor, use an overlapping `--since` window, and
deduplicate IDs from `--json`. This is a query, not a durable watcher or change
feed. Use per-chat reads for a scope limited to selected chats.

For a second account, a snapshot, or an explicit source, choose exactly one
access mode:

```text
--passphrase-stdin                  encrypted live WeChat database root
--snapshot-local-credential <file> ordinary local snapshot reopening
--snapshot-recovery-kit <file>     portable 24-word snapshot recovery
--snapshot-passphrase-stdin        optional Argon2id snapshot passphrase
--snapshot-key-stdin               legacy raw-key snapshot compatibility
--decrypted                         explicit plaintext fixture/export root
```

List conversations, then page only the selected conversation:

```text
greenbubbles conversations list <source-root> \
  --passphrase-stdin --limit 100

greenbubbles messages list <source-root> \
  --passphrase-stdin --conversation <id> --limit 100 [--cursor <token>]

greenbubbles message get <source-root> \
  --passphrase-stdin --conversation <id> --message <opaque-id>
```

The default page is 100 and the hard maximum is 500. There is no `--all`.
Conversation and message cursors use keyset ordering, are bound to the source
and filter, and should be discarded if the CLI rejects them. `message get`
accepts an opaque identity returned by `messages list` or `messages search` for
the same source and conversation.

For ordinary search, pass `--query "text"` as an argument. Use `--query-stdin`
for interactive or piped input instead. Choose exactly one. With `--query`,
stdin is needed only for a credential in an explicit access mode. Keep keys out
of arguments. Both input forms use the same literal, parameterized search.

Search prefers WeChat's compatible native FTS database read-only. When native
FTS is unavailable, it scans a fixed decoded source window without writing an
index; it never silently scans the entire corpus in one request:

```text
{ <key-line>; <query-utf8>; } | \
  greenbubbles messages search <source-root> \
  --passphrase-stdin --query-stdin [--conversation <id>] \
  [--limit 50] [--cursor <token>]
```

For `--snapshot-key-stdin`, the input ordering is likewise snapshot key line,
then query. For `--snapshot-passphrase-stdin`, it is passphrase line, then
query. For `--snapshot-local-credential`, `--snapshot-recovery-kit`, and
`--decrypted`, standard input contains only the query. A protector file path is
an argument, but its contents and the unwrapped database key are not. Query text
must never be a process argument. Search is capped at 200 returned results.
The fallback examines at most 500 source messages and 16 conversations per
response, may return an empty page with `hasMore: true`, and identifies itself
with `fallbackSearchSourceWindowBounded`. Continue until `hasMore` is false.

With `--json`, every success uses `greenbubbles.query.v1`. Check:

- `consistency.guarantee`, `crossDatabaseAtomic`, and `coverageComplete`;
- `warnings`, especially unavailable/incompatible shards,
  `nativeSearchIndexFreshnessUnverified`, and
  `fallbackSearchSourceWindowBounded`, plus
  `contactEnrichmentUnavailable` or `contactDisplayNameUnresolved`;
- `page.hasMore` and `page.nextCursor`.

Live cross-shard reads are statement-consistent per database, not globally
atomic. Use a recoverable snapshot when repeatability across pages matters.
Never infer that an absent row was deleted or never existed when coverage is
incomplete.

Conversation items may carry `displayName`, and message/search items may carry
`senderDisplayName`. Prefer those optional presentation values while preserving
the stable raw `id`/`sender` fields. Enrichment is one bounded read-only contact
batch of at most 500 unique IDs. It ordinarily makes `databaseCount` include
`contact.db` and `crossDatabaseAtomic` false; an unavailable contact database
does not block the primary result.

Lazy attachment access is separate from message-page retrieval. Prefer the
exact message-bound form and select exactly one database access mode:

```text
greenbubbles attachment inspect <account-or-source-root> \
  --conversation <id> --message <opaque-message-id> \
  --kind image|voice|video|document <access-mode>

greenbubbles attachment materialize <account-or-source-root> \
  --conversation <id> --message <opaque-message-id> \
  --kind image|voice|video|document \
  --attachment <opaque-id> --output <new-private-path>
```

The message identity is the exact source-bound ID returned by list/search; never
substitute a server ID, document title, or raw path. Inspection writes nothing.
Materialization creates exactly one owner-only file, refuses overwrite and
output inside the protected source, and returns format/size/SHA-256 without
returning either path. Image input is capped at 128 MiB, voice at 32 MiB per
payload and 128 MiB cumulative/output, video at 2 GiB, document at 512 MiB, and
inspection at 256 candidates, 4,096 directories, and 100,000 entries.

Compatibility-only image access may use `--conversation <id> --md5
<32-hex-md5>` with no database access option. This form does not support voice,
video, or documents.

## Advanced workflows

For policy-scoped direct/replica reads, static exports, and memory ingestion,
read [advanced.md](advanced.md) only when that workflow is requested.
