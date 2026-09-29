# Use GreenBubbles with your own agent

GreenBubbles provides the local data boundary; your current Codex, Claude Code,
OpenCode, Kimi Code, Gemini CLI, Grok Build, or compatible shell-capable agent can do the extraction, summarization, and
incremental memory edits. The embedded model workflow is optional.

## Choose where the model runs

| Route | Authentication and billing | Best fit |
| --- | --- | --- |
| Existing coding agent + skills | Your agent's existing subscription login or provider configuration | Recommended if you already have a suitable subscription with available usage |
| Embedded `ai-summarize-direct` workflow | Dedicated `GEMINI_API_KEY`; separate provider API billing | A bounded model-generated memory artifact directly through GreenBubbles |
| Optional `personal-memory-parallel.py` driver | Launches your chosen coding agent; that agent's authentication determines billing | Scripted batches or scheduled runs |

The embedded summarizer produces bounded artifacts; the personal-memory skill
maintains the git-versioned domain project. They are different entry points,
not interchangeable CLI commands. A subscription does not supply the embedded
summarizer's API key. Using an already-paid subscription allowance can avoid
additional API charges and is our preferred starting point. It is not a promise
of unlimited or universally cheaper processing: limits, overage, model choice,
and workload matter. Verify the agent is using subscription authentication,
rather than API-key billing. See [Codex authentication](https://learn.chatgpt.com/docs/auth)
and [Claude Code costs](https://code.claude.com/docs/en/costs).

## Use directly — no skill installation required

Skills are documentation with optional helper scripts. Your agent can read them
from a checkout or extracted bundle without copying them into a discovery folder:

> Read `/absolute/path/to/greenbubbles/skills/greenbubbles-personal-memory/SKILL.md`
> and follow its references to extract and incrementally maintain my Markdown
> memory in this session. If access needs setup, read the sibling
> `greenbubbles-setup/SKILL.md`.

Keep each skill's references and helpers alongside its `SKILL.md`. The local
GreenBubbles CLI is the executable dependency. The agent needs shell access and
permission to read the selected source and edit the memory project.

## Homebrew paths

The Homebrew package stores the skills and helpers under `libexec`:

```sh
echo "$(brew --prefix greenbubbles)/libexec/skills/greenbubbles-personal-memory/SKILL.md"
python3 "$(brew --prefix greenbubbles)/libexec/scripts/install-skills.py" --agent codex
```

The second command is optional. For the relative commands below, first change
into `$(brew --prefix greenbubbles)/libexec`, or use the corresponding absolute
script path. Source checkouts and extracted release ZIPs use their own root.

## Optional automatic discovery

From this checkout or an extracted CLI release, choose your host:

```sh
python3 scripts/install-skills.py --agent codex
python3 scripts/install-skills.py --agent claude
python3 scripts/install-skills.py --agent opencode
python3 scripts/install-skills.py --agent kimi
python3 scripts/install-skills.py --agent gemini
python3 scripts/install-skills.py --agent grok
```

The installer only copies documentation and helpers into the host's discovery
folder. Each target gets the same three skills:

- `greenbubbles-setup`: read-only CLI/profile checks and initial setup guidance.
- `greenbubbles-context`: bounded search and retrieval.
- `greenbubbles-personal-memory`: prepare, review, cite, organize, and refresh memory.

| Host / target | User discovery directory | With `--project PATH` |
| --- | --- | --- |
| Codex / `codex` | `~/.agents/skills` | `PATH/.agents/skills` |
| Claude Code / `claude` | `~/.claude/skills` | `PATH/.claude/skills` |
| OpenCode / `opencode` | `~/.config/opencode/skills` | `PATH/.opencode/skills` |
| Kimi Code / `kimi` | `$KIMI_CODE_HOME/skills`, default `~/.kimi-code/skills` | `PATH/.kimi-code/skills` |
| Gemini CLI / `gemini` | `~/.gemini/skills` | `PATH/.gemini/skills` |
| Grok Build CLI / `grok` | `~/.grok/skills` | `PATH/.grok/skills` |

The Grok target is xAI's local Grok Build (`grok`) coding CLI. It does not add
local filesystem access to the Grok website or mobile app. OpenCode can also
use Grok through a configured provider; that uses the OpenCode skill target.
No target changes authentication or model selection. Subscription allowances
and API charges depend on each host's configured provider, not the skill.

Copies remain usable after the checkout is moved. No settings, credentials,
host permissions, model configuration, or running agents are changed.
For a project-scoped install add `--project /absolute/path/to/project`.
Other Agent Skills-compatible hosts can use `--dest /path/to/their/skills`.
The host still needs a shell, local CLI/source access, and file editing capability.

Re-run with `--update` to replace an unmodified managed installation. Identical
installs are a no-op; locally edited or unmanaged differing skills are preserved
and reported as conflicts. Move or merge your customizations before updating.
Restart the host if the skills do not appear. Codex supports `$greenbubbles-setup`;
Claude Code and Grok Build support `/greenbubbles-setup`; Kimi Code uses
`/skill:greenbubbles-setup`. In OpenCode, ask it to use `greenbubbles-setup`.
In Gemini CLI, use `/skills reload`, then ask it to use the skill. Host skill
activation and tool permissions still apply. If a custom configuration uses a
different discovery root, pass `--dest` with that root's skills directory.

Discovery details: [Codex skills](https://learn.chatgpt.com/docs/build-skills),
[Claude Code skills](https://code.claude.com/docs/en/skills),
[OpenCode skills](https://opencode.ai/docs/skills/),
[Kimi Code skills](https://www.kimi.com/code/docs/en/kimi-code-cli/customization/skills.html),
[Gemini CLI skills](https://geminicli.com/docs/cli/using-agent-skills/), and
[Grok Build skills](https://docs.x.ai/build/features/skills-plugins-marketplaces).

## Use the current agent

Example request after reading the skill directly or enabling discovery:

> Use greenbubbles-personal-memory with my GreenBubbles profile `live` to
> organize my WeChat history into a Markdown memory project at `~/memory/me`.
> Do the extraction in this session. Preserve source citations and update
> existing facts without duplication. Start with September's direct chats.

For a follow-up:

> Refresh that memory project from my latest WeChat history. Resume any
> incomplete batch first, and report the dates covered and coverage limitations.

The agent checks access, prepares a private local corpus, reads bounded pages,
patches `manifest.md` and `domains/*.md`, acknowledges reviewed evidence, and
commits progress. It uses the model you already configured. No extra embedded
model API key or nested agent is required.

For diagnostics only:

```sh
python3 skills/greenbubbles-setup/scripts/doctor.py \
  --greenbubbles /absolute/path/to/greenbubbles --profile live
```

The doctor never captures keys or prints subprocess output. Exit 0 means the
memory CLI and selected profile validated; exit 1 means setup needs attention.
It does not prove full source coverage. Missing credentials route to owner-operated
capture guidance; capture may require a WeChat restart/re-login and re-signing.
See [key acquisition](PASSPHRASE_ACQUISITION.md).

Preparation/decryption is local. Any message page the agent reads goes to that
agent's configured model provider. The corpus and derived memory are private
artifacts; installation does not copy them into the skill package.

## Incremental updates and limits

The skill resumes outstanding batches using the same state. A new immutable
corpus generation uses a new state path and the same domain project. Routine
updates use an explicit completed time window with inclusive overlap and
deduplicate facts. Late-imported historical messages or old-timestamp edits
require a broader reconciliation pass; timestamp windows are not a complete
change feed. The skill documents both paths and does not silently advance a
checkpoint after incomplete work.

Use one writer per domain project. A git commit lock does not prevent two agents
from overwriting each other's domain edits. No scheduler is installed by the
skills. The optional driver remains available from the repository/CLI release;
use `--agent codex --format markdown --shards 1 --parallel 1`, or choose Claude
or Gemini. The driver currently has built-in launchers for Pi, Claude, Codex,
and Gemini, plus `--agent command` for a custom launcher. The discovery targets
above do not imply identically named driver launchers: use OpenCode, Kimi Code,
and Grok Build in their current sessions with the skills.
An incomplete driver tick now exits nonzero, preserves its checkpoint, and resumes
its saved plan and shard states when rerun with the same corpus and scope options.
It advances the time checkpoint only after every planned shard completes.

## Distribute without the checkout

```sh
python3 scripts/install-skills.py --bundle /tmp/greenbubbles-skills
```

This creates a standalone directory with the three skills, installer, license,
and usage instructions. Copy or zip it for another machine, then point an agent
at its `SKILL.md` files or optionally run `scripts/install-skills.py`. The GreenBubbles binary is a separate prerequisite;
the bundle carries no binaries, credentials, chat data, or personal configuration.
