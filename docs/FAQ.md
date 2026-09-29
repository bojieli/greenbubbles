# FAQ

Common questions, including the ones whose honest answer is "that's a real
limitation."

## Getting it working

### I don't have the database key. Can I use this at all?

Yes, once you capture it. Capturing copies the key from your own WeChat app:

1. Re-sign your copy of WeChat so a debugger is allowed to attach.
2. Run `greenbubbles-acquire preflight` to check you're ready.
3. Run `sudo greenbubbles-acquire capture`, then log out of WeChat and back in.

The tool checks the key against every database and saves it to a file only you
can read. This needs administrator access. Step-by-step:
[key setup guide](PASSPHRASE_ACQUISITION.md).

GreenBubbles doesn't break WeChat's encryption and never will; it reads the key
from your own app. If you'd rather not capture it, you can still open a backup
someone already made for you, or data that is already unencrypted.

### Do I need a profile or a folder path?

Not for your live WeChat data. After capture, `greenbubbles chats` finds the
WeChat account you're using and reads the key from
`~/.greenbubbles-acquire/passphrase.txt`.

- Old account folders left behind after switching accounts are ignored.
- If two accounts were both used in the last 14 days, GreenBubbles won't guess.
  Set `source.root` in `~/.greenbubbles/config.toml` to the one you want.
- To use a different key file, set `source.passphrase_file` in the same file.
- If the key file is missing, you get an `invalidProfile` error with a plain
  explanation. It never prints the path or the key.

You need a [profile](QUERY_PROFILES.md) only for a second account or a backup.

### Which folder holds my WeChat data?

The folder named `db_storage`, which contains `contact`, `session`, and
`message`. GreenBubbles finds it for you. On current WeChat versions it is:

```text
~/Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files/<account>/db_storage
```

Point at `db_storage` itself, not the folder above it and not a single `.db`
file. To list your accounts and their folders:

```sh
greenbubbles-discover accounts --include-paths
```

That output contains your account ID. Keep it private.

### The key works, but one database won't open

WeChat splits your history across many database files. If one of them fails,
you still get results from the others, and the output says which file was
skipped and marks the results as incomplete. Nothing is dropped silently. If
one of the essential databases (contacts, sessions, or messages) fails, the
command fails.

### How do I check it works on my own data?

Run this from a source checkout:

```sh
swift scripts/check-live-database.swift
```

It finds your accounts, tries your key on each, and runs a set of reading and
searching checks against real conversations. It prints one JSON report with no
paths, IDs, search text, or message content, so it's safe to paste into an
issue. It only works with your real data and key, so it doesn't run in CI.

## Things that look like bugs

### Why is search sometimes slow?

When WeChat's own search index can't be used, GreenBubbles searches the most
recent 500 messages directly. That takes about a quarter of a second for one
chat, or about a third of a second across 16 chats. The alternative would be
keeping a second searchable copy of your messages on disk, and that speed
didn't justify it. Numbers: [MEASUREMENTS.md](MEASUREMENTS.md).

### Search found nothing, but I know the message exists

There are three likely causes:

1. **The search hasn't reached it yet.** Each search result page looks at no
   more than 500 messages across 16 chats. An empty page doesn't mean nothing
   was found. If the output has `hasMore: true`, run the command again with
   `--cursor` set to the `nextCursor` value.
2. **WeChat's own index is out of date.** When GreenBubbles uses WeChat's index
   and can't confirm it's current, the output says
   `nativeSearchIndexFreshnessUnverified`.
3. **The message isn't on this Mac.** Messages kept only on WeChat's servers or
   only on your phone can't be reached from here.

### Names show as `wxid_…` instead of people's names

GreenBubbles looks up names in WeChat's contact database, up to 500 people per
command. If it can't find someone, the output keeps the raw ID and says
`contactDisplayNameUnresolved` or `contactEnrichmentUnavailable`. Messages are
still shown; only the name is missing. A group's name comes from the group
itself, never from whoever spoke last.

### Two pages disagree, or the output says `crossDatabaseAtomic: false`

That's an honest warning, not a bug. WeChat stores your history in several
database files, and GreenBubbles reads them one after another, not all at the
same instant. If WeChat writes in between, results can differ slightly. For a
view that never changes, make a [backup](RECOVERABLE_SNAPSHOTS.md) and read
that instead.

### Does a long query slow down WeChat?

No. GreenBubbles finishes each database read before doing anything else, so it
never holds a database open while you or an AI is thinking. WeChat can keep
writing normally. That's also why there's no command that streams your whole
history at once.

## Safety

### Is my database key ever sent to an AI?

No. Keys, passphrases, recovery phrases, search text, and draft messages are
passed on standard input. They never appear on the command line or in output,
logs, errors, or saved files, and keys are erased from memory after use. No AI
tool can ask GreenBubbles for them.

### Can an AI read all my chats?

It depends on how the AI reaches your data.

- **Through the AI connector with a policy:** only what the policy allows. A
  policy covers one account and, for each chat, says which actions and message
  fields are allowed, an optional date range, and whether a cloud AI may see
  it. Cloud access is off unless you turn it on for that chat. Every allowed or
  denied request is written to a tamper-evident audit log with no message text.
  See [AI context CLI](AI_CONTEXT_CLI.md).
