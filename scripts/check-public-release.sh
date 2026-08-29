#!/usr/bin/env bash

set -euo pipefail

failures=0

fail() {
  echo "error: $*" >&2
  failures=$((failures + 1))
}

require_file() {
  local path="$1"
  if [[ ! -f "$path" || -L "$path" || ! -s "$path" ]]; then
    echo "error: required public-release file is missing, empty, or not a regular file: $path" >&2
    failures=$((failures + 1))
  fi
}

require_file LICENSE
require_file THIRD_PARTY_NOTICES.md
require_file NOTICE.md
require_file CHANGELOG.md
require_file PRIVACY.md
require_file SECURITY.md
require_file assets/greenbubbles-icon.svg
require_file assets/how-it-works.svg
require_file docs/README.md
require_file docs/KNOWN_LIMITATIONS.md
require_file docs/THREAT_MODEL.md
require_file docs/DISTRIBUTION_INVENTORY.md
require_file docs/PUBLIC_RELEASE_CHECKLIST.md
require_file Native/GreenBubbles/about.toml
require_file Native/GreenBubbles/about.hbs

if ! grep -Eq '^license[[:space:]]*=[[:space:]]*"MIT"' Native/GreenBubbles/Cargo.toml; then
  fail "Rust package metadata must declare MIT"
fi

if ! grep -Fq "MIT License" LICENSE \
  || ! grep -Fq "Copyright (c) 2026 Bojie Li" LICENSE; then
  fail "LICENSE is not the approved GreenBubbles MIT text"
fi

for required_notice in \
  "Copyright (c) 2008-2020 Zetetic LLC" \
  "Copyright (c) 2006-2012, Skype Limited" \
  "NO EXPRESS OR IMPLIED LICENSES TO ANY PARTY'S PATENT RIGHTS" \
  "Copyright (c) Meta Platforms, Inc. and affiliates" \
  "Copyright (c) 2025 CloudDreamAI / TANGandXUE" \
  'wx-context 0.7.4' \
  'Mozilla Public License Version 2.0'; do
  if ! grep -Fq "$required_notice" THIRD_PARTY_NOTICES.md; then
    fail "third-party notice bundle is missing: $required_notice"
  fi
done

for stale_text in \
  "No project-wide public license has been selected yet" \
  "not yet a complete public binary notice bundle" \
  "Repository state: private; no public-release approval" \
  "Status: **not ready for public release**" \
  "Public binary distribution is not yet approved"; do
  if grep -Fq "$stale_text" README.md NOTICE.md CONTRIBUTING.md \
    docs/DISTRIBUTION_INVENTORY.md docs/PUBLIC_RELEASE_CHECKLIST.md; then
    fail "public-facing documentation contains stale release state: $stale_text"
  fi
done

for workflow_requirement in \
  'secrets.APPLE_CERTIFICATE_P12_BASE64' \
  'secrets.APPLE_CERTIFICATE_PASSWORD' \
  'secrets.APPLE_SIGNING_IDENTITY' \
  'secrets.APPLE_TEAM_ID' \
  'secrets.APPLE_NOTARY_KEY_P8' \
  'secrets.APPLE_NOTARY_KEY_ID' \
  'secrets.APPLE_NOTARY_ISSUER_ID' \
  'codesign --verify --strict' \
  'xcrun notarytool submit' \
  'xcrun stapler validate' \
  'gh release create'; do
  if ! grep -Fq "$workflow_requirement" .github/workflows/release.yml; then
    fail "release workflow is missing: $workflow_requirement"
  fi
done

if ! grep -Fq "Developer ID signed and Apple notarized" README.md; then
  fail "README does not describe the signed and notarized release"
fi

if ! grep -Fq "selected MIT and explicitly authorized" docs/DISTRIBUTION_INVENTORY.md; then
  fail "distribution inventory does not record the owner's MIT decision"
fi

release_tag=
if (( $# > 0 )); then
  release_tag=$1
fi

if [[ -n "$release_tag" ]]; then
  cargo_version=$(awk -F '"' '/^version[[:space:]]*=/ { print $2; exit }' Native/GreenBubbles/Cargo.toml)
  if [[ "$release_tag" != "v$cargo_version" ]]; then
    fail "release tag $release_tag does not match Cargo version v$cargo_version"
  fi
  if [[ $(git cat-file -t "$release_tag" 2>/dev/null || true) != "tag" ]]; then
    fail "release tag must exist locally and be annotated: $release_tag"
  elif [[ $(git rev-list -n 1 "$release_tag") != $(git rev-parse HEAD) ]]; then
    fail "release tag $release_tag does not point to the checked-out commit"
  fi
  if ! grep -Fq "## $cargo_version -" CHANGELOG.md; then
    fail "CHANGELOG has no dated $cargo_version release entry"
  fi
fi

if (( failures > 0 )); then
  echo "public binary release preflight failed with $failures issue(s)" >&2
  exit 1
fi

echo "public binary release prerequisites are present"
