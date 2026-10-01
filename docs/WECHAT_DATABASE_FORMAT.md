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

## Database-by-database reference

This section names the purpose of each database observed during the direct SQL
check. A database can contain empty tables on one account and populated tables
on another. An empty table is still useful evidence: it tells a reader what
the WeChat build supports even when that feature has not been used.

| Database | What it stores | How GreenBubbles uses it |
| --- | --- | --- |
| `message/message_<n>.db` | Ordinary chat shards. Each `Msg_<conversation-hash>` table stores one conversation's messages. `Name2Id` maps internal conversation IDs to usernames. | Primary live message source. |
| `message/biz_message_<n>.db` | Business-account and business-chat message shards with the same `Msg_<hash>` row shape. | Primary message source when present. |
| `message/media_<n>.db` | Voice and other media metadata, including `VoiceInfo` on builds that provide it. | Attachment lookup; it is not a message store. |
| `message/message_resource.db` | Resource rows connecting a message to media metadata, IDs, hashes, or packed information. | Attachment lookup and provenance. |
| `message/message_fts.db` | Full-text search indexes for messages. | Used only as an index when its schema matches; its shadow tables are not messages. |
| `message/weclaw.db` | WeChat internal state. The inspected account had no usable content tables. | Inventory only unless a supported schema appears. |
| `contact/contact.db` | Contacts, chat rooms, group membership, labels, business contacts, and tickets. | Sender names, participant types, groups, and enrichment. |
| `contact/contact_fts.db` | Contact and chat-room-member search indexes. | Search/enrichment support only. |
| `session/session.db` | Conversation list, drafts, unread counts, and last-message summaries. | Session enrichment and unread/session metadata; never the authoritative message history. |
| `general/general.db` | Friend requests, transfers, red envelopes, recall records, web search, WeApp and Finder state. | Selected system-event enrichment; these tables are not ordinary chat shards. |
| `hardlink/hardlink.db` | MD5-to-file indexes for images, videos, files, and directory IDs. | Resolves account-local attachment paths. |
| `head_image/head_image.db` | Contact avatar blobs keyed by username and MD5. | Avatar or contact-image lookup when requested. |
| `favorite/favorite.db` | Saved/favorite items and favorite tags. | Auxiliary saved-item inventory; not chat history. |
| `favorite/favorite_fts.db` | Full-text index over favorite content. | Favorite search support only. |
| `emoticon/emoticon.db` | Installed and non-store sticker packages, captions, files, and ordering. | Sticker metadata and resource lookup. |
| `sns/sns.db` | Moments timeline, comments, drafts, top items, and timeline break markers. | Cached-surface inventory; it is not treated as ordinary chat history. |
| `solitaire/solitaire.db` | WeChat Solitaire/接龙 content, folds, and validity state. | Auxiliary feature inventory. |
| `bizchat/bizchat.db` | Business-chat groups and users. | Auxiliary business-contact metadata. |
| `chatbot/chatbot_message.db` | Chatbot sessions and chatbot messages. | Auxiliary chatbot inventory; supported only when its message signature is explicitly recognized. |
| `third_app_icon/third_app_icon.db` | Third-party app icon images keyed by app and MD5. | App-card resource enrichment. |
| `MMKV`, `*.kvdb`, `*.material` | WeChat preferences, key/value state, or capture/material files. | Preserved in the inventory where relevant; not parsed as SQLite message tables. |

The names above are the observed roles, not a guarantee that every WeChat
release creates every file. WeChat may add a numeric suffix, create a new
feature database, or move a feature into another store. GreenBubbles records
that change as a schema or coverage difference instead of treating the new
file as an ordinary message database.

### Exact ordinary-message schema

The current live message tables have names of the form
`Msg_<32-lowercase-hex-characters>`. The hash identifies a conversation in
WeChat's internal naming scheme; it is not a username and should not be used as
one. The same row schema was observed in ordinary and business message stores:

| Column | Meaning | Normalized by GreenBubbles |
| --- | --- | --- |
| `local_id` | WeChat's local row identifier. | Local message ID. |
| `server_id` | Server-assigned message identifier. | Server message ID and identity fallback. |
| `local_type` | Packed WeChat message type and subtype. | Split into numeric type/subtype and decoded with `wx-db`. |
| `sort_seq` | WeChat's per-conversation ordering sequence. | Primary ordering field and cursor component. |
| `real_sender_id` | Internal sender identifier for the row. | Sender evidence and contact lookup. |
| `create_time` | Message creation time, represented by WeChat as an integer timestamp. | Local creation time after the source-time conversion. |
| `status` | WeChat row/message state. | Preserved status; exposed in the live item. |
| `upload_status` | Outgoing upload state. | Preserved in raw source columns; not used as delivery proof. |
| `download_status` | Incoming/download state. | Preserved in raw source columns and relevant to media availability. |
| `server_seq` | Server-side ordering or transport sequence. | Preserved as source metadata. |
| `origin_source` | WeChat provenance/source marker. | Preserved as source metadata. |
| `source` | WeChat source/provenance field. | Preserved as source metadata. |
| `message_content` | Main message body: text, XML, packed bytes, or compressed data. | Decoded content and raw bytes. |
| `compress_content` | Optional alternate compressed content blob. | Used only when non-empty and applicable. |
| `packed_info_data` | Packed auxiliary information, commonly used by rich messages and attachments. | Preserved and passed to decoders/resource resolution. |
| `WCDB_CT_message_content` | WCDB compression/storage marker for `message_content`. | Selects the appropriate content interpretation. |
| `WCDB_CT_source` | WCDB compression/storage marker for `source`. | Preserved as source metadata. |

