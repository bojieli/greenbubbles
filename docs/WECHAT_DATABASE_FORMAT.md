# WeChat database layout and message formats

This document explains what GreenBubbles expects to find in a macOS WeChat
4.1 database, how the CLI discovers it, and how a database row becomes a
message in the CLI output or a restored archive.

It is a compatibility guide, not a promise that every WeChat build has the
same schema. WeChat changes private table names, columns, and encodings. The
parser therefore matches column signatures and aliases, records raw values and
coverage, and reports an explicit gap when it cannot decode something. It does
not guess a missing field.

The descriptions here are based on the database discovery and decoding code in
`Native/GreenBubbles/src/live_query.rs`, `entities.rs`, `restore.rs`,
`model.rs`, and the `wx-db` decoder used by the Rust crate. The tested encrypted
profile and archive rules are described in [Storage format](STORAGE_FORMAT.md).

## Start with the right mental model

There are three layers:

1. **WeChat's files.** WeChat keeps several SQLite databases and sidecar files
   under an account's `db_storage` directory. Some are ordinary SQLite files;
   current message stores are usually WCDB/SQLCipher databases.
2. **GreenBubbles' reader.** The CLI opens those files read-only, applies the
   encrypted SQLite settings when needed, discovers tables and columns, and
   converts rows into a stable query result.
3. **The result.** A live query returns a bounded JSON page. A restoration
   writes a larger, auditable archive with source provenance, raw columns and a
   typed interpretation of each message.

The live query is not an export database. It opens the installed WeChat data
for each command. A snapshot or restored archive is a separate output that you
choose to create.

## Where the files are

A normal live source starts here, with names varying by account and WeChat
build:

```text
<WeChat account directory>/
  db_storage/
    message/
      message_0.db, message_1.db, ...
      biz_message_0.db, ...
      media_0.db, media_1.db, ...
      message_resource.db
      message_fts.db
      weclaw.db
      *.kvdb
      *.db-wal, *.db-shm
    contact/contact.db
    contact/contact_fts.db
    session/session.db
    bizchat/bizchat.db
    chatbot/chatbot_message.db
    emoticon/emoticon.db
    favorite/favorite.db
    general/general.db
    hardlink/hardlink.db
    head_image/head_image.db
    sns/sns.db
    ...                         # other feature databases and MMKV files
```

This is an observed live layout from two readable WeChat accounts on 2026-10-01.
Both accounts had the feature directories shown above. The exact shard counts
and database sizes differed. The live files had opaque first-page headers rather
than a plaintext `SQLite format 3` header, and the database files generally had
active `-wal` and `-shm` sidecars. The inventory also contained WeChat's
`*.material` capture files and MMKV files; these are not SQLite message tables.

The live reader starts from the `db_storage` directory. It requires the
account directory and database directories to be real, owner-readable
locations; it rejects symlinks and unsafe paths. It derives an opaque account
identity from the account directory name. A common `wxid_..._XXXX` suffix is
removed only under the rules implemented in `live_account_holder_source_id`;
content, contact names, and message traffic are never used to identify the
account.

The exact inventory is discovered rather than hard-coded. The live reader scans
numbered `message_<digits>.db` and `biz_message_<digits>.db` files under
`message/`. It also scans `media.db` and `media_<digits>.db` in the top-level
`media/` directory when present, and in `message/` on current WeChat builds.
It sorts the candidates and rejects duplicates, path traversal, non-regular
files, and inventories above the fixed safety limit. A business shard is still
read as a message source; its internal shard identifier is kept in a separate
range so warnings can identify it without colliding with an ordinary shard.

### SQLite sidecars and consistency

A SQLite database may have a `-wal` write-ahead log and a `-shm` shared-memory
sidecar. A live read opens the source read-only. A snapshot acquisition copies
the database and its sidecars as a group; the database identity must remain
stable while the group is captured. The [storage-format guide](STORAGE_FORMAT.md)
describes the atomic copy and encrypted-WAL rules in detail.

A live query reports a consistency object. It tells the caller which source it
opened, how many database shards participated, whether the result is complete,
and when the observation occurred. Results from several live shards are a
consistent query view for that command, not a cross-database transaction.

The inventory check and the query are separate stages. Discovery can find a
root that the query cannot safely open—for example, if the selected account
path is not current-user-owned or fails the no-symlink checks. In that case the
CLI reports an unsafe source instead of weakening the boundary.

## Encryption and opening a database

GreenBubbles supports two storage families:

