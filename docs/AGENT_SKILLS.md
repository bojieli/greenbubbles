# Use GreenBubbles with your own agent

You can have a coding agent you already use read your WeChat history and write
notes from it: Codex, Claude Code, OpenCode, Kimi Code, Gemini CLI, Grok Build,
or any other agent that can run shell commands. GreenBubbles reads the data;
your agent does the summarizing and writing.

GreenBubbles teaches the agent how through **skills**: Markdown files of
instructions, with a few helper files next to them. Your agent can read a skill
straight from where it is. Installing it is optional.

## Pick how the AI runs

| Option | Who pays | Good for |
| --- | --- | --- |
| **Your coding agent + skills** (recommended) | Your agent's existing subscription or provider setup | Most people, especially if you already have a subscription with spare usage |
| **Built-in summarizer** (`ai-summarize-direct`) | Your own `GEMINI_API_KEY`, billed by Google | A one-off cited summary made by GreenBubbles itself |
| **Batch script** (`personal-memory-parallel.py`) | Whatever agent it launches | Scripted or scheduled runs (advanced) |

These are separate tools, not different settings of one command. The built-in
summarizer writes a new summary each run. The personal-memory skill keeps one
set of notes, tracked with Git, and updates it over time. A subscription does
not give the built-in summarizer an API key.

