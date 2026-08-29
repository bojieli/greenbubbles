# Known limitations

Everything on this page is a real constraint, not a caveat added for form. Read
it before deciding this belongs on your machine. Where a limitation has a
number attached, the number is in [MEASUREMENTS.md](MEASUREMENTS.md).

## The ones that decide whether this is for you

**Getting the database key needs root and re-signing your own WeChat.**
GreenBubbles cannot open encrypted history without the matching 32-byte key,
and it contains no decryption bypass. The bundled helper captures it from your
own running client, which requires root for the debugger attach and an ad-hoc
re-sign that replaces Apple's signature on WeChat until you reinstall or it
auto-updates. It is a one-minute step and it is documented end to end in
[PASSPHRASE_ACQUISITION.md](PASSPHRASE_ACQUISITION.md) — but if you cannot run
as root on this machine, you cannot complete setup.

**macOS only, Apple silicon only in released form.** macOS 14 or later. The
published binaries are arm64; Intel is unbuilt and unverified. There is no
Windows, Linux, Android or iOS support and none is planned.

**WeChat 4.1+ only, and the format is closed.** The compatibility profile
tracks a signed, current client. An update can change decoding at any time.
Unknown or partially readable data is reported as a limitation and the
completion verdict stays false — but "reported as a gap" still means you did
not get that message.

**This is a research alpha.** It is for technical users who can read a JSON
envelope and decide whether its coverage claims are good enough for what they
are doing.

## Evidence that does not exist

**The 60-second latency objective has never been measured.** The goal — text
newly persisted by WeChat becomes searchable within 60 seconds at p95, on a
real account — has no supporting run. The tooling deliberately refuses to
assemble one from the partial evidence it can produce: every latency sample
sets `fullEndToEndObjectiveProven` to `false`, and accumulating samples cannot
change it.

**No performance number comes from a live account under load.** Every timing is
a synthetic benchmark or a bounded local sample on one M2 Max. Static corpus
sizes are real; nothing that moves has been measured against a real client
writing to the databases concurrently.

**Restoration completeness is proven per archive, not in general.** The auditor
proves an archive is internally consistent and its recorded media files still
exist. It cannot prove an undiscovered WeChat table was absent, that a private
field's semantics were interpreted correctly, or that synthetic nested-tag
fixtures cover every real merged-message or Finder variant. Closing that needs
one real compatible-version corpus with zero unhandled tables and an explicit
state for every media reference, which does not exist yet.

**The snapshot protector has not had an external cryptographic review.** The
construction uses standard maintained primitives — BIP-39 entropy, HKDF-SHA-256,
Argon2id, XChaCha20-Poly1305 — and invents nothing. It has still not been
reviewed by anyone outside this project.

## Things that work, with edges

**Search is slow when WeChat's own index cannot be used.** The zero-write
fallback decrypts a fixed 500-message window: ~246 ms p95 for one conversation,
~352 ms p95 across 16. That is the deliberate trade against writing a second
encrypted copy of your messages to disk.

**A page across shards is not one atomic instant.** WeChat splits history
across databases, so a message page touching four of them is four statements.
Responses report `crossDatabaseAtomic: false` rather than pretending otherwise.
If you need a stable multi-page or cross-database view, query a snapshot
generation instead of the live source.

**Contact names can fail to resolve.** Enrichment reads at most 500 unique IDs
per request from `contact.db`. Missing rows or an incompatible contact schema
emit `contactDisplayNameUnresolved` or `contactEnrichmentUnavailable`, keep the
raw identifier, and mark enrichment incomplete. They never fail the message
read.

**Video and document lookup is heuristic.** It consults `hardlink.db` first,
then falls back to a fixed-depth conversation-scoped filesystem scan using the
decoded MD5 and, for documents, a title basename. A renamed or moved file may
not be found.

**A database-only snapshot does not contain your media.** It can resolve
database-resident voice payloads. It does not claim to hold external image,
video or document files unless a future snapshot format inventories them
explicitly.

**Audit chains are tamper-evident, not signed.** The connector journal hashes
each event with its predecessor's digest. That detects editing, reordering,
insertion and removal when a retained successor remains. It cannot detect a
cleanly removed final suffix without an external anchor, or defeat an owner who
rewrites the whole journal and recomputes every unkeyed hash. Real action
accountability would need independent signing or anchoring, which is not built.

**Retention never deletes, which means it never reclaims space either.**
Retired archives are quarantined by atomic same-filesystem rename. Permanent
deletion is a separate manual decision, and once taken that generation cannot
be restored.

## Deliberately absent

**Sending.** Experimental code exists. A default build carries no pinned
release verification key, so no rollout stage above `dryRun` can open, and the
guard denies while any gate-evidence flag is false. Two things are outstanding
by design: a qualified mechanism, legal and account-safety decision for an
exact client build, and a provisioned release signing key. Sending is not a
supported public feature and is never reachable from an AI tool call. See
[SEND_ADAPTER.md](SEND_ADAPTER.md).

**Anything that touches WeChat's servers.** No network calls, no private API
use, no bot account, no active reads. The read path does not inject code into
WeChat.

**Notification subscription.** macOS provides no supported way to subscribe to
another application's notification bodies. Filesystem events can wake the
reconciler as a *hint*; consistent snapshots and canonical-ID reconciliation
remain the only authority, and periodic reconciliation recovers missed hints.
`greenbubbles-discover notification-hints` reports whether Accessibility trust
exists and always reports completeness as false.

**Moments are cached, not live.** The cached-Moments scope is passive
observation with its own policy dimension. It grants no active read.

## Reporting one

If you find a message type, table or relationship GreenBubbles reads
incompletely, that is the most useful bug report this project can receive.
Describe it structurally — type codes, table shapes, counts — and attach no
message content, no identifiers, and no paths. See
[CONTRIBUTING.md](../CONTRIBUTING.md).
