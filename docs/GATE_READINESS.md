# Remaining gate and readiness audit

Audit date: 2026-08-27
Plan revision: `PLAN.md` dated 2026-08-27

This audit maps every unchecked plan item to the exact evidence required to
complete it. An implemented synthetic harness is not substituted for real-
client evidence, and an external prerequisite is not silently converted into
permission to operate on a live account.

## Current readiness

| Phase | Current state | Controlling evidence still absent |
| --- | --- | --- |
| 0 | Complete for private development | Repository-owner license selection before any public release. |
| 0.5 | Not passed | Authorized disposable corpus, real-client synchronization evidence, ordinary-contact feasibility evidence, and qualified legal/distribution decisions. |
| 1 | In progress | Real current-version row/type/media completion proof and cross-version/platform discovery evidence. |
| 2 | Implemented, deeply audited, and recovery-drilled on synthetic encrypted replicas | Real disposable-account persistence and 60-second-p95 evidence. |
| 3 | Complete for reads and non-executing drafts | No remaining Phase 3 checkbox. |
| 4 | Blocked | Phase 0.5 and Phase 1 exit gates, a supportable mechanism, a disposable account/conversation, and legal/account-safety approval. |
| 5 | Passive cached reads implemented | Public article access is robots-denied; authenticated reads lack a proven high-level contract and disposable-account approval. |
| 6 | Validated for one MCP host and one resumable consumer | A second source is optional and requires a separate product/repository decision. |

No stable 32-byte WeChat database passphrase, owner-supplied plaintext export,
or disposable-account snapshot is present in the repository. Consequently,
GreenBubbles does not currently claim full real-corpus restoration even though
the row-accounting, unknown-type retention, relationship, multimodal, verified-
path, replica, synchronization, retrieval, and consumer machinery is exercised
by synthetic fixtures.

## Evidence handling rule

Real databases, messages, media, absolute account paths, passphrases, replica
keys, captures, and restored archives must remain outside Git. A database
passphrase must be entered directly through the restoration command's standard
input on the owner's machine; it must not be pasted into an issue, commit, chat,
model prompt, command argument, or readiness document. Only redacted reports,
aggregate measurements, pinned public-build fingerprints, and sanitized
synthetic regressions may become repository evidence.

## Phase 0 and Phase 0.5