- **Through the command line or a coding agent:** it has the same access to
  your files that you have. A skill tells an agent what to do, but it can't
  restrict it. Use the connector when you need enforced limits.
- **Through personal memory:** you choose which chats, people, or dates to
  include; if you leave those choices empty, every message is included. The
  agent reads the chosen messages in pages of up to 48 KB, and the pages
  include real names and contact IDs. It never sees your database key. The
  agent eventually reads every page you selected, so if it uses a cloud model,
  all of that goes to the model provider under their privacy terms. See
  [Personal memory](PERSONAL_MEMORY.md).

### Can I list my WeChat contacts?

Yes:

```sh
greenbubbles contacts list --limit 50
```

It returns 1 to 500 contacts per page. Filter with
`--kind person|group|official|service|account-holder|unknown`. Add `--details`
only if you need remarks, nicknames, and aliases. `person` means an ordinary
address-book entry; it doesn't prove you're still in touch. Your own account is
shown as `You`.

### What if a message in my history tells the AI to do something?

Through the connector, nothing happens. The AI can only ask for specific
actions, and each one is checked against your policy before any message text is
returned. A message saying "open another chat," "turn on cloud access," or
"send a reply" is just text. The AI can't send messages at all.

### Does anything get uploaded?

Reading, searching, exports, and notes upload nothing. There's no background
service, telemetry, or cloud component.

The one exception is `ai-summarize-direct`, and only when you run it. It sends
the chats your policy marks `allowRemoteModel` to Google's Gemini 3.8 Flash and
records that in the audit log. It sends message text, senders, and times, but
not real message IDs, sender IDs, your policy or audit files, or database
details.

Any other cloud AI, search index, log collector, or crash reporter you use is
outside GreenBubbles' control; deciding whether to trust it is up to you.

### Can WeChat tell I'm doing this?

Reading never contacts WeChat's servers, injects code, or uses WeChat's private
interfaces, so there's nothing for WeChat to notice. Capturing the key is
different: it re-signs WeChat and needs a logout and login, which WeChat can
obviously see. See the [threat model](THREAT_MODEL.md).

### What about the other people in my chats?

They're in your history, and they didn't choose your tools. No feature can
settle that for you. What GreenBubbles offers is a way to share only the chats
you actually need instead of everything. Deciding what to share is a judgment
call worth making on purpose.

## Backups and recovery

### Is copying `db_storage` a backup?

No, and this is the most costly mistake you can make here. Those files are
encrypted with WeChat's key. If you ever lose access to that key, the copy is
useless.

A GreenBubbles backup (snapshot) is encrypted again with a new random key,
protected by a 24-word recovery phrase you keep. To prove a backup works,
`snapshot verify` opens it **without any WeChat key**:

```sh
greenbubbles snapshot verify <snapshot-directory> \
  --snapshot-recovery-kit <owner-only-recovery-kit-file>
```

See [Recoverable snapshots](RECOVERABLE_SNAPSHOTS.md).

### I lost my Keychain entry or hidden credential file

Open the backup with your recovery-kit file instead, then create a new
credential for it. Don't try to recreate the lost one; it was a random key, not
something made from a password.

### I lost the 24 words

If the Keychain entry or hidden credential still exists on the Mac that made
the backup, open the backup with it and create a new recovery phrase right
away. If both are gone, the backup can't be opened. That's what the encryption
is for.

This is also why GreenBubbles won't let you remove the last recovery phrase,
and why it writes the recovery kit before the long backup starts, not after.

### Where should I keep the recovery words?

Anywhere except next to the only copy of the backup. A real backup needs an
intact backup copy **and** a working recovery phrase, kept in different places.
Never reuse a cryptocurrency wallet phrase.

## Scope

### Can I send messages?

No. Experimental sending code exists, but public builds lock it: they can only
do a dry run. No AI tool can reach it. Unlocking it depends on legal and account
safety questions, not on code. See [SEND_ADAPTER.md](SEND_ADAPTER.md).

### Windows? Linux? Android? iOS?

No, and none are planned. Released builds need macOS 14 or later on Apple
silicon.

### Will it keep working after a WeChat update?

Usually, but not always. It has been tested on WeChat 4.1, and a normal update
doesn't make a captured key stop working. But WeChat's format is private and
can change without notice. When GreenBubbles meets data it can't read, it says
so instead of guessing.

### Can I use more than one WeChat account?

Yes. GreenBubbles finds every account it can read. Each account has its own key,
policies, and backups, and a policy made for one account is rejected for
another. Run commands for each account separately, using a
[profile](QUERY_PROFILES.md) for each.

### How much disk space does it need?

Reading and searching need almost none, because GreenBubbles reads WeChat's own
files instead of copying them. A backup is about the size of WeChat's
databases. A full restore to plain text is the expensive option: in one test, a
2.98 GB WeChat database became a 13.5 GB text archive, briefly needing 7.4 GB of
extra working space, and copying all media added about 30 GB. Numbers:
[MEASUREMENTS.md](MEASUREMENTS.md).

### My problem isn't listed here

Check [known limitations](KNOWN_LIMITATIONS.md) first; it may be a known
limit rather than a bug. When reporting a bug, describe what happened without
message content, IDs, or full file paths. Report security problems privately as
described in [SECURITY.md](../SECURITY.md), never in a public issue.