- **Plain SQLite:** a database whose header begins with `SQLite format 3`.
- **WCDB/SQLCipher 4:** the encrypted WeChat family observed in macOS WeChat
  4.1.x. The profile uses 4096-byte pages, AES-256-CBC, PBKDF2-HMAC-SHA512 with
  256,000 iterations, HMAC-SHA512 authentication, and 80 reserved bytes.

The live reader does not try arbitrary encryption settings. A non-SQLite header
selects the pinned encrypted family and must open with the supplied account key.
The key is supplied through the configured secure input path; it is not placed
in a command argument or written into the query result.

For a snapshot, the reader opens the file read-only with `SQLITE_OPEN_NOFOLLOW`,
sets SQLCipher compatibility 4 and memory security, applies the snapshot key,
and validates the schema before querying. A wrong recovery key fails before any
message is returned.

## How GreenBubbles finds tables

The parser first inventories each database's tables and columns. It uses
case-insensitive column aliases and table signatures rather than assuming one
schema name. This matters because ordinary chats, business accounts, chatbot
conversations, and older WeChat builds do not necessarily use the same names.

Every table is classified as one of these broad roles:

| Role | Meaning |
| --- | --- |
| `message` | A table whose columns are sufficient to interpret message rows. |
| `knownAuxiliary` | A resource, contact, session, group, or other table used to enrich messages. |
| `other` | A table retained in coverage but not used as message content. |
| `unhandledMessageCandidate` | A message-like table that did not meet the supported signature. It remains a coverage gap. |

A table is never silently discarded merely because its name is unfamiliar. The
archive coverage ledger records its logical path, table name, columns, row
counts, role and any limitation. Format-3 and later coverage also fingerprints
the ordered `PRAGMA table_xinfo` metadata and related SQLite schema objects, so
schema drift can be detected without publishing the SQL itself.

## Message table discovery and aliases

The restore path accepts aliases for the fields it needs. The main groups are:

| Logical field | Accepted examples |
| --- | --- |
| Row identity | the first SQLite row value, plus `local_id`, `message_local_id`, `msg_local_id`, `meslocalid` |
| Server identity | `server_id`, `svr_id`, `message_svr_id`, `msg_svr_id`, `msg_server_id`, `messvrid`, `svrid` |
| Ordering | `sort_seq`, `sort_sequence`, `sequence` |
| Type | `local_type`, `message_local_type`, `msg_type`, `message_type`, `type`, `type_` |
| Sender | `real_sender_id`, `sender_id`, `from_id`, `from_user_id`, or a sender-name column |
| Conversation | `talker`, `talker_name`, `chat_name`, `chat_username`, `conversation_id`, `dialogue_id`, `session_id`, `biz_username`, `username`, `user_name`, `chat_id`, `chat_name_id` |
| Time | `create_time`, `message_create_time`, `msg_create_time`, `create_timestamp`, `timestamp`, `timestamp_` |
| Status | `status`, `message_status` |
| Transport and provenance | `origin_source`, `source`, `upload_status`, `download_status`, `server_seq` |
| Direction | `is_sender`, `is_sender_`, `is_send`, `is_sent_by_self` |
| Content | `message_content`, `msg_content`, `content`, `content_`, `message_data`, `msg_data`, `card_wraplist_buffer` |
| Packed metadata | `packed_info_data`, `packed_info`, `message_packed_info` |
| Compression marker | `WCDB_CT_message_content`, `wcdb_ct_message_content`, `compression_type` |
| Source compression marker | `WCDB_CT_source` |
| Optional compressed content | `compress_content`, `compressed_content` |

The exact aliases are implementation details and can expand as compatibility
work adds evidence. A row that lacks the required identity, conversation, type,
or content information is retained as a decode or coverage gap rather than
being turned into a fabricated message.

## What direct SQL confirmed

On 2026-10-01 we opened the encrypted databases in two readable WeChat
accounts directly with SQLCipher, using the account recovery key and the same
key-derivation profile used by GreenBubbles. The verification queried schema
metadata and row counts without selecting message text, contact names, or other
payloads.

The ordinary and business message stores contained tables named `Msg_*`. Their
common live column set was:

```text
local_id, server_id, local_type, sort_seq, real_sender_id,
create_time, status, upload_status, download_status, server_seq,
origin_source, source, message_content, compress_content,
packed_info_data, WCDB_CT_message_content, WCDB_CT_source
```

This confirms that the parser's required fields are present in the current
message stores, while the transport and provenance columns are optional
metadata for the normalized message view. GreenBubbles preserves every source
column in the restoration's `rawColumns` object, even when a column is not
needed to answer a live query.