Every `Msg_<hash>` table also has SQLite indexes whose names end in
`_SENDERID`, `_SERVERID`, `_SORTSEQ`, or `_TYPE_SEQ`. Those indexes accelerate
WeChat queries; they are not additional fields or message tables. `Name2Id`
and `SendInfo` are companion tables: `Name2Id.user_name` maps an internal
conversation ID to a username, while `SendInfo` associates a chat-name ID with
a local message ID. `DeleteInfo`, `DeleteResInfo`, `HistoryAddMsgInfo`, and
`HistorySysMsgInfo` track deletion/history operations and are auxiliary state.

### Contact and group schema

The central `contact` table has this exact live column set:

```text
id, username, local_type, alias, encrypt_username, flag, delete_flag,
verify_flag, remark, remark_quan_pin, remark_pin_yin_initial, nick_name,
pin_yin_initial, quan_pin, big_head_url, small_head_url, head_img_md5,
chat_room_notify, is_in_chat_room, description, extra_buffer, chat_room_type
```

| Table | Columns | Meaning |
| --- | --- | --- |
| `contact` | The columns above | One contact or account record. `username` is the stable WeChat-side identifier; names, aliases, remarks, URLs, flags, and extension data are attributes. |
| `stranger` | Same column set as `contact` | Contact-like records not yet in the normal contact set. |
| `chat_room` | `id, username, owner, ext_buffer` | Group identity, group username, owner, and opaque extension data. |
| `chatroom_member` | `room_id, member_id` | Many-to-many group-to-contact relationship. |
| `chat_room_info_detail` | `room_id_, username_, announcement_, announcement_editor_, announcement_publish_time_, chat_room_status_, xml_announcement_, ext_buffer_` | Group announcement and status information. |
| `contact_label` | `label_id_, label_name_, sort_order_` | User-defined contact labels. |
| `biz_info` | `id, username, type, accept_type, child_type, version, external_info, brand_info, brand_icon_url, brand_list, brand_flag, belong, ext_buffer, home_url, sync_version` | Business/official-account metadata. |
| `ticket_info` | `id, ticket` | Contact ticket or verification metadata. |

The `*_pin_yin`, `quan_pin`, and initial columns are search forms of a name;
they are not separate names. `extra_buffer`, `ext_buffer`, and similar blob
columns are opaque WeChat extension data. GreenBubbles retains them when
restoring raw columns but does not invent a public schema for their bytes.

### Session schema

`SessionTable` has this exact live column set:

```text
username, type, unread_count, unread_first_msg_srv_id,
unread_first_pat_msg_local_id, unread_first_pat_msg_sort_seq, is_hidden,
summary, draft, status, last_timestamp, sort_timestamp,
last_clear_unread_timestamp, last_msg_locald_id, last_msg_type,
last_msg_sub_type, last_msg_sender, last_sender_display_name,
last_msg_ext_type
```

`username` identifies the conversation. `unread_count` and the
`unread_first_*` fields describe the unread boundary. `is_hidden`, `status`,
and `draft` describe UI/session state. The `last_*` fields are a cached summary
of the latest message; they can be stale and do not replace the `Msg_<hash>`
row. `SessionUnreadListTable_1` stores `username_id, server_id, create_time,
local_id` for individual unread entries, and `SessionUnreadStatTable_1` stores
`username_id, unread_stat` aggregate counts. `SessionDraft` stores
`username, window_id, timestamp, draft_data`.

### System and feature-table schemas

The following field sets are confirmed by the live SQL inspection. Their
meanings are limited to the feature named by the table; unfamiliar extension
and buffer fields remain opaque.

