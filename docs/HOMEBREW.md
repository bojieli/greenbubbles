# CLI releases and Homebrew

## Verified public artifacts

The public release checked on 2026-09-29 is
[v0.3.0](https://github.com/bojieli/greenbubbles/releases/tag/v0.3.0), a prerelease.
Its assets include the signed/notarized Apple-silicon CLI ZIP, app DMG, checksums,
SBOM, and notarization logs. The public v0.3.1 release URL returned 404 during
this check; a local tag or Cargo version is not evidence of a published download.

The seed formula in `Formula/greenbubbles.rb` uses the published CLI ZIP:

- Asset: `greenbubbles-0.3.0-macos-arm64.zip`
- SHA-256: `017767b9d2c1f32039eba06f531e335b74fe8a1dfd19100c1cb7b89d49bba22a`
- Source: [GitHub's release asset listing](https://github.com/bojieli/greenbubbles/releases/expanded_assets/v0.3.0).

The portable installer and expanded skill support are prepared for v0.4.0,
not features claimed for that older binary release. For the updated memory
protocol/help, build the current source until a release containing it is published.

## Tap setup

This repository doubles as an upstream tap through its top-level `Formula/`
directory. No separate repository or cross-repository token is needed.
This is an upstream tap, not a listing in `homebrew/core`.

**Publication prerequisite:** merge/push `Formula/greenbubbles.rb` and the new
workflows to the public repository before advertising these commands as live.
Preparation in a local checkout alone does not make a Homebrew package available.

Once the formula is published on the repository's default branch:

```sh
brew tap bojieli/greenbubbles https://github.com/bojieli/greenbubbles.git
brew install bojieli/greenbubbles/greenbubbles
```

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
