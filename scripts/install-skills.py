#!/usr/bin/env python3
"""Copy portable GreenBubbles skills into a host's discovery directory.

No host settings, credentials, permissions, or binaries are changed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import tempfile

NAMES = ("greenbubbles-setup", "greenbubbles-context", "greenbubbles-personal-memory")
AGENT_DIRECTORIES = {
    "codex": ".agents",
    "claude": ".claude",
    "opencode": ".opencode",
    "kimi": ".kimi-code",
    "gemini": ".gemini",
    "grok": ".grok",
}
RECEIPT = ".greenbubbles-skill-install.json"


def discovery_directory(agent, project=None):
    """Resolve native discovery paths without changing host configuration."""
    if project is not None:
        return project.expanduser().absolute() / AGENT_DIRECTORIES[agent] / "skills"
    if agent == "opencode":
        return Path.home() / ".config" / "opencode" / "skills"
    if agent == "kimi" and os.environ.get("KIMI_CODE_HOME"):
        return Path(os.environ["KIMI_CODE_HOME"]).expanduser().absolute() / "skills"
    return Path.home() / AGENT_DIRECTORIES[agent] / "skills"


def inventory(root):
    result = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError(f"Refusing symlink in skill: {path}")
        if path.is_file() and path.name != RECEIPT:
            if "__pycache__" in path.parts or path.suffix == ".pyc":
                continue
            result[path.relative_to(root).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def install(source, destination, update=False):
    # Preflight all skills before changing any existing installation.
    plans = []
    for name in NAMES:
        src, dst = source / name, destination / name
        if not (src / "SKILL.md").is_file():
            raise ValueError(f"Missing bundled skill: {src}")
        files = inventory(src)
        if dst.is_symlink():
            raise ValueError(f"Refusing to replace symlink: {dst}")
        if dst.exists():
            if not dst.is_dir():
                raise ValueError(f"Destination is not a directory: {dst}")
            actual = inventory(dst)
            if actual == files:
                continue
            receipt = dst / RECEIPT
            if not update or not receipt.is_file():
                raise ValueError(f"Existing skill differs: {dst}; --update only replaces an unmodified managed installation")
            if json.loads(receipt.read_text()).get("files") != actual:
                raise ValueError(f"Locally edited skill: {dst}; preserve or move it before updating")
        plans.append((src, dst, files))
    destination.mkdir(parents=True, exist_ok=True)
    for src, dst, files in plans:
        with tempfile.TemporaryDirectory(prefix=".greenbubbles-install-", dir=destination) as scratch:
            stage, backup = Path(scratch) / "new", Path(scratch) / "old"
            shutil.copytree(src, stage, ignore=shutil.ignore_patterns("__pycache__", "*.pyc", RECEIPT))
            (stage / RECEIPT).write_text(json.dumps({"version": 1, "files": files}, indent=2) + "\n")
            existed = dst.exists()
            if existed:
                dst.rename(backup)
            try:
                stage.rename(dst)
            except OSError:
                if existed:
                    backup.rename(dst)
                raise
    return [str(destination / name) for name in NAMES]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent", choices=tuple(AGENT_DIRECTORIES))
    parser.add_argument("--project", type=Path, help="install for this project instead of the current user")
    parser.add_argument("--dest", type=Path, help="custom skills directory for another compatible host")
    parser.add_argument("--update", action="store_true", help="update only unmodified installations made by this script")
    parser.add_argument("--bundle", type=Path, help="create a standalone distributable directory (must not exist)")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    source = root / "skills"
    try:
        if args.bundle:
            if args.agent or args.dest or args.project or args.update:
                parser.error("--bundle cannot be combined with install options")
            output = args.bundle.expanduser().absolute()
            if output.exists():
                raise ValueError(f"Bundle already exists: {output}")
            for name in NAMES:
                inventory(source / name)
            output.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(dir=output.parent) as scratch:
                staged = Path(scratch) / "bundle"
                (staged / "scripts").mkdir(parents=True)
                shutil.copy2(__file__, staged / "scripts" / "install-skills.py")
                for name in NAMES:
                    shutil.copytree(source / name, staged / "skills" / name,
                                    ignore=shutil.ignore_patterns("__pycache__", "*.pyc", RECEIPT))
                shutil.copy2(root / "LICENSE", staged / "LICENSE")
                (staged / "README.md").write_text(
                    "# GreenBubbles portable skills\n\nRequires Python 3 and a separately installed GreenBubbles Rust CLI.\n\n"
                    "No skill installation is required: ask your agent to read `skills/greenbubbles-personal-memory/SKILL.md`.\n"
                    "For setup, read `skills/greenbubbles-setup/SKILL.md`; references and helpers remain alongside each skill.\n"
                    "Optional discovery: `python3 scripts/install-skills.py --agent codex`\n"
                    "Targets: codex, claude, opencode, kimi, gemini, grok (Grok Build).\n"
                    "Add `--project /path/to/project` for project scope. Use `--update` for unmodified managed copies.\n"
                    "Start with greenbubbles-setup in your agent. No embedded agent is required.\n")
                staged.rename(output)
            print(json.dumps({"bundle": str(output)}))
            return
        if bool(args.agent) == bool(args.dest) or (args.dest and args.project):
            parser.error("choose --agent [--project PATH] or --dest PATH")
        destination = (args.dest.expanduser().absolute() if args.dest
                       else discovery_directory(args.agent, args.project))
        print(json.dumps({"installed": install(source, destination, args.update)}, indent=2))
    except (OSError, ValueError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