Using a subscription you already pay for often costs nothing extra, which is
why we suggest starting there. It is not unlimited: plan limits, overage, model
choice, and how much you ask the agent to read all matter. Check that your agent
is signed in with your subscription and not an API key. See
[Codex authentication](https://learn.chatgpt.com/docs/auth) and
[Claude Code costs](https://code.claude.com/docs/en/costs).

## Get started

1. Finish key setup first. See [Getting your database key](PASSPHRASE_ACQUISITION.md).
2. Find the skill file. With Homebrew:

   ```sh
   echo "$(brew --prefix greenbubbles)/libexec/skills/greenbubbles-personal-memory/SKILL.md"
   ```

   In a source checkout or the extracted CLI ZIP, it is
   `skills/greenbubbles-personal-memory/SKILL.md`.
3. Open your agent and ask it something like:

   > Read `/absolute/path/to/greenbubbles-personal-memory/SKILL.md` and follow
   > it to build Markdown notes from my WeChat history in `~/memory/me`, in
   > this session. If GreenBubbles isn't set up yet, read the setup skill at
   > `../greenbubbles-setup/SKILL.md` next to it.

Keep each `SKILL.md` together with the files around it; the skill refers to
them. The agent needs permission to run shell commands, run the `greenbubbles`
command, and edit files in your notes folder.

## What to ask your agent

To build notes for the first time:

> Use greenbubbles-personal-memory to organize my WeChat history into a
> Wikipedia-style knowledge base at `~/memory/me`. Read the live database in
> this session. Ask me what time range to cover if I haven't said. Revise the
> articles in place.

To bring them up to date later:

> Refresh that knowledge base from my latest WeChat history. Keep the same
> project, and tell me the dates covered and which chats are still unread.

The agent will:

1. check that GreenBubbles can read your data;
2. rank your chats by how often you write in them, with `chats rank`;
3. read them with `messages list` and `messages search`;
4. write or update `index.md` (the front page), `manifest.md` (what was read
   and when), and one article per life area in `domains/`.

It writes in the language you usually use in WeChat, and it uses whatever model
your agent is already set up with. No extra API key is needed. See
[Personal memory](PERSONAL_MEMORY.md) for how the notes are organized.

## Let your agent find the skills automatically (optional)

Instead of giving the path each time, you can copy the skills into your agent's
skills folder. Pick your agent:

```sh
python3 scripts/install-skills.py --agent codex
python3 scripts/install-skills.py --agent claude
python3 scripts/install-skills.py --agent opencode
python3 scripts/install-skills.py --agent kimi
python3 scripts/install-skills.py --agent gemini
python3 scripts/install-skills.py --agent grok
```

Run these from a source checkout or the extracted CLI ZIP. With Homebrew, run
them from `$(brew --prefix greenbubbles)/libexec`, or use the full path:

```sh
python3 "$(brew --prefix greenbubbles)/libexec/scripts/install-skills.py" --agent codex
```

This copies three skills:

- `greenbubbles-setup`: checks that GreenBubbles works and helps with first setup.
- `greenbubbles-context`: searches and retrieves messages.
- `greenbubbles-personal-memory`: builds and updates your notes.

It only copies files. It doesn't change your agent's settings, sign-in, model,
or permissions, and the copies keep working if you move the checkout.

| Agent / `--agent` value | Installs to | With `--project PATH` |
| --- | --- | --- |
| Codex / `codex` | `~/.agents/skills` | `PATH/.agents/skills` |
| Claude Code / `claude` | `~/.claude/skills` | `PATH/.claude/skills` |
| OpenCode / `opencode` | `~/.config/opencode/skills` | `PATH/.opencode/skills` |
| Kimi Code / `kimi` | `$KIMI_CODE_HOME/skills`, default `~/.kimi-code/skills` | `PATH/.kimi-code/skills` |
| Gemini CLI / `gemini` | `~/.gemini/skills` | `PATH/.gemini/skills` |
| Grok Build / `grok` | `~/.grok/skills` | `PATH/.grok/skills` |

Other options:

- `--project /absolute/path/to/project` installs for one project instead of for
  your whole account.
- `--dest /path/to/skills` installs into any other folder, for agents not listed
  above or a custom setup.
- `--update` replaces an older copy made by this script. If you edited your
  copy, it is left alone and reported as a conflict; move your changes out
  first.

After installing, restart your agent if the skills don't appear. Then start the
setup skill:

| Agent | How to start the setup skill |
| --- | --- |
| Codex | `$greenbubbles-setup` |
| Claude Code, Grok Build | `/greenbubbles-setup` |
| Kimi Code | `/skill:greenbubbles-setup` |
| OpenCode | ask it to use `greenbubbles-setup` |
| Gemini CLI | run `/skills reload`, then ask it to use the skill |

The `grok` option is for xAI's Grok Build command-line tool. It doesn't give the
Grok website or phone app access to your files. If you use Grok models through
OpenCode, use the `opencode` option instead.

More about each agent's skills: [Codex](https://learn.chatgpt.com/docs/build-skills),
[Claude Code](https://code.claude.com/docs/en/skills),
[OpenCode](https://opencode.ai/docs/skills/),
[Kimi Code](https://www.kimi.com/code/docs/en/kimi-code-cli/customization/skills.html),
[Gemini CLI](https://geminicli.com/docs/cli/using-agent-skills/),
[Grok Build](https://docs.x.ai/build/features/skills-plugins-marketplaces).

## If something isn't working

Run the setup check:

```sh
python3 skills/greenbubbles-setup/scripts/doctor.py \
  --greenbubbles /absolute/path/to/greenbubbles --profile live
```

- **Exit code 0:** GreenBubbles and the chosen profile work.
- **Exit code 1:** setup needs attention.

The check only reads; it never captures keys or prints command output. Passing
it doesn't prove every message is readable. If the key is missing, follow the
[key setup guide](PASSPHRASE_ACQUISITION.md). Capturing the key may require
restarting WeChat, logging in again, and re-signing it.

## Privacy

GreenBubbles decrypts and reads your data on your Mac. But every message page
your agent reads is sent to the agent's model provider, if it uses a cloud
model. Your notes are private files; installing the skills never copies them
anywhere. Never paste your database key or recovery phrase into a prompt. See
[PRIVACY.md](../PRIVACY.md).

## Updating notes later, and limits

An update opens the same notes folder, reads recent messages, and revises the
articles.

- **Old messages can be missed.** An update usually starts from the last date it
  covered. Messages imported late with older dates, or old messages edited
  later, fall before that date. If that matters, ask the agent to re-read the
  chosen chats over the whole original date range.
- **Missing isn't deleted.** If a search doesn't find a fact, that doesn't mean
  the fact was removed.
- **One agent at a time.** Two agents editing the same notes can overwrite each
  other's changes, even with Git.
- **No schedule is set up.** The skills don't run on their own.

For scripted or scheduled runs, the batch script `personal-memory-parallel.py`
is in the repository and the CLI ZIP. It has built-in support for Pi, Claude,
Codex, and Gemini, plus `--agent command` for anything else. For example:
`--agent codex --format markdown --shards 1 --parallel 1`. Use OpenCode, Kimi
Code, and Grok Build interactively with the skills instead. If a run stops
partway, it exits with an error and keeps its progress; run the same command
again to continue. See [Personal memory](PERSONAL_MEMORY.md#batch-script-and-evidence-archive-advanced)
for details.

## Copy the skills to another Mac

```sh
python3 scripts/install-skills.py --bundle /tmp/greenbubbles-skills
```

This makes a standalone folder with the three skills, the installer, the
license, and instructions. Copy or zip it to another Mac, then point an agent at
a `SKILL.md` or run its `scripts/install-skills.py`. You still need to install
GreenBubbles itself there. The folder contains no programs, keys, chat data, or
personal settings.
