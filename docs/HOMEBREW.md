# CLI releases and Homebrew

## Published release

[v0.4.0](https://github.com/bojieli/greenbubbles/releases/tag/v0.4.0) is available
as a research-alpha prerelease for Apple silicon and macOS 14+. Its assets include
the signed/notarized CLI ZIP, app DMG, checksums, SBOM, and notarization logs.
The CLI ZIP also includes the portable skills, optional installer, and memory driver.

The formula in [`Formula/greenbubbles.rb`](../Formula/greenbubbles.rb) pins the
published CLI ZIP by SHA-256. Compare its hash with the release's `SHA256SUMS`
asset or [GitHub's release asset listing](https://github.com/bojieli/greenbubbles/releases/expanded_assets/v0.4.0).

## Tap setup

This repository doubles as an upstream tap through its top-level `Formula/`
directory. No separate repository or cross-repository token is needed.
This is an upstream tap, not a listing in `homebrew/core`.

Install from the published tap:

```sh
brew tap bojieli/greenbubbles https://github.com/bojieli/greenbubbles.git
brew install bojieli/greenbubbles/greenbubbles
```

If Homebrew reports an untrusted formula, explicitly trust this formula and retry
the tap/install commands:

```sh
brew trust --formula bojieli/greenbubbles/greenbubbles
```

Older Homebrew versions without `brew trust` do not need this step.

The explicit remote matters because this repository is named `greenbubbles`,
not `homebrew-greenbubbles`. Requirements are Apple silicon and macOS 14+.
The formula installs the prebuilt CLI/tool set and preserves its code signatures.
It does not install the native app, capture keys, modify WeChat, or register skills.
Docs and skills included by the selected release are under
`$(brew --prefix greenbubbles)/libexec`. Point your agent at those files directly.
If that release contains `scripts/install-skills.py`, automatic discovery remains
optional:

```sh
python3 "$(brew --prefix greenbubbles)/libexec/scripts/install-skills.py" --agent opencode
```

After a newer formula is published, use `brew update` and
`brew upgrade bojieli/greenbubbles/greenbubbles`.

## Release automation

The signed release workflow still builds and notarizes the binaries. It now also:

1. Generates `greenbubbles.rb` from the final signed CLI ZIP's SHA-256.
2. Publishes the formula with the other release assets and includes it in checksums.
3. Calls the `Update Homebrew` workflow after publishing the release.

The Homebrew job fetches the exact tag's published release, downloads the CLI ZIP
and checksums, verifies the checksum and GitHub asset digest when supplied, and
updates only `Formula/greenbubbles.rb` on the default branch through GitHub's
contents API. An already-current formula is a no-op, older releases cannot
roll it back, and a changed checksum for the same version is rejected.
It uses the normal repository `GITHUB_TOKEN` with `contents: write`. If branch
protection blocks the update, the job fails visibly; it does not bypass protection.
Maintainers can apply the generated formula through their normal review process.

For an existing published numeric tag, retry just the Homebrew step after these
files are on the default branch:

```sh
gh workflow run homebrew.yml --repo bojieli/greenbubbles -f tag=v0.4.0
```

Or generate a formula locally from a signed archive:

```sh
python3 scripts/homebrew-release.py --version 0.4.0 \
  --archive /private/path/greenbubbles-0.4.0-macos-arm64.zip \
  --output /tmp/greenbubbles.rb
```

The publisher uses explicit release tags because GitHub's `releases/latest`
endpoint omits prereleases. It does not move tags or recreate binary releases.
To publish a new CLI version, use the existing signed release workflow after CI
passes and the annotated version tag points to the intended source commit.

## Validation

Run `python3 -m unittest discover -s scripts -p 'test_*.py'`,
`ruby -c Formula/greenbubbles.rb`, and `actionlint` before publication.
A network-enabled Mac can then install from the tap and run
`brew test bojieli/greenbubbles/greenbubbles`. Formula syntax and checksum tests
alone do not prove a successful download, Gatekeeper assessment, or installation.

See [Homebrew's tap documentation](https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap).