| ID | Unchecked plan item | Exact completion evidence | Current disposition |
| --- | --- | --- | --- |
| P0-LICENSE | Select an open-source license before public release. | An explicit repository-owner license choice, a committed license file, and consistency review of dependency and distribution obligations. | The factual dependency inventory and drift check exist, but the project remains `Proprietary`, `publish = false`, and has no owner-selected public license. External owner decision; keep the repository private meanwhile. |
| P05-CORPUS | Confirm useful conversation and attachment data on the pinned current client and document local coverage. | An owner-authorized disposable/test-account snapshot from the exact signed pinned build; a redacted inventory showing conversation shards and media roots; and a restoration coverage report demonstrating actual locally available text plus representative downloaded attachments. | Owner-authorized pinned snapshots now prove that local ordinary/business message shards, message-resource/media stores, contacts, sessions, and cached Moments candidates exist. The latest complete metadata-only pass found 136,786 attachment candidates (38,874,071,097 bytes) in one local root and 87 (1,469,805 bytes) in the other, without traversal issues, symbolic links, or a cap hit. All copied databases are encrypted; content usefulness and message/artifact linkage remain unproven until passphrase-through-stdin restoration produces a redacted coverage report. |
| P05-NONINVASIVE | Determine whether a lawful owner-authorized acquisition route exists, with or without modifying/re-signing WeChat, process attachment, memory scanning, or reusable credential export. | A successful real-corpus restoration from a documented official portable archive, owner-supplied plaintext SQLite, an owner-supplied passphrase-through-stdin snapshot, or the gated owner-authorized `greenbubbles-acquire` capture; the redacted acquisition record must name only that route and show the pinned client state honestly (signed, or owner re-signed for the capture path). | Real passive snapshots and storage preflight succeed without modifying or invoking WeChat, and the passive pipeline's non-invasive guarantees are unchanged. Synthetic plaintext and encrypted/WAL restoration tests pass. A current public-project survey found no documented non-invasive macOS passphrase source—only live debugger/process hooks, memory scanning, client re-signing, already supplied keys, older iTunes/iOS backups, proprietary phone-mediated backup, or a separate official bot relationship. The owner's 2026-08-27 decision lifted the previous blanket prohibition on debugger-based acquisition: the LLDB `CCKeyDerivationPBKDF` capture was validated live on the owner's own machine and account (26/26 databases HMAC-verified on the pinned 4.1.12 build) and is now embedded as the explicitly gated `greenbubbles-acquire` path (`--owner-authorized`, manual owner-run re-sign, pinned-build check, page-1 HMAC proof; see `docs/PASSPHRASE_ACQUISITION.md`). The real encrypted corpus still requires a passphrase-through-stdin restoration; whether that passphrase is owner-supplied directly or produced by the gated capture, full live restoration evidence remains absent. |
| P05-REALSYNC | Prove bootstrap and incremental synchronization on disposable data. | Redacted real-client measurements for idle, one-message, burst, edit, recall, deletion, missed hint, integrity reconciliation, and crash/restart; committed checkpoints must never lead replica state, and new persisted text must meet the 60-second p95 objective. | Synthetic benchmark/fault cases, the audited offline restore/merge/publish transaction, and the continuous monotonic archive follower now pass, including bootstrap, changed/idempotent generations, atomic state, rollback/equivocation denial, replacement-replica binding, commit-before-state crash recovery, recoverable sealed-generation quarantine, and a read-only deep replica audit over canonical hashes/projections, links, FTS, checkpoint/coverage, and sync/change history. Pre-migration backups are independently audited for schemas 1–3, and a non-destructive recovery drill seals the source namespace, creates a separate encrypted current-schema candidate, backfills historical serving state, and requires the deep audit without replacing active state. Snapshot planning/acquisition plus privacy-safe restoration, publication, application, checkpoint, bound sample, and nearest-rank p50/p95 timing schemas are implemented. The composer explicitly records that source-persistence time, inter-command delay, and disposable-scenario attribution remain absent and never claims end-to-end success. A fresh real 25-set bootstrap/incremental pair independently proves exact 8-set content-change classification and 24-entry proportional copying, with 3,362 ms bootstrap and 1,996 ms immediate-incremental acquisition totals; contents stayed encrypted, so message-case attribution and real end-to-end 60-second p95 samples still require passphrase-through-stdin restoration on disposable data. |
| P05-ACTION-MECHANISM | Determine whether a supportable user-authorized text/reply/file mechanism exists for an ordinary disposable contact/group. | A reviewed mechanism decision, exact pinned build, disposable account and allow-listed test peer, visible user-mediated test protocol, and observed official-client results for each independently claimed operation. | No sanctioned or otherwise supportable high-level mechanism is proven. No live attempt is authorized. |
| P05-ACTION-VISIBLE | Require a visible experiment and do not defeat security controls. | A pre-approved test script that keeps the official UI/account warning path visible, records user confirmation, and contains explicit abort conditions for integrity, caller, anti-tamper, or account warnings. | Requires the preceding mechanism, disposable account, and review. |
| P05-ACTION-IDENTITY | Prove recipient resolution, idempotency, and observable state. | Tests and redacted live evidence binding account, conversation, optional reply target, immutable payload, and idempotency key; retries must not duplicate; outcome must be reconciled from official-client state as sent, failed, or unknown. | Blocked with action feasibility. An internal return value can never satisfy this item. |
| P05-ACTION-RISK | Determine version fragility, account risk, maintenance cost, and a fail-closed signal. | A written adapter support matrix, client-update drift test, automatic disable signal, disposable-account risk results, maintenance estimate, and go/no-go decision approved after legal/supportability review. | No adapter has passed the feasibility experiment. |
| P05-LEGAL-ASSESS | Assess source, binary, schema, fixture, and hosted-repository distribution separately. | Qualified-counsel analysis for each artifact class and intended jurisdiction, plus the repository owner's recorded distribution decision. | `DISTRIBUTION_INVENTORY.md` now records the factual category and dependency boundary, including pinned `wx-cli`, SQLCipher, and SILK notice evidence. It is not legal analysis or an owner release decision; private development only. |
| P05-LEGAL-EXCEPTIONS | Determine whether an interoperability, portability, research, or other exception covers each mechanism. | Qualified written advice tied to the exact acquisition/read/action mechanisms, artifacts, and jurisdictions—not a general fair-use assumption. | External legal review required. |
| P05-TENCENT | Explore Tencent permission or a sanctioned portability/action route. | A repository-owner-approved outreach record and Tencent documentation or response; absence of permission must not be described as permission. | External communication is not authorized by the implementation request. |
| P05-RESPONSE | Establish a response plan for updates, complaints, takedowns, security reports, and maintainer/host exposure. | Named owner and counsel/security contacts, intake and preservation rules, immediate private-disable/release-hold procedure, supported-build revocation steps, user notice criteria, and repository/host response procedure approved by the owner. | `OPERATIONAL_RESPONSE_PLAN.md` now supplies the private-development containment, evidence, revocation, exposure, report/complaint, notice, recovery, and approval workflow. It remains a draft: named owners, monitored secure intake, counsel/security approval, jurisdictional decisions, host procedures, and target response times are external and absent. |
| P05-PUBLISHABLE | Document which components and experiments may be published. | A component-by-component publication matrix incorporating the legal assessments, Tencent outcome, dependency licenses, fixture provenance, and an explicit owner release decision. | A preliminary factual matrix distinguishes source, binary, schema, fixture, real-data, hosting, and research categories. It deliberately grants none of them release approval and cannot be finalized before the preceding legal/distribution work. |

