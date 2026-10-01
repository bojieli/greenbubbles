---
name: greenbubbles-setup
description: Install or diagnose the local GreenBubbles CLI and read-only WeChat access. Use for a missing CLI, failed database access, source selection, or initial owner-operated key capture.
---

# Set up GreenBubbles

Work on the user's Mac with the local CLI. Resolve file links relative to this
skill. Setup needs no embedded agent, MCP server, or separate model API key.

## Check access

Run `python3 <this-skill>/scripts/doctor.py` first. Supply
`--greenbubbles /absolute/path/to/greenbubbles` or `--profile NAME` when known.
The doctor checks capabilities and read-only source access without printing keys
or messages. A failed check is a setup issue, not evidence of empty history.

## Fix the failed check

Read [references/setup.md](references/setup.md) for the relevant installation,
source/profile, or capture steps. Use `command -v greenbubbles` and
`greenbubbles --version` to identify the selected executable; 0.10.0 includes
`chats find`, name selection, and `messages recent`. Do not silently substitute
another executable after a failed check.

Ordinary live use needs no profile after capture: the CLI discovers the account
and reads `~/.greenbubbles-acquire/passphrase.txt`. Custom paths belong in
`~/.greenbubbles/config.toml`. Use profiles for a second account or a snapshot.
Never put credentials in chat, arguments, logs, or projects.

If a source hangs, diagnose one directory open with a timeout before retrying.
Workspace permissions, Unix ownership, and macOS privacy access are separate;
compare the user's terminal with the agent host and verify changes with a fresh
read. Key capture is owner-operated; follow the setup reference's authorization
requirements for steps that re-sign WeChat.

## Continue the requested task

After `source status` succeeds, use [context](../greenbubbles-context/SKILL.md)
for a query or [personal memory](../greenbubbles-personal-memory/SKILL.md) for
maintained notes. Reading messages across all chats is unnecessary to verify
setup. Keep the complete sibling skill package available; installation into
host discovery folders is optional.
