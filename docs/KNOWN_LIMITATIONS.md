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

**Generated memory is an inference, not a canonical record.**
`ai-summarize-direct` validates account attribution, citation aliases,
conversation scope, response structure and bounded coverage, but a supported
citation does not mechanically prove that every nuance of a model-written
sentence is faithful. Gemini output can vary across runs and still needs human
review. Each run creates a new immutable generation; automatic semantic merge,
conflict resolution and promotion of an older inferred wiki are not built.

**A canonical personal-memory corpus can cover every eligible message, but what
the agent writes from it is still an inference.** A v2 `allMessages` preparation
no longer omits inactive months or silent sessions. It scans every inventoried
hashed message table, including tables whose conversation identity cannot be
reversed; `rowCoverageComplete` can therefore be true while
`sourceCoverageComplete` is false and `unmatchedMessageTable` remains reported.
Rows with undecodable metadata or content remain explicit coverage failures.
Per-message text still obeys `maximumMessageTextBytes`, attachments and
unsupported payloads are represented by compact summaries rather than every
source byte, and `tr=true` marks text that reached that bound. A completed
unfiltered scope proves that the agent reviewed every hydrated corpus message,
not that the wiki or the UserAsCode project it wrote captured every nuance.
Citation checks cannot prove semantic faithfulness, so human review and the
coverage report remain required. Legacy v1 corpora keep the old selective
account-holder-active behavior and cannot establish whole-database review.

**The UserAsCode knowledge project is agent-written, and one tick at a time.**
Each `tick` asks an agent to diff new facts against existing domain state and
patch in place. Nothing mechanically proves it classified a fact into the right
domain, noticed that an incoming fact contradicts a stored one, or avoided
writing a duplicate in different words; `git diff` and the format's own tests
are review aids, not proofs. Python constraints execute deterministically, but
only over state the agent chose to record, and only once the agent has written
the constraint. Shards do not run concurrently over one project — `tick` caps
`--parallel` at 1 because concurrent agents raced on shared domain files — so
extraction throughput is one agent, however many shards are requested. The
Markdown format has no executable constraints at all; its alerts are notes.

**Preparation is atomic but not resumable mid-scan.** An interrupted
`memory prepare` leaves no published partial corpus and must restart. Once the
corpus exists, `memory next/page/acknowledge/commit` is crash-safe and repeats
an unacknowledged page exactly. A future preparation checkpoint format would
need to bind live source mutation across restarts before it could safely
resume. For incremental extraction after new messages arrive, use
`memory prepare --extend` — it re-scans metadata fully but hydrates only new
rows, so interrupting it is safe (re-run and it starts fresh). The UserAsCode
knowledge project is updated incrementally per batch; no full re-extraction is
required.

**A prepared corpus is a point-in-time live generation, not a database lock.**
GreenBubbles verifies row identity between metadata selection and hydration and
publishes one internally bound immutable generation. Messages arriving before
a later preparation may make that later corpus larger; counts from separate
preparations must not be combined. Prepare a new corpus when a refreshed
history is required, then review or promote it explicitly.

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
