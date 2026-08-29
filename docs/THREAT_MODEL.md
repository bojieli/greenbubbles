# Threat model

GreenBubbles handles some of the most sensitive data a person owns: years of
private conversation, the identities of everyone in them, and a key that opens
all of it. This page states what is being protected, from whom, where the trust
boundaries are, and — importantly — which risks this project does not address
and cannot.

## Assets

| Asset | Why it matters |
| --- | --- |
| The WeChat database key | Opens the entire corpus. Cannot be rotated by you. |
| The live databases | The corpus itself, plus everyone else's messages in it |
| Snapshot recovery material | 24 words that open a snapshot forever |
| The replica key | Opens the serving replica |
| Policy files | Define what a model is allowed to see |
| Audit journals | The only record of what was released |
| Media files | 59 GB of photos, voice and documents in one recorded corpus |

Note the second row carefully. Your history contains other people's words.
They did not choose your threat model, your AI tools, or your disk encryption.

## Trust boundaries

```text
  WeChat's own files ─┐
                      │ read-only, query_only, no writes ever
  GreenBubbles adapter┤
                      │ bounded page, typed operations, no SQL
  Policy + audit ─────┤
                      │ conversations · fields · time · destination
  Your AI tool ───────┘
                        ← everything past here is outside our control
```

Four boundaries, in decreasing order of how much GreenBubbles can promise:

1. **Source → adapter.** Enforced by `SQLITE_OPEN_READ_ONLY`, `PRAGMA
   query_only = ON`, ownership and file-type checks, and no write path in the
   read code at all.
2. **Adapter → caller.** Enforced by typed operations with allowlisted filters,
   hard response caps, and the absence of any `--all` or raw-SQL surface.
3. **Caller → model.** Enforced by policy: account binding, per-conversation
   operations and fields, time range, and an explicit local-versus-remote
   destination decision. Every allow and every deny is journalled.
4. **Model → everything else.** *Not enforced by GreenBubbles.* See below.

## What is defended against

**Accidental bulk disclosure.** There is no operation that returns the corpus.
The largest single response is capped at 8 MiB with 16 KiB per text field, and
those are fixed bounds, not caller-adjustable page sizes.

**Secrets leaking through the process table.** Keys, passphrases, recovery
words, search queries and draft text all arrive on standard input. Process
arguments are world-readable on a shared machine, and shell history outlives the
command.

**Prompt injection from message content.** A caller selects a typed operation;
the connector checks that operation against policy *before* returning data. A
message that instructs an agent to open another conversation, enable a remote
model, or send a reply stays inert text at every layer. The agent cannot reach
the send adapter, and the adapter would refuse a recipient the owner never
approved.

**Silent corruption or partial coverage being read as completeness.** Unknown
tables, undecodable types, skipped shards and unresolved relationships are
reported as gaps, and the top-level verdict stays false. See
[AUDITING.md](AUDITING.md).

**Tampering with the record after the fact.** The connector journal is a hash
chain: each format-2 event hashes the canonical event including its
predecessor's digest. Editing, reordering, insertion, and removal with a
retained successor are detected. Clean suffix truncation is an explicit limit,
described below.

**Unsafe files and paths.** Private inputs must be regular files owned by the
current effective user, without symlinks, without multiple links, and without
group or world permissions. A restrictive mode on a file owned by *another*
account is not accepted as owner authorization. Descriptors are opened
`O_NOFOLLOW`/`O_CLOEXEC`, and media candidates are re-verified before and after
each read.

**Losing access to your own backup.** A snapshot is re-encrypted under its own
random key, wrapped by portable 24-word recovery material. `snapshot verify`
proves recoverability by opening it with no WeChat key at all. Removing the
last portable protector is forbidden.

## What is not defended against, and cannot be

**Everything downstream of the boundary.** If the model you send a page to is
remote, that page is now on someone else's infrastructure. The same applies to
an embedder, a vector store, a log collector, a crash reporter, or an agent
framework that keeps transcripts. GreenBubbles controls what leaves its own
process; approving the destination is entirely your decision, and the
`destination: remote` flag exists to make it a deliberate one.

**A compromised machine.** If an attacker has your user account, they have your
key material, your policies and your replica. Filesystem permissions are a
correctness boundary against mistakes, not a defence against local root.

**A malicious owner.** The audit chain uses unkeyed hashes. Someone who can
rewrite the whole journal can recompute all of them, and without an external
anchor a verifier cannot know that a valid final suffix was removed. This is
honest tamper evidence, not attestation.

**The other people in your history.** Nothing here gives them a say in what you
release about them. That is a judgment you make, and it deserves more thought
than the technology can encode. A conversation policy scoped to the threads you
actually need is the mechanism; using it is on you.

**Changes you make to your own client.** Key acquisition re-signs WeChat ad
hoc, which replaces Apple's signature until you reinstall or the app updates.
That is a change to your machine's software state, and GreenBubbles reports it
honestly rather than pretending the client is pristine afterwards.

**Traffic analysis of WeChat itself.** GreenBubbles never contacts WeChat's
servers, so it neither creates nor conceals anything visible to them. The
acquisition helper's logout and login *is* visible to WeChat.

## Non-goals

Stated so they are not mistaken for gaps:

- no stealth, anti-detection, or evasion of any kind;
- no account takeover, credential theft, or access-control bypass;
- no reading of accounts or data the operator is not authorized for;
- no anonymity; GreenBubbles is not a privacy network;
- no bot accounts, no private WeChat network APIs, no code injection into
  WeChat;
- no cloud component, no telemetry, no background upload service.

## Reporting

Security issues go to the private path in [SECURITY.md](../SECURITY.md), never
to a public issue. Do not attach a database key, recovery words, real message
content, real identifiers, or absolute paths to any report — a structural
description is both safer and more useful.