| Table | Exact columns | Meaning |
| --- | --- | --- |
| `FMessageTable` | `user_name_, type_, timestamp_, encrypt_user_name_, content_, is_sender_, ticket_, scene_, fmessage_detail_buf_, remark_, label_ids_` | Friend/contact event: user, event type/time, content, sender flag, ticket, scene, details, remark, and labels. |
| `redEnvelopeTable` | `message_server_id, session_name, sender_user_name, native_url, send_id, scene_id, hb_status, hb_type, receive_status` | Red-envelope message/payment metadata and state. |
| `transferTable` | `transfer_id, transcation_id, message_server_id, second_message_server_id, session_name, pay_sub_type, pay_receiver, pay_payer, begin_transfer_time, last_modified_time, invalid_time, last_update_time, delay_confirm_flag, bubble_clicked_flag` | Transfer identifiers, participants, timestamps, subtype, and UI/state flags. |
| `revokemessage` | `to_user_name, svr_id, message_type, revoke_time, content, at_user_list` | Recall/revoke information for a message. |
| `SnsTimeLine` | `tid, user_name, content, pack_info_buf` | Moments item ID, author, serialized content, and packed metadata. |
| `SnsMessage_tmp3` | `local_id, create_time, type, feed_id, is_unread, from_username, from_nickname, to_username, to_nickname, content, serialized_comment_buf, serialized_ref_buf, comment_id, client_id, comment64_id, comment_flag, del_status, is_relative_me` | Moments comment/interaction rows and their serialized content/reference fields. |
| `SnsTopItem_1` | `tid, username, summary, create_time, last_read_time, is_read` | Moments top-item/read-state cache. |
| `fav_db_item` | `local_id, server_id, type, update_seq, flag, update_time, version, content, source_id, sync_status, upload_status, fromusr, fromusr_id, realchatname, realchatname_id, ext_buf, upload_error_code, trans_res_status, trans_res_error_code` | A saved item, its content/source, sender/chat context, sync state, and transfer errors. |
| `head_image` | `username, md5, image_buffer, update_time` | Avatar bytes and their content hash. |
| `file_hardlink_info_v4`, `image_hardlink_info_v4`, `video_hardlink_info_v4` | `md5_hash, md5, type, file_name, file_size, modify_time, dir1, dir2, _rowid_, extra_buffer` | Attachment hash, kind, name, size, modification time, directory IDs, and extension data. |
| `VoiceInfo` (when present) | Server/local message ID aliases plus voice-data fields | Voice payload metadata; exact optional columns vary by build and are matched by signature. |
| `kNonStoreEmoticonTable` | `type, md5, caption, product_id, aes_key, thumb_url, tp_url, auth_key, cdn_url, extern_url, extern_md5, encrypt_url, designer_id, activity_id` | Non-store sticker metadata and download/encryption URLs. |
| `kStoreEmoticonPackageTable` | `package_id_, package_name_, payment_status_, download_status_, install_time_, remove_time_, sort_order_, introduction_, full_description_, copyright_, author_, store_icon_url_, panel_url_` | Installed sticker package metadata and lifecycle state. |

The `db_info`, `config`, `buff`, and `table_info` tables use the common
key/value columns `Key, ValueInt64, ValueDouble, ValueStdStr, ValueBlob`.
Their values are database-local settings, not message fields.

The attachment tables observed in the live account had these exact schemas:

| Table | Exact columns | Meaning |
| --- | --- | --- |
| `MessageResourceInfo` | `message_id, chat_id, sender_id, message_local_type, message_create_time, message_local_id, message_svr_id, message_origin_source, packed_info` | Links a message to its resource record using message/chat/sender identity, type, time, local/server IDs, source marker, and packed metadata. |
| `MessageResourceDetail` | `resource_id, message_id, type, size, create_time, access_time, status, data_index, packed_info` | Describes one resource, its size/timestamps/state, data slot, and packed details. |
| `FtsRange` | `session_id, db_time_stamp, start_local_id, end_local_id, range_type` | Resource/full-text range boundaries for a session. |
| `ChatName2Id` and `SenderName2Id` | `user_name, update_time` | Name-to-internal-ID mapping timestamps for resource lookup. |
| `VoiceInfo` in `message/media_<n>.db` | `chat_name_id, create_time, local_id, svr_id, voice_data, data_index` | Voice row keyed by chat and local/server message IDs, with creation time, voice bytes, and a data slot. |

`message_resource.db` also contains `FtsDeleteInfo(session_id,
max_message_id)`, which records an FTS/resource deletion boundary. The many
`*_INDEX` tables in these databases are SQLite indexes over the tables above.

### How to read the schema safely

Use `PRAGMA table_xinfo('<table>')` on a decrypted copy when you need declared
types, nullability, generated-column flags, or hidden-column information.
The field lists above intentionally show names only because SQLite declared
types in WCDB are not reliable descriptions of the payload: a `BLOB` may be
compressed bytes, serialized XML, or an opaque extension buffer. Never query
or publish row values from a real account as part of a schema report. A safe
schema report records the database path relative to `db_storage`, table name,
column names, declared metadata, row count, and a structural fingerprint while
omitting identifiers, names, message content, keys, and binary payloads.

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
