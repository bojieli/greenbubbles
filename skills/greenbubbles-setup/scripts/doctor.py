#!/usr/bin/env python3
"""Read-only capability check. Never prints subprocess output or credentials."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess


def probe(binary, args):
    try:
        result = subprocess.run([binary, *args], stdin=subprocess.DEVNULL,
                                capture_output=True, text=True, timeout=30)
        return result.returncode, result.stdout
    except (OSError, subprocess.TimeoutExpired):
        return None, ""


def check(binary, profile=None):
    resolved = shutil.which(binary)
    if not resolved:
        return {"ok": False, "checks": {"cli": "missing"}, "next": "Install the GreenBubbles Rust CLI or pass --greenbubbles /absolute/path."}
    code, output = probe(resolved, ["memory", "--help"])
    compatible = code == 0 and all(word in output for word in ("prepare", "next", "page", "acknowledge", "commit", "--format"))
    checks = {"cli": "compatible" if compatible else "unsupported-memory-cli"}
    if compatible:
        code, output = probe(resolved, ["profile", "validate", *([profile] if profile else [])])
        try:
            payload = json.loads(output)
            valid = code == 0 and payload.get("ok") is True and payload.get("schema") == "greenbubbles.query-profile-validation.v1"
        except (ValueError, AttributeError):
            valid = False
        checks["source"] = "ready" if valid else "profile-or-source-unavailable"
    ready = compatible and checks.get("source") == "ready"
    return {"ok": ready, "binary": str(Path(resolved).resolve()), "checks": checks,
            "next": "Use the context or personal-memory skill." if ready else "Read references/setup.md; inspect CLI help and profile validation locally. No capture was attempted."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--greenbubbles", default=os.environ.get("GREENBUBBLES_CLI", "greenbubbles"))
    parser.add_argument("--profile")
    args = parser.parse_args()
    result = check(args.greenbubbles, args.profile)
    print(json.dumps(result, indent=2))
    raise SystemExit(0 if result["ok"] else 1)


if __name__ == "__main__":
    main()