The same direct inspection confirmed the supporting stores used by the CLI:

- `contact/contact.db` has `contact`, `chat_room`, and `chatroom_member` tables.
- `session/session.db` has `SessionTable` and unread-state tables.
- `general/general.db` has friend-request, red-envelope, transfer, and related
  system tables.
- `hardlink/hardlink.db` has image, video, file, and directory link indexes.
- `message/message_resource.db` and the `media_*.db` files hold attachment
  metadata; they are separate from the `Msg_*` message tables.
- `sns/sns.db`, `favorite/favorite.db`, and `emoticon/emoticon.db` contain
  feature data rather than ordinary chat rows. They are inventoried as
  auxiliary tables and are not silently presented as chat messages.

SQLite full-text search tables also appear in several stores. Names ending in
`_data`, `_idx`, `_content`, `_config`, or `_docsize` are usually FTS support
tables, not independent message sources.

## From a row to a message

The parser preserves the original SQLite values and derives a normalized view.
For a restored `CanonicalMessage`, the important fields are:

- `canonicalId`: GreenBubbles' stable row identity within the restored data;
- `sourceSetId`, `sourceLogicalPath`, `sourceTableId`, `sourceTableName`, and
  `sourceRowId`: the exact provenance needed to audit the row;
- `conversationId` and the base64 encoded source conversation identifier;
- sender identifiers and their source identifier, when available;
- local, server, and sort identifiers;
- the creation timestamp, raw type, logical type and subtype, and status;
- direction plus the evidence used to decide it;
- the original content and packed bytes, encoded for the archive;
- every original column in `rawColumns`;
- a typed payload, semantic decode state and an optional gap reason;
- relationships and artifact references discovered from the row.

The live CLI exposes a smaller result. A message item contains an opaque `id`,
conversation ID, sort sequence, server ID, numeric type and subtype, labels for
those types, sender, local creation time, status, decoded content, and content
decode state. Search results additionally contain the local message ID and a
bounded text snippet.

### Ordering

Rows are ordered using the strongest available source evidence: sort sequence,
then server ID, then creation time, then local ID, with a hybrid fallback when
necessary. The chosen ordering basis is recorded in an archive. A timestamp is
not assumed to be unique and is not used by itself to identify a message.

### Direction and sender

The parser prefers an explicit sender/direction column. When that is absent it
uses the decoded sender, contact tables, the account holder identity, and
legacy direct-chat heuristics. The evidence is recorded as one of:

- `explicitSourceColumn`;
- `senderMatchesAccount` or `senderDiffersFromAccount`;
- legacy direct-chat evidence based on the sender and conversation;
- `senderAccountConflictWithExplicitSourceColumn`;
- `unresolved`.

A sender display name is enrichment, not identity. Opaque participant IDs are
scoped to the account and are kept separate from names.

## Message type and content formats

The stored type is a packed local type. GreenBubbles splits it into a logical
message type and subtype, then asks the `wx-db` decoder to interpret the row.
The live output keeps both numeric values and human-readable labels. Unknown
numeric types remain unknown; they are not relabeled as text.

The content column may be ordinary UTF-8 text, XML-like text, binary packed
metadata, compressed bytes, or a value that the current decoder cannot
interpret. The compression marker applies to the primary content column when
present. An empty optional `compress_content` blob does not override a real
compressed primary content value.

The typed payload can represent, among other cases:

- plain text and system notices;
- images, stickers, voice, video and files;
- links, locations, contacts and app cards;
- quoted, replied-to, recalled, edited and reacted messages;
- merged or forwarded histories;
- channel/Finder content;
- contact events and VoIP-related legacy forms;
- an explicit unknown payload retaining the raw evidence and reason.

The exact numeric type map belongs to the `wx-db` decoder and changes as
WeChat changes. Use the CLI's `messageTypeLabel` and `messageSubtypeLabel`, or
the restored typed payload, rather than building a second hard-coded map from
memory.

### Special legacy forms

`FMessageTable` is treated as a friend/contact event table rather than as an
ordinary chat message table. Several legacy numeric types have named lossless
forms, including contact-card, VoIP and push-mail forms. If the XML is malformed
or the type is unsupported, the result keeps a `LegacyRaw` or `Unknown` payload
and reports the semantic gap.

## Conversations, contacts and groups

A conversation is identified from the row's talker/chat/session value and
scoped to the account. Direct conversations create a participant for the other
side when the source identifier supports that interpretation.

The entity pass also reads auxiliary tables:

- `SessionTable` supplies conversation/session records when it has a username,
  talker or conversation identifier column;
