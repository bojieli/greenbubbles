# Privacy

GreenBubbles collects nothing about you. It has no telemetry, analytics, crash
reporting, update check, or license check, and nothing about your use reaches
the project author.

Reading, searching, backups, exports, and note-building never use the network.
The one command that sends your messages anywhere is `ai-summarize-direct`,
and only when you run it (see [below](#what-can-leave-your-mac)).

The rest of this page explains what stays on your Mac, what can leave it, and
who decides.

## What lives on your Mac

| What | Where | What it exposes |
| --- | --- | --- |
| WeChat's own databases | where WeChat put them; GreenBubbles never changes them | your whole history |
| Backups (snapshots) | wherever you created them | your whole history, encrypted again |
| Database key | `~/.greenbubbles-acquire/passphrase.txt` or wherever you moved it | opens everything; it cannot be changed |
| Recovery phrase (24 words) | wherever you keep it | opens a backup, forever |
| Replica key | wherever you stored it | opens a replica (a local copy of your history kept for faster queries) |
| Query profiles | `~/.greenbubbles/` | file paths, not secrets, but still revealing |
| Audit logs | wherever you configured them | which operations ran and how many items; no message text |
| Progress reports | wherever you wrote them | totals, sometimes database paths |
| Built-in summarizer output | wherever you wrote it | message text, AI summaries, and links back to source messages |
| Personal-memory corpus | wherever you wrote it | possibly a copy of every included message, with contact details |
| Personal-memory notes and progress | wherever you wrote them | facts about you and others, relationships, and citations |

GreenBubbles only reads WeChat's files and never writes to them. Every private
file it creates can be read only by your account (mode `0600`, in a `0700`
folder). It refuses to use a file that other accounts can read, that is a
symbolic link, or that another account owns.

A finished personal-memory corpus is the exception: its files are made
read-only (`0400`, folders `0500`) so an agent cannot accidentally change the
evidence while it edits your notes. The notes themselves stay writable.

## What can leave your Mac

Messages leave only when **you** run a command and choose where its output goes.

- **A local AI model** gets only what your policy file allows: specific chats,
  specific fields, a specific time range.
- **A cloud AI model** gets nothing unless you turn on cloud access for that
  specific chat. The AI cannot turn this on for itself.
- **`ai-summarize-direct`** sends chats to Google's Gemini API itself, but only
  chats your policy marks `allowRemoteModel`. It sends short stand-in names and
  each message's sender, time, type, and text. Real message IDs, sender IDs,
  database details, and your policy and audit files stay on your Mac. It reads
  `GEMINI_API_KEY` from the environment, never from the command line.
- **File paths** on your Mac are never given to a cloud model, even for chats
  where cloud access is on.
- **The personal-memory commands** (`memory prepare`, `next`, `page`,
  `acknowledge`, `commit`) make no network requests themselves. But the agent
  reading their pages sees message text plus real names, contact IDs, and group
  titles. If that agent uses a cloud model, all of that goes to its provider.

Every decision, allowed or denied, is written to a tamper-evident audit log
that holds no message text. Check it with `audit-connector-log`.

### What GreenBubbles cannot control

Once text leaves GreenBubbles, its privacy depends on everything that handles
it next:

- the AI model: running on your Mac, or someone else's service;
- the embedding service and vector database, if you build a search index;
- how long your agent tool keeps transcripts and logs;
- any log collectors or crash reporters on your Mac.

GreenBubbles labels cloud destinations and keeps file paths away from them,
but it cannot see what happens after that. Checking those services is up to
you, and it is the part people most often get wrong. See the
[threat model](docs/THREAT_MODEL.md).

## Other people

Your WeChat history isn't only yours. It holds what other people told you in
confidence, never expecting an AI to read it years later.

No technical feature solves this. What GreenBubbles gives you is a way to share
narrowly: a policy can limit access to certain chats, fields, and dates, so
sharing one project's chat doesn't share the rest of your life or theirs. The
limited view also leaves out raw database columns, file paths, and hidden
metadata, and exports replace WeChat IDs with anonymous ones.

Use the narrow scope. That is what it is for.

## What the project sees

Nothing, unless you send it.

When you report a problem, share only reports that contain no personal content;
most commands can print one for exactly this purpose. Never attach a database,
key, recovery phrase, message, photo or file, account ID, full file path, or
notes. Report security problems as described in [SECURITY.md](SECURITY.md).

The repository contains no real user data. Automated checks look for secrets
before every commit and in CI, and all tests use made-up data.

## Keeping and deleting data

GreenBubbles never deletes a finished backup, replica, audit log, or query
profile on its own, no matter how old. When you retire an old backup, it is
moved to a quarantine folder and stays there until you delete it.

GreenBubbles does clean up its own half-finished temporary files after a failed
or cancelled operation, and temporary files when a session ends. That cleanup
can never delete a finished backup.

This is on purpose: silently losing a backup is worse than keeping one too
long. But it means **deleting old data is your job.** Before you delete an old
backup, make sure you can restore the one you're keeping.

## Uninstalling

1. Delete the app and the command-line tools.
2. Delete anything you created: backups, recovery phrases, key files, replicas,
   audit logs, query profiles in `~/.greenbubbles/`,
   `~/.greenbubbles-acquire/passphrase.txt`, and search indexes in
   `~/Library/Application Support/GreenBubbles/HistoryIndexes`.
3. If you installed the send helper, run `greenbubbles-send uninstall-helper`.
   It removes the login item and prints the `tccutil reset` commands that take
   back its Accessibility and Screen Recording permissions.

GreenBubbles installs nothing from third parties, so nothing else is left
behind. Your WeChat data is untouched, because GreenBubbles never wrote to it.
