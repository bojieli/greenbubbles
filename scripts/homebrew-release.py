#!/usr/bin/env python3
"""Generate a pinned Homebrew formula; optionally publish it from a verified release.

No release/latest lookup: GreenBubbles currently publishes prereleases. Publication
uses GitHub's contents API and changes only Formula/greenbubbles.rb, on the default
branch or, with --branch, on a staging branch reset to the default branch's tip.
The default branch requires a passing CI check, so the release workflow stages the
commit, runs CI on it, then fast-forwards the default branch. It never creates or
replaces a binary release.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
TEMPLATE = ROOT / "Packaging/Homebrew/greenbubbles.rb.in"


def version_tuple(version):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("expected a numeric major.minor.patch release version")
    return tuple(map(int, version.split(".")))


def render(version, digest, repository):
    version_tuple(version)
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise ValueError("expected a lowercase SHA-256 digest")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("expected an owner/repository name")
    return (TEMPLATE.read_text().replace("@VERSION@", version)
            .replace("@SHA256@", digest).replace("@REPOSITORY@", repository))


def archive_digest(archive):
    digest = hashlib.sha256()
    with archive.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_checksum(archive, checksum_file):
    matches = []
    for line in checksum_file.read_text().splitlines():
        match = re.fullmatch(r"([0-9a-f]{64}) [ *](.+)", line)
        if match and match[2] == archive.name:
            matches.append(match[1])
    actual = archive_digest(archive)
    if matches != [actual]:
        raise ValueError("CLI archive is missing from checksums, duplicated, or has a mismatched digest")
    return actual


def gh(*args, payload=None):
    command = ["gh", *args]
    if payload is not None:
        command += ["--input", "-"]
    result = subprocess.run(command, input=json.dumps(payload) if payload is not None else None,
                            capture_output=True, text=True, check=True)
    return result.stdout


def formula_version(content):
    # Homebrew infers the version from this URL; an explicit version is redundant.
    url = re.search(
        r'^  url "https://github.com/[^/]+/[^/]+/releases/download/v([0-9]+\.[0-9]+\.[0-9]+)/greenbubbles-\1-macos-arm64\.zip"$',
        content, re.MULTILINE)
    declared = re.search(r'^  version "([^"]+)"$', content, re.MULTILINE)
    if not url or (declared and declared[1] != url[1]):
        raise ValueError("existing formula has no consistent release version; refusing to replace it")
    return url[1]


def publication_payload(existing, formula, version, branch):
    """Never roll a tap back or silently replace a same-version release hash."""
    content = base64.b64decode(existing["content"]).decode()
    previous = version_tuple(formula_version(content))
    desired = version_tuple(version)
    if previous > desired:
        return None
    if previous == desired:
        old_hash = re.search(r'^  sha256 "([0-9a-f]{64})"$', content, re.MULTILINE)
        new_hash = re.search(r'^  sha256 "([0-9a-f]{64})"$', formula, re.MULTILINE)
        if not old_hash or old_hash[1] != new_hash[1]:
            raise ValueError("same-version release checksum changed; refusing replacement")
        if content == formula:
            return None
    return {"message": f"brew: update GreenBubbles to {version}", "branch": branch,
            "sha": existing["sha"], "content": base64.b64encode(formula.encode()).decode()}


def validate_branch(branch):
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*(/[A-Za-z0-9][A-Za-z0-9._-]*)*", branch) or ".." in branch:
        raise ValueError("invalid staging branch name")


def stage_branch(repository, base_branch, branch):
    """Point the staging branch at the base branch's tip, creating or resetting it."""
    base = json.loads(gh("api", f"repos/{repository}/git/ref/heads/{base_branch}"))["object"]["sha"]
    try:
        gh("api", f"repos/{repository}/git/refs", "-X", "POST",
           payload={"ref": f"refs/heads/{branch}", "sha": base})
    except subprocess.CalledProcessError:
        # Left over from an earlier attempt; a staging branch is never shared.
        gh("api", f"repos/{repository}/git/refs/heads/{branch}", "-X", "PATCH",
           payload={"sha": base, "force": True})


def report_commit(commit):
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a") as handle:
            handle.write(f"commit={commit}\n")


def publish(version, repository, staging_branch=None):
    tag = f"v{version}"
    release = json.loads(gh("api", f"repos/{repository}/releases/tags/{tag}"))
    if release.get("draft") or release.get("tag_name") != tag:
        raise ValueError("expected a published release for the exact requested tag")
    archive_name = f"greenbubbles-{version}-macos-arm64.zip"
    checksums_name = f"SHA256SUMS-{version}.txt"
    assets = {asset["name"]: asset for asset in release["assets"]}
    if archive_name not in assets or checksums_name not in assets:
        raise ValueError("published release lacks the CLI ZIP or checksums")
    with tempfile.TemporaryDirectory(prefix="greenbubbles-homebrew-") as temporary:
        directory = Path(temporary)
        gh("release", "download", tag, "--repo", repository,
           "--pattern", archive_name, "--pattern", checksums_name, "--dir", temporary)
        digest = verify_checksum(directory / archive_name, directory / checksums_name)
        github_digest = assets[archive_name].get("digest")
        if github_digest and github_digest != f"sha256:{digest}":
            raise ValueError("GitHub asset digest does not match the downloaded CLI archive")
    formula = render(version, digest, repository)
    repo = json.loads(gh("api", f"repos/{repository}"))
    branch = repo["default_branch"]
    if staging_branch:
        stage_branch(repository, branch, staging_branch)
        branch = staging_branch
    # The seed formula must first be merged with this workflow. Reading failure
    # (auth/network/missing file) must never be mistaken for an empty repository.
    existing = json.loads(gh("api", f"repos/{repository}/contents/Formula/greenbubbles.rb",
                             "-X", "GET", "-f", f"ref={branch}"))
    payload = publication_payload(existing, formula, version, branch)
    if payload is None:
        print("Homebrew formula is already current or newer; no change")
        report_commit("")
        return
    result = gh("api", f"repos/{repository}/contents/Formula/greenbubbles.rb", "-X", "PUT", payload=payload)
    commit = json.loads(result)["commit"]["sha"]
    report_commit(commit)
    print(f"Published Formula/greenbubbles.rb for {tag} on {branch} as {commit}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--repository", default="bojieli/greenbubbles")
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--archive", type=Path, help="hash a local signed CLI ZIP")
    source.add_argument("--sha256", help="digest already verified from a published release")
    source.add_argument("--publish", action="store_true", help="verify published assets and update the default-branch formula via gh")
    parser.add_argument("--branch", help="with --publish, commit to this staging branch instead of the default branch")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        # Validate identifiers before using them in subprocess or API arguments.
        render(args.version, "0" * 64, args.repository)
        if args.publish:
            if args.output:
                parser.error("--output cannot be combined with --publish")
            if args.branch:
                validate_branch(args.branch)
            publish(args.version, args.repository, args.branch)
        elif args.branch:
            parser.error("--branch requires --publish")
        else:
            formula = render(args.version, archive_digest(args.archive) if args.archive else args.sha256,
                             args.repository)
            if args.output:
                args.output.parent.mkdir(parents=True, exist_ok=True)
                args.output.write_text(formula)
            else:
                print(formula, end="")
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Homebrew release failed: {error}\n")


if __name__ == "__main__":
    main()
