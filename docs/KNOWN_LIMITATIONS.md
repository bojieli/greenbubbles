# Known limitations

Everything here is a real limit. Read it before deciding whether GreenBubbles
belongs on your Mac. Where a limit has a number, the measurement is in
[MEASUREMENTS.md](MEASUREMENTS.md).

## Check these before you install

**You need administrator access, and you must re-sign WeChat.** GreenBubbles
can't read your chats without your account's key, and it has no way around the
encryption. It copies the key from your own running WeChat, which needs `sudo`
and a local re-signing of the WeChat app. If you can't use `sudo` on this Mac,
you can't finish setup. See the [key setup guide](PASSPHRASE_ACQUISITION.md).

**macOS 14 or later, on Apple silicon.** Released builds are Apple silicon only;
Intel Macs aren't built or tested. There's no Windows, Linux, Android, or iOS
version, and none is planned.

**WeChat 4.1 or later only, and its format is private.** A WeChat update can
change how data is stored at any time. When GreenBubbles meets data it can't
read, it tells you and marks the results incomplete, but you still don't get
that message.

**This is a research alpha.** It's for technical users who are comfortable
reading JSON output and judging whether "complete" and "incomplete" markers
are good enough for what they need.

## Everyday limits

**Search can be slow.** When WeChat's own search index can't be used,
GreenBubbles searches the most recent 500 messages directly: about a quarter of
a second for one chat, about a third of a second across 16. That's the price of
not keeping a second copy of your messages on disk.

**Results from several databases aren't taken at one instant.** WeChat splits
your history across several database files, and GreenBubbles reads them one
after another. The output says `crossDatabaseAtomic: false` to be honest about
this. For a view that never changes between pages, read a
[backup](RECOVERABLE_SNAPSHOTS.md) instead of live data.

**Some names may not show.** GreenBubbles looks up up to 500 contact names per
command. If it can't find a name, it shows the raw ID (like `wxid_…`) and says
`contactDisplayNameUnresolved` or `contactEnrichmentUnavailable`. The messages
still appear.

**Finding videos and documents is best-effort.** GreenBubbles checks WeChat's
file index first, then searches the chat's folders by file fingerprint (and,
for documents, by file name). A file you renamed or moved may not be found.

**A backup doesn't include photos, videos, or documents.** It holds WeChat's
databases, including voice messages stored inside them, but not image, video,
or document files stored separately.

**Old backups are never deleted automatically, so they keep using disk space.**
When you retire a backup, it's moved to a quarantine folder. Deleting it for
good is a separate step you take yourself, and it can't be undone.

## Limits of AI-written notes

**AI summaries can be wrong.** The built-in summarizer
(`ai-summarize-direct`) checks that each summary cites real messages from
allowed chats, but a correct citation doesn't prove the sentence is a faithful
reading. Gemini can give different results on different runs, so review what
it writes. Each run writes a new, separate summary; merging runs or resolving
conflicts between them isn't built.

**Personal-memory notes are also the agent's interpretation.** The personal
memory workflow can give the agent every message in your history, but that
proves only that the agent read them all, not that its notes captured
everything correctly. Things to know:

- Long messages are cut at a size limit and marked `tr=true`. Attachments are
  shown as short descriptions, not their full contents.
- Some message tables can't be matched to a chat. The coverage report then
  says `rowCoverageComplete: true` but `sourceCoverageComplete: false`, and
  lists `unmatchedMessageTable`.
- Messages GreenBubbles can't decode are listed as coverage failures.
- Collections prepared with the older version 1 format include only chats where
  you were active, so they can't show that your whole history was reviewed.

Always review the notes and the coverage report.

**The agent updates notes one step at a time, and nothing checks its
judgment.** Each run asks the agent to compare new facts with your existing
notes and edit them. Nothing proves it filed a fact in the right place, noticed
a contradiction, or avoided writing the same fact twice in different words.
`git diff` and the format's tests help you review, but they aren't proof.
Python rules run reliably, but only on what the agent chose to record. The
Markdown format has no runnable rules; its alerts are just notes. Only one
agent works on a notes project at a time (`tick` limits `--parallel` to 1),
because agents running together overwrote each other's edits.

**Preparing messages can't pause and resume.** If `memory prepare` is
interrupted, nothing half-finished is saved; run it again from the start. After
preparation, reading pages is crash-safe: an unfinished page is shown again
exactly. To add new messages later, use `memory prepare --extend`, which only
loads what's new and is safe to interrupt and rerun. Your notes are updated
batch by batch, with no need to start over.

**Prepared messages are a snapshot in time.** A later preparation can include
messages that arrived since, so don't add up counts from different
preparations. Prepare again when you want newer history.

## Not yet proven

**Search freshness hasn't been measured.** The goal is for a new WeChat
message to be searchable within 60 seconds, 95% of the time, on a real
account. No test has shown this yet, and the tools refuse to claim it from
partial evidence: every sample reports `fullEndToEndObjectiveProven: false`.

**No timing comes from a real account in active use.** Every timing is a
synthetic benchmark or a small local sample on one M2 Max Mac. Data sizes are
real, but nothing was measured while WeChat was writing at the same time.

**A full restore is checked only against itself.** The checker proves a
restored archive is internally consistent and its media files exist. It can't
prove there wasn't a WeChat table GreenBubbles doesn't know about, or that
every private field was understood correctly. That would need a real,
compatible history with no unhandled tables, which doesn't exist yet.

**Backup encryption hasn't had an outside review.** It uses standard,
maintained building blocks (BIP-39, HKDF-SHA-256, Argon2id,
XChaCha20-Poly1305) and invents nothing new, but nobody outside the project has
reviewed it.

**Audit logs can show tampering, but they aren't signed.** Each entry includes a
fingerprint of the one before it, so editing, reordering, adding, or removing an
entry in the middle is detected. Removing the newest entries cleanly isn't
detected, and someone who controls your Mac could rewrite the whole log. Fixing
that would need independent signing, which isn't built.

## Left out on purpose

**Sending messages.** Experimental code exists, but public builds lock it to
dry runs only. Unlocking it needs a legal and account-safety decision for a
specific WeChat version, plus a release signing key, and neither exists. No AI
tool can reach it. See [SEND_ADAPTER.md](SEND_ADAPTER.md).

**Anything that contacts WeChat's servers.** No network calls to WeChat, no
private interfaces, no bot accounts, and no injecting code into WeChat.

**Live notifications.** macOS doesn't let one app read another app's
notifications. GreenBubbles can use file changes as a hint that something new
arrived, but it always confirms by rereading the databases.
`greenbubbles-discover notification-hints` shows whether Accessibility access is
granted, and always reports the hints as incomplete.

**Live Moments.** GreenBubbles can read Moments that WeChat has already saved on
your Mac, under its own policy setting. It can't fetch new ones.

## Found a gap?

If GreenBubbles reads some message type, table, or relationship incompletely,
that's the most useful bug report you can send. Describe it by its structure
(type codes, table layout, counts), with no message content, IDs, or paths. See
[CONTRIBUTING.md](../CONTRIBUTING.md).
