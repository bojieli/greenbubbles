# How GreenBubbles compares

People arriving here have usually already tried a WeChat exporter, or have at
least found one. This page is about where GreenBubbles is genuinely different,
and where an exporter is the better tool and you should use one.

## The short version

An exporter's job is to turn a closed database into files you can read. Its
success condition is a complete, decrypted copy on your disk.

GreenBubbles' job is to answer one question at a time from data that never
leaves its original form. Its success condition is that the complete copy is
never created.

Those are different goals, and the second one only makes sense because of the
consumer. A model that receives a full export receives everything: the medical
conversation, the argument, the salary discussion, the message a friend asked
you not to repeat. Nothing about "I only wanted the project thread" survives
contact with a directory of JSON.

| | GreenBubbles | Typical exporter |
| --- | --- | --- |
| Normal output | one bounded page, with citations | the whole corpus |
| Plaintext copy on disk | none by default | the entire history |
| Media | one attachment, resolved on request | all of it, or none |
| Freshness | reported per response | as of export time |
| Incomplete coverage | reported explicitly, verdict stays false | usually silent |
| Backup story | encrypted under its own key, 24-word recovery | a folder of decrypted files |
| Getting the key | one verified command, then bounded reads | often the headline feature |
| Scope of a mistake | one page | everything |

## When you should use an exporter instead

Genuinely, and without qualification:

- **You want to read an archive.** A browsable HTML transcript of a group chat
  from 2019 is a perfectly good thing to want, and GreenBubbles' bounded pager
  is a worse way to get it.
- **You are leaving the platform.** If the goal is a portable copy of
  everything, that *is* a full export, and GreenBubbles' explicit restoration
  path is more machinery than you need.
- **You are on Windows or Android.** GreenBubbles is macOS-only and has no plan
  to change that.
- **You want analytics over your whole history.** Word clouds, message
  frequency by year, who you talk to most. That needs the whole corpus, by
  definition.

## What GreenBubbles adds

**Coverage evidence.** Every response says which databases it read, whether the
read was atomic across them, what it could not resolve, and what it skipped. An
export gives you a directory and leaves you to assume it is complete. When
GreenBubbles cannot decode a message type, the message is reported as a gap and
the completion verdict stays false. See [AUDITING.md](AUDITING.md).

**A backup that outlives WeChat.** A copy of encrypted WeChat files is not a
backup — it still needs WeChat's key, which lives in a running application. A
GreenBubbles snapshot is re-encrypted under a key of its own, recoverable from
24 words you hold, and verified by opening it with *no* WeChat key at all. See
[RECOVERABLE_SNAPSHOTS.md](RECOVERABLE_SNAPSHOTS.md).

**A boundary designed for a model.** Policy binds conversations, fields, time
range and destination; message text is treated as untrusted data that cannot
select another operation; every decision lands in a hash-chained, body-free
journal. See [AI_TOOL_BOUNDARY.md](AI_TOOL_BOUNDARY.md).

**Continuous, change-proportional synchronization.** An incremental acquisition
copies only source sets whose byte evidence actually changed — 9 of 25 in the
recorded run — instead of re-exporting everything.

## On the projects themselves

GreenBubbles is not the first attempt at this, and the earlier ones established
that people want access to their own archives:
[WeChatMsg](https://github.com/LC044/WeChatMsg),
[PyWxDump](https://github.com/xaoyaoo/PyWxDump),
[chatlog](https://github.com/sjzar/chatlog) and
[WechatExporter](https://github.com/BlueMatthew/WechatExporter).

Two things this project's own survey recorded, which are worth knowing before
you plan around any of them:

- **Acquisition is the hard part everywhere, and nobody has a supported
  answer.** Every macOS path that exists — including
  [`robbin/wechat-exporter`](https://github.com/robbin/wechat-exporter) —
  re-signs the client and extracts the key from a live process. Windows
  projects use process memory scanning or injected hooks. Different mechanics,
  same category. GreenBubbles' helper works the same way, and
  [its guide](PASSPHRASE_ACQUISITION.md) documents the mechanism step by step
  rather than hiding it.
`WechatExporter` is a different shape again: it reads an unencrypted iTunes/iOS
backup, targets much older mobile versions, and neither obtains nor needs the
current macOS desktop key. If your history is on a phone and you have a backup,
that is a materially safer path than anything involving a live process.

## What GreenBubbles is not

It is not a WeChat server, a cloud sync service, a bot account, an anonymity
tool, or an access-control bypass. It reads data you are already authorized to
read, on hardware you own, and its send path ships cryptographically closed.

It is also not finished. Read [KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md)
before choosing it over a tool that has been around longer.
