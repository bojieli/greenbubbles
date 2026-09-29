---
name: greenbubbles-setup
description: Set up or diagnose local GreenBubbles CLI access to the user's WeChat history for Codex, Claude Code, OpenCode, Kimi Code, Gemini CLI, Grok Build, or another shell-capable agent. Use for missing CLI, source profiles, credentials, or initial capture setup.
---

# GreenBubbles setup

Use the installed local CLI; no embedded agent, MCP server, or separate model is required.
Resolve resources relative to this SKILL.md, not the current working directory.

Run `python3 <this-skill>/scripts/doctor.py` first. Supply `--greenbubbles
/absolute/path/to/greenbubbles` or `--profile NAME` when known. The helper checks
CLI capabilities and opens the configured source read-only; it does not print
credentials or message text, capture keys, change profiles, or launch agents.
A failing check is a setup diagnosis, not evidence that history is empty.
If a live source hangs, test one directory open with a timeout before retrying
database reads. Workspace permission, Unix ownership, and macOS privacy access
are separate. Compare the user's terminal with the agent host; do not infer TCC
attribution from parent PID alone or diagnose a denial from a preflight log.
Verify any privacy change with a fresh read before claiming access is fixed.

Read [references/setup.md](references/setup.md) for installation, source setup,
or owner-operated capture. For the live database, do not create a profile:
after capture, `greenbubbles chats` opens the newest installed WeChat database
and reads `~/.greenbubbles-acquire/passphrase.txt`. A different database or
passphrase path belongs in `~/.greenbubbles/config.toml`, not in a command
argument. Reuse an existing profile only for a snapshot or a second account.
Never paste keys into chat, arguments, logs, or projects.

Once ready, read the sibling `../greenbubbles-context/SKILL.md` for bounded
queries or `../greenbubbles-personal-memory/SKILL.md` for a maintained
Markdown/Python project. Use host discovery if already available; copying the
skills into discovery directories is optional. If a sibling is missing, locate
the complete package rather than inventing commands. Keep the current host's normal tool permissions.