## Phase 1 real-source restoration

| ID | Unchecked plan item | Exact completion evidence | Current disposition |
| --- | --- | --- | --- |
| P1-DISCOVERY | Verify root candidates on Intel/Apple Silicon and two desktop versions. | Redacted discovery reports from both architectures and at least two explicitly fingerprinted client versions, using only directory/artifact metadata; synthetic fixtures covering any new root layout. | The installed universal binary proves architectures in the bundle, not runtime root behavior on a second machine/version. |
| P1-ROWS | Enumerate every message-bearing shard/table and prove row accounting. | A current authorized full snapshot whose coverage ledger has no unhandled message-table candidate; for every supported message table, `source rows = restored rows + rejected rows`, with zero rejections and duplicate identities. | Machinery and the independent ledger/count/schema audit are implemented and synthetically tested. Audit-report format 2 now emits the exact combined row-accounting component plus non-empty-corpus and external-attestation limits; an authorized real corpus is still required to discover the actually observed table set. |
| P1-TYPES | Decode every observed logical type and retain unknowns. | The same corpus reports zero unknown observed logical types and zero semantic coverage gaps; each newly observed type has a source-preserving decoder plus sanitized regression fixture. | Unknowns are losslessly retained, block completion, and are independently recounted by `audit-archive`. Merged-history and Finder/channel nested XML now have a bounded, raw-retaining structural projection with audit recomputation and sanitized fixtures. Only a real corpus establishes the observed type and nested-graph universe. |
| P1-MEDIA | Resolve every locally downloaded multimodal/file artifact. | Every attachment reference in the authorized corpus is classified as a verified owner-authorized local file/connector derivative, or explicitly missing, remote-only, expired, deleted, corrupt, or deferred; complete mode requires no deferred, ambiguous, unsafe, or unexplained state and records digest/format/path provenance. | Synthetic multimodal restoration, exact preferred-variant/state auditing, full resource-table provenance, stale/substituted-path rejection, and conversation/time-scoped local connector/MCP path retrieval pass. Audit-report format 2 separately proves media-reference presence, verified local-media presence, artifact verification, decoding, and resolved media phase. Production replica bootstrap reruns the independent audit and changed synchronization artifacts are state/file verified. Real database-to-media linkage still requires passphrase-through-stdin restoration. |

The P1-ROWS, P1-TYPES, and P1-MEDIA items are the controlling proof for the
user's full and faithful conversation-restoration requirement. They must be
evaluated together on the same immutable snapshot. Passing only row accounting
while unknown message types or unresolved media remain does not constitute a
full restoration.

## Phase 4 ordinary-contact actions

All Phase 4 implementation is deliberately blocked until the Phase 0.5
technical, action, and legal/supportability gates and the Phase 1 restoration
exit gate pass. The disposable test account and peer must not be an ordinary
personal contact.

