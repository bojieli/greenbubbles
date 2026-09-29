# CLI releases and Homebrew

This page covers installing the GreenBubbles command-line tool with Homebrew,
checking a downloaded file, and upgrading. The last section is for maintainers.

You need a Mac with Apple silicon and macOS 14 or later.

## Install with Homebrew

1. Add the GreenBubbles tap and install:

   ```sh
   brew tap bojieli/greenbubbles https://github.com/bojieli/greenbubbles.git
   brew install bojieli/greenbubbles/greenbubbles
   ```

2. If Homebrew says the formula is untrusted, trust it and run the two commands
   above again. Older Homebrew versions don't have `brew trust` and don't need
   this step.

   ```sh
   brew trust --formula bojieli/greenbubbles/greenbubbles
   ```

The tap command needs the full GitHub URL because this repository is named
`greenbubbles`, not `homebrew-greenbubbles`. GreenBubbles is not in Homebrew's
main `homebrew/core` list; the formula lives in this repository's `Formula/`
folder.

### What Homebrew installs

Homebrew installs the prebuilt, signed command-line tools. It does **not**:

- install the Mac app (download the DMG from
  [Releases](https://github.com/bojieli/greenbubbles/releases) instead);
- capture your database key or change WeChat (see the
  [key setup guide](PASSPHRASE_ACQUISITION.md));
- register skills with your coding agent.

The docs and agent skills that come with the release are in
`$(brew --prefix greenbubbles)/libexec`. You can point your agent at those files
directly. If you want your agent to find the skills on its own in future
sessions, you can optionally copy them into its skills folder:

```sh
# Replace opencode with codex, claude, kimi, gemini, or grok.
python3 "$(brew --prefix greenbubbles)/libexec/scripts/install-skills.py" --agent opencode
```

## Upgrade

When a new version is published:

```sh
brew update
brew upgrade bojieli/greenbubbles/greenbubbles
```

## Download without Homebrew

Each [GitHub release](https://github.com/bojieli/greenbubbles/releases) is a
research-alpha prerelease. It includes:

- `GreenBubbles-<version>-macos-arm64.dmg`: the Mac app, signed and notarized
  by Apple;
- `greenbubbles-<version>-macos-arm64.zip`: the command-line tools, plus the
  agent skills, the optional skill installer, and the memory helper;
- `SHA256SUMS-<version>.txt`: checksums for every file;
- a software bill of materials (SBOM) and Apple notarization records.

## Check a download

Download the file you want and the `SHA256SUMS` file from the same release. The
examples below use version 0.6.0; replace it with the version you downloaded.

For the app DMG, check the checksum, then check Apple's notarization:

```sh
grep ' GreenBubbles-0.6.0-macos-arm64.dmg$' SHA256SUMS-0.6.0.txt | \
  shasum -a 256 -c -
xcrun stapler validate GreenBubbles-0.6.0-macos-arm64.dmg
```

For the command-line ZIP:

```sh
grep ' greenbubbles-0.6.0-macos-arm64.zip$' SHA256SUMS-0.6.0.txt | \
  shasum -a 256 -c -
```

`shasum` prints `OK` when the file matches.

The Homebrew formula, [`Formula/greenbubbles.rb`](../Formula/greenbubbles.rb),
also records the ZIP's SHA-256. You can compare it with the release's
`SHA256SUMS` file.

## For maintainers: publishing a release

The signed release workflow builds, signs, and notarizes the binaries. Then it:

1. Writes `greenbubbles.rb` using the SHA-256 of the final signed CLI ZIP.
2. Publishes that formula with the other release files and includes it in the
   checksums.
3. Runs the `Update Homebrew` workflow.

The `Update Homebrew` job downloads the release's CLI ZIP and checksums for
that exact tag, checks them (and GitHub's asset digest when available), and
updates only `Formula/greenbubbles.rb` on the default branch through GitHub's
contents API. It uses the repository's normal `GITHUB_TOKEN` with
`contents: write`. It is safe to rerun:

- If the formula is already current, it does nothing.
- It never rolls the formula back to an older release.
- It rejects a different checksum for a version already published.
- If branch protection blocks the update, the job fails visibly instead of
  getting around it. Maintainers can then apply the generated formula through
  normal review.

To rerun only the Homebrew step for an existing tag:

```sh
gh workflow run homebrew.yml --repo bojieli/greenbubbles -f tag=v0.6.0
```

To generate a formula on your own machine from a signed archive:

```sh
python3 scripts/homebrew-release.py --version 0.6.0 \
  --archive /private/path/greenbubbles-0.6.0-macos-arm64.zip \
  --output /tmp/greenbubbles.rb
```

The workflow uses explicit tags because GitHub's `releases/latest` ignores
prereleases. It never moves tags or rebuilds a published release. To publish a
new version, wait for CI to pass, point an annotated version tag at the right
commit, and run the signed release workflow.

### Checks before publishing

```sh
python3 -m unittest discover -s scripts -p 'test_*.py'
ruby -c Formula/greenbubbles.rb
actionlint
```

Then, on a Mac with network access, install from the tap and run
`brew test bojieli/greenbubbles/greenbubbles`. Passing syntax and checksum
tests doesn't prove that downloading, Gatekeeper, and installation work.

See [Homebrew's tap documentation](https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap).