- `chat_room` supplies group conversations, owner information, and membership
  data from its username, owner and extension/member buffer columns;
- contact tables supply display name, remark, nickname, alias and contact kind;
- message rows seed conversations and observed senders even when an auxiliary
  table is incomplete.

Contact kinds in the normalized result are `accountHolder`, `person`, `group`,
`official`, `service`, and `unknown`. A missing contact name does not erase the
conversation or message; it produces an unresolved or raw-only entity state.

## Attachments and media

Message rows usually point to media indirectly through an MD5, local/server ID,
resource ID, or packed metadata. GreenBubbles follows verified auxiliary links
rather than guessing a path. The known chain is:

```text
message row
  -> MessageResourceInfo (local/server ID and packed-info bytes)
  -> MD5 or title metadata
  -> account-scoped message or business media tree

voice row
  -> VoiceInfo (server ID, with local-ID fallback)
  -> Tencent SILK payload
```

An artifact records its kind, role, source database/table/row, local or account-
relative path, size and hashes, detected format, decode state, and availability.
Availability distinguishes downloaded, materialized, not downloaded, remote-only,
expired, deleted, corrupt, ambiguous, metadata-missing, unsafe-path and
account-root-unavailable states.

Image sources may use legacy XOR, fixed-key AES, or per-account-key `.dat`
formats. Voice, video, posters, documents, thumbnails and raw voice blobs are
kept with their provenance. Voice conversion to Ogg Opus is an additional
output; it never replaces the original SILK payload.

The live reading page prints a local `file` path only when the adapter has a
verified local artifact. It does not turn a missing local file into a claim that
the remote media was deleted or expired.

## XML, rich messages and forwarded content

Some message content is XML embedded in `content`, `recorditem`, or `recordxml`
text. Merged histories (`49:19`) and Finder/channel media (`49:51`, `49:63`) are
preserved as raw XML and, when safe, a normalized XML projection.

The normalized tree retains element and attribute names, namespaces, text,
comments and processing instructions. It is a generic projection, not a claim
about an undocumented private schema. DTD processing is disabled, and the
parser bounds each document to 8 MiB, 100,000 nodes and four levels of embedded
XML. Raw XML remains authoritative when parsing fails.

Quoted, merged and related messages become explicit relationships. A
relationship can be resolved, pending, absent locally, missing its identifier,
or ambiguous. A raw reference is retained when available; an unresolved target
is never silently replaced with a different message.

## What the CLI returns

List and search commands return a bounded page. The JSON envelope contains:

- `schema`, `formatVersion`, `operation`, and `ok`;
- `source`, identifying live or snapshot mode and an opaque identity;
- `consistency`, including database count, cross-database atomicity,
  completeness, and observation time;
- `page`, including limit, returned count, `hasMore`, and `nextCursor`;
- `warnings`, with a code, message, optional shard ID and optional count;
- `items`, containing conversations, contacts, messages, or search hits.

The compact reading page intentionally shows less: sender display, whether the
sender is the account holder, local time, text, an optional local file path,
and enough conversation information to continue paging. `--json` is the form
to use when an agent needs IDs, type fields, decode state, or the full envelope.

## What is authoritative, and what is not

The following are source evidence or verified derivations:

- raw SQLite values and raw XML;
- table, column, row and file provenance;
- verified file hashes and schema fingerprints;
- decoded values whose parser reports a supported type;
- explicit availability, decode and coverage states.

The following are interpretations and may be absent:

- display names and contact kinds;
- sender direction when no explicit source flag exists;
- message text for compressed, malformed or unsupported content;
- attachment availability for content that was never downloaded;
- relationships whose target is not present in the local databases.

A complete-looking page is not proof that the whole account was read. Follow
`nextCursor` until `hasMore` is false, inspect warnings and coverage, and treat
an explicit decode gap as evidence that the source format needs more support.

## Compatibility and safe inspection

Use `greenbubbles discover` or the CLI's normal account discovery instead of
copying account paths into prompts. Keep database keys, recovery phrases,
account paths and message data private. Read live files read-only and work on a
snapshot when you need repeatable analysis.

When WeChat changes its private schema, the right response is to add an alias,
a decoder, an auxiliary classification, or a recorded limitation. Do not edit a
live database to make a query work, and do not treat an unrecognized table or
message type as ordinary text.

For the tested encryption profile and capture/archive contracts, see
[Storage format](STORAGE_FORMAT.md). For command syntax and exact output fields,
see the [CLI reference](CLI_REFERENCE.md). For the security boundary around
agents and remote models, see the [threat model](THREAT_MODEL.md).