| ID | Unchecked plan item | Exact completion evidence | Current disposition |
| --- | --- | --- | --- |
| P4-REVIEW | Re-evaluate mechanism, version, platform rules, counsel, and account safety. | A dated go decision covering the exact build and mechanism immediately before implementation. | Blocked by Phase 0.5. |
| P4-TEXT | Start with confirmed text to one allow-listed disposable conversation. | One narrowly scoped adapter, default-off capability, immutable preview/approval, and redacted observed-sent/failed/unknown live test evidence. | No send adapter or live attempt. |
| P4-APPROVAL | Bind approval to the exact immutable draft. | Tests proving that account, recipient, reply target, text, attachment, expiry, policy, or connector-version changes invalidate a one-use approval token. | The offline contract binds external approval evidence to the exact gate, capability, draft, account, conversation, adapter, and build and rejects consumed IDs. The state auditor independently recomputes every stored draft, reports policy/version/checkpoint staleness and expiry, and cross-links request/review events. It deliberately cannot issue or consume an approval; adapter-owned authentication and transactional one-use proof remain gated. |
| P4-GUARDS | Enforce idempotency, rates, global kill switch, and version failure outside the model. | Deterministic adapter-bound tests including concurrent retries, restart recovery, rate exhaustion, kill-switch activation, and build drift, with no network/client invocation after denial. | A pure checker now fails closed for each guard input, while exposing no attempt operation. Durable atomic reservation, concurrency/restart tests, and proof of pre-invocation denial still require the selected adapter boundary. |
| P4-LIFECYCLE | Model drafted/approved/attempted/observed-sent/observed-failed/unknown. | Transactional state-machine tests plus live reconciliation evidence; no internal acknowledgement is labeled delivery. | The offline model validates only monotonic drafted/approved/attempted/observed-sent/observed-failed/unknown sequences and has no delivered state. Current request/draft/review audit events are hash-chained and independently verifiable, but the serving process still cannot produce persistent approval/attempt/result transitions; live reconciliation remains gated. |
| P4-RECONCILE | Link official-client result back to replica and audit. | The live test message is independently observed in official-client/local state, deduplicated into the replica, and linked to the immutable action and audit IDs; ambiguous results remain unknown. | Requires safe live text action and real-source synchronization. |
| P4-REPLY | Add reply only when quoted target/result is verifiable. | Exact source target binding, invalidation tests, and observed resulting quote/reply linkage on the disposable conversation. | Later independent capability; not inherited from text send. |
| P4-FILE | Add file send with immutable digest and revalidation. | Allow-listed local file, exact name/type/size/digest preview, descriptor-level revalidation immediately before attempt, retry safety, and observed resulting attachment linkage. | Later independent capability; not inherited from text send. |
| P4-OTHER | Treat images, reactions, cards, membership/mentions, payments, calls, deletions, and Moments mutations separately. | A distinct risk review, capability bit, scope, test protocol, and exit gate for each operation considered. | All unavailable; no broad action capability is inferred. |
| P4-AUTONOMY | Consider narrow autonomous rules only after confirmed actions are reliable. | Strong operational evidence for confirmed text/reply/file, an explicit new owner policy decision, bounded allow-list, independent kill switch/rates, and adversarial safety review. | Not eligible for implementation in the current product phase. |

## Phase 5 optional reads

| ID | Unchecked plan item | Exact completion evidence | Current disposition |
| --- | --- | --- | --- |
| P5-ARTICLE | Parse a public article only when ordinary URL access permits it. | At validation time, the published robots policy must allow the exact `/s` path for the helper user agent, the page must be ordinary unauthenticated HTTPS content without paywall signals, and bounded parsing tests plus one authorized public sample must pass without subresource crawling. | The live 2026-08-27 robots policy says `Disallow: /`; the helper correctly stops before fetching the article. |
| P5-ACTIVE-CONTRACT | Determine whether a high-level authenticated read can reuse the existing client without credential export. | A documented, caller-authorized high-level read-only contract with deterministic account/scope semantics and no injection, attachment, memory access, credential export, re-signing, or control bypass. | Static IPC inventory proves no such contract. |
| P5-ACTIVE-PROTOTYPE | Prototype only on a disposable account and pinned client. | Phase 0.5 approval, disposable account/test content, explicit bounded protocol, and redacted observed read result on the exact build. | No live prototype is authorized. |
| P5-ACTIVE-STOP | Stop on security weakening or inability to fail closed. | The prototype record shows all abort checks, automatic version disable, and either a safe pass or a recorded negative result with no fallback. | Becomes evaluable only if a prototype is approved; any prohibited requirement ends the experiment. |

## Phase 6 optional second source

| ID | Unchecked plan item | Exact completion evidence | Current disposition |
| --- | --- | --- | --- |
| P6-SECOND | If a second source is worth pursuing, implement it separately before extracting shared code. | An explicit product decision naming a valuable source and workflow, a separate private repository, a real connector implementation, and evidence-based comparison against the existing source contract. | Conditional, not a GreenBubbles completion requirement. No source or separate-repository authorization has been selected. |

## Resumption order

When new external evidence becomes available, resume in this order:

1. Keep the repository private and obtain an owner-authorized disposable
   current-version corpus through the official/plaintext/passphrase acquisition
   order in `ACQUISITION_FEASIBILITY.md`.
2. Run one immutable full restoration and close P1-ROWS, P1-TYPES, and P1-MEDIA
   together; add only sanitized regressions for newly observed structures.
3. Prove real-client incremental synchronization and the Phase 2 service
   objectives on that disposable account.
4. Complete the legal/supportability decisions before any active-read or
   ordinary-contact experiment.
5. If approved, perform one visible, allow-listed disposable feasibility test;
   a negative result ends that adapter path.
6. Select a license and publication matrix only when the repository owner is
   ready to consider public distribution.

Until step 1 supplies real authorized evidence, further decoder changes would
be guesses. Until step 4 supplies authority and a supportable mechanism, active
read and action code would weaken the privilege boundaries established by the
plan.
