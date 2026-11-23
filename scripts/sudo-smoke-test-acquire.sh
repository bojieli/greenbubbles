#!/bin/bash
# Manual smoke test for greenbubbles-acquire's privilege handling under sudo.
#
# Run it as yourself from a checkout, not with sudo:
#
#     scripts/sudo-smoke-test-acquire.sh
#
# It builds greenbubbles-acquire and a small harness around the real
# SecretOutputWriter as you, then calls sudo for each privileged step (sudo
# asks for your password once). Everything uses synthetic data in a fresh
# private folder under /private/tmp: a made-up query-profiles.json and a
# synthetic SQLCipher db_storage tree whose key is the public test pattern
# below. It never reads ~/.greenbubbles, WeChat data, or the keychain, never
# attaches a debugger, and removes everything it created.
#
# Pass --no-sudo to run only the unprivileged checks.

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This smoke test is for macOS." >&2
  exit 2
fi
if [[ "${EUID}" -eq 0 ]]; then
  echo "Run this as yourself, not with sudo; it calls sudo for each privileged step." >&2
  exit 2
fi

use_sudo=1
if [[ "${1:-}" == "--no-sudo" ]]; then
  use_sudo=0
elif ! sudo -n true 2>/dev/null && [[ ! -t 0 ]]; then
  echo "note: no terminal for a sudo password; running only the unprivileged checks"
  use_sudo=0
fi

repository="$(cd "$(dirname "$0")/.." && pwd)"
me_uid="$(id -u)"
# Synthetic key: the public fixture pattern used by the unit tests.
key_hex="00112233445566778899aabbccddeeff102132435465768798a9bacbdcedfe0f"

work="$(mktemp -d /private/tmp/greenbubbles-acquire-smoke.XXXXXX)"
chmod 700 "$work"
root_probe="/private/var/root/greenbubbles-acquire-smoke-$(basename "$work")"

cleanup() {
  if [[ "$use_sudo" -eq 1 ]] && sudo -n test -e "$root_probe" 2>/dev/null; then
    sudo -n rm -rf "$root_probe" || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

passed=0
failed=0
skipped=0
pass() { echo "PASS  $1"; passed=$((passed + 1)); }
fail() { echo "FAIL  $1${2:+ — $2}"; failed=$((failed + 1)); }
skip() { echo "SKIP  $1"; skipped=$((skipped + 1)); }

# --- Build (as you) ----------------------------------------------------------

echo "Building greenbubbles-acquire from $repository ..."
if ! (cd "$repository" && swift build --product greenbubbles-acquire >"$work/build.log" 2>&1); then
  cat "$work/build.log" >&2
  exit 1
fi
binary="$(cd "$repository" && swift build --show-bin-path)/greenbubbles-acquire"

cat >"$work/harness.swift" <<'SWIFT'
import Foundation

// Writes the key read from standard input with the real SecretOutputWriter,
// to --output or to the credentialFile of --profile, as the invoking user.
@main
struct SmokeHarness {
  static func main() {
    do {
      try run()
    } catch {
      FileHandle.standardError.write(Data("harness: \(error)\n".utf8))
      exit(1)
    }
  }

  static func run() throws {
    var profile: String?
    var output: URL?
    var overwrite = false
    var arguments = Array(CommandLine.arguments.dropFirst())
    while !arguments.isEmpty {
      let option = arguments.removeFirst()
      switch option {
      case "--profile": profile = arguments.removeFirst()
      case "--output": output = URL(fileURLWithPath: arguments.removeFirst())
      case "--overwrite": overwrite = true
      default: throw CocoaError(.featureUnsupported)
      }
    }
    let user = try InvokingUser.current()
    let destination: URL
    if let output {
      destination = output
    } else {
      let locations = try QueryProfileTarget.locations(
        environment: ProcessInfo.processInfo.environment, home: user.homeDirectory)
      let target = try user.perform {
        try QueryProfileTarget.resolve(
          requestedProfile: profile, locations: locations, ownerUID: user.uid)
      }
      guard let file = target.credentialFile else { throw CocoaError(.fileNoSuchFile) }
      destination = file
    }
    try SecretOutputWriter.checkDestination(destination)
    let secret = try PassphraseSecret.readFromStandardInput()
    try SecretOutputWriter().write(secret, to: destination, overwrite: overwrite, as: user)
    print("harness: wrote as uid \(user.uid) (process euid \(geteuid()))")
  }
}
SWIFT
sources="$repository/Sources/GreenBubblesAcquire"
swiftc -parse-as-library -o "$work/harness" "$work/harness.swift" \
  "$sources/PassphraseSecret.swift" "$sources/InvokingUser.swift" \
  "$sources/SecretOutputWriter.swift" "$sources/QueryProfileTarget.swift" \
  "$sources/PrivateConfigurationFile.swift" "$sources/JSONDuplicateKeyCheck.swift"
harness="$work/harness"

# --- Synthetic data (as you) -------------------------------------------------

mkdir -m 700 "$work/cfg" "$work/account" "$work/keys"
database_root="$work/account/db_storage"
mkdir -m 700 "$database_root"
cat >"$work/make_fixture.py" <<'PYTHON'
import hashlib, hmac, os, struct, sys

root, key_hex = sys.argv[1], sys.argv[2]
password = bytes.fromhex(key_hex)
for index, component in enumerate(["contact", "session", "message"]):
    salt = bytes(((index + 1) * 17 + offset) & 0xFF for offset in range(16))
    page = bytearray(4096)
    page[0:16] = salt
    for offset in range(16, 4032):
        page[offset] = (offset * 31 + 7 + index) & 0xFF
    encryption_key = hashlib.pbkdf2_hmac("sha512", password, salt, 256000, 32)
    mac_key = hashlib.pbkdf2_hmac("sha512", encryption_key, bytes(b ^ 0x3A for b in salt), 2, 32)
    page[4032:4096] = hmac.new(mac_key, bytes(page[16:4032]) + struct.pack("<I", 1), "sha512").digest()
    os.makedirs(os.path.join(root, component), mode=0o700)
    with open(os.path.join(root, component, component + ".db"), "wb") as handle:
        handle.write(page)
PYTHON
python3 -I "$work/make_fixture.py" "$database_root" "$key_hex"
printf '%s\n' "$key_hex" >"$work/synthetic.key"
chmod 600 "$work/synthetic.key"

profiles="$work/cfg/query-profiles.json"
cat >"$profiles" <<JSON
{
  "schema": "greenbubbles.query-profiles.v1",
  "formatVersion": 1,
  "defaultProfile": "smoke",
  "profiles": {
    "smoke": {
      "sourceRoot": "$database_root",
      "access": { "mode": "liveWeChatKeyFile", "credentialFile": "$work/keys/new/live.key" }
    },
    "rootdir": {
      "sourceRoot": "$database_root",
      "access": { "mode": "liveWeChatKeyFile", "credentialFile": "$root_probe/live.key" }
    }
  }
}
JSON
chmod 600 "$profiles"
export GREENBUBBLES_QUERY_PROFILES_FILE="$profiles"
export GREENBUBBLES_CONFIG_FILE="$work/cfg/config.toml"  # deliberately absent

# Always pass the variables explicitly to root as well, so a sudo policy that
# drops them can never make a step read the real ~/.greenbubbles.
as_root() {
  sudo --preserve-env=GREENBUBBLES_QUERY_PROFILES_FILE,GREENBUBBLES_CONFIG_FILE \
    /usr/bin/env "GREENBUBBLES_QUERY_PROFILES_FILE=$profiles" \
    "GREENBUBBLES_CONFIG_FILE=$work/cfg/config.toml" "$@"
}

owner_and_mode() { stat -f '%u %Lp' "$1"; }

report_has_no_permission_problem() {
  python3 -I -c '
import json, sys
report = json.load(open(sys.argv[1]))
bad = [b for b in report["blockers"]
       if any(word in b.lower() for word in ("permission", "not safe", "not usable", "posix error", "query profile file"))]
ok = report.get("databaseCount") == 3 and not bad
print("databaseCount=%s blockers=%s" % (report.get("databaseCount"), report["blockers"]))
sys.exit(0 if ok else 1)
' "$1"
}

# --- Unprivileged checks -----------------------------------------------------

if "$binary" verify --passphrase-stdin --profile smoke <"$work/synthetic.key" >"$work/u1.out" 2>&1; then
  pass "user: verify reads the profile and matches all synthetic databases"
else
  fail "user: verify" "$(tr '\n' ' ' <"$work/u1.out")"
fi

"$binary" preflight --json --profile smoke >"$work/u2.json" 2>"$work/u2.err" || true
if report_has_no_permission_problem "$work/u2.json" >"$work/u2.out" 2>&1; then
  pass "user: preflight --json reads the profile and inventories 3 databases"
else
  fail "user: preflight --json" "$(tr '\n' ' ' <"$work/u2.out")"
fi

if "$harness" --output "$work/keys/user-run/nested/k.key" <"$work/synthetic.key" >/dev/null 2>"$work/u3.err" \
  && [[ "$(owner_and_mode "$work/keys/user-run/nested/k.key")" == "$me_uid 600" ]] \
  && [[ "$(owner_and_mode "$work/keys/user-run")" == "$me_uid 700" ]]; then
  pass "user: writer creates 0700 folders and a 0600 key"
else
  fail "user: writer" "$(tr '\n' ' ' <"$work/u3.err")"
fi

# --- sudo checks ---------------------------------------------------------------

if [[ "$use_sudo" -eq 0 ]]; then
  for check in "sudo: --preserve-env passes the profile location" \
    "sudo (a): preflight reads the profile as you" "sudo (a): verify reads databases as you" \
    "sudo (b): key written from the profile is yours" "sudo (c): root-owned folder refused" \
    "sudo (d): root-owned file not overwritten"; do
    skip "$check"
  done
else
  echo "sudo will ask for your password once if needed."
  sudo -v

  seen="$(sudo --preserve-env=GREENBUBBLES_QUERY_PROFILES_FILE /usr/bin/printenv GREENBUBBLES_QUERY_PROFILES_FILE || true)"
  if [[ "$seen" == "$profiles" ]]; then
    pass "sudo: --preserve-env passes the profile location"
  else
    fail "sudo: --preserve-env passes the profile location" "root saw '${seen}'"
  fi

  # (a) Profile and databases are read as you; only "WeChat not running" or
  # similar may block.
  as_root "$binary" preflight --json --profile smoke >"$work/a.json" 2>"$work/a.err" || true
  if report_has_no_permission_problem "$work/a.json" >"$work/a.out" 2>&1 \
    && python3 -I -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1]))["runningAsRoot"] else 1)' "$work/a.json"; then
    pass "sudo (a): preflight reads the profile as you ($(tr '\n' ' ' <"$work/a.out"))"
  else
    fail "sudo (a): preflight" "$(cat "$work/a.out" "$work/a.err" | tr '\n' ' ')"
  fi
  if as_root "$binary" verify --passphrase-stdin --profile smoke <"$work/synthetic.key" >"$work/a2.out" 2>&1; then
    pass "sudo (a): verify reads databases as you"
  else
    fail "sudo (a): verify" "$(tr '\n' ' ' <"$work/a2.out")"
  fi

  # (b) The profile's credentialFile, in folders that don't exist yet.
  if as_root "$harness" --profile smoke <"$work/synthetic.key" >"$work/b.out" 2>&1 \
    && [[ "$(owner_and_mode "$work/keys/new/live.key")" == "$me_uid 600" ]] \
    && [[ "$(owner_and_mode "$work/keys/new")" == "$me_uid 700" ]] \
    && [[ "$(cat "$work/keys/new/live.key")" == "$key_hex" ]] \
    && "$binary" verify --passphrase-stdin --profile smoke <"$work/keys/new/live.key" >/dev/null 2>&1; then
    pass "sudo (b): key written from the profile is yours, 0600 in a new 0700 folder, and verifies"
  else
    fail "sudo (b): profile write" "$(tr '\n' ' ' <"$work/b.out"); $(ls -ln "$work/keys/new" 2>&1 | tr '\n' ' ')"
  fi

  # (c) A credentialFile inside root's home must fail with a permission error
  # and create nothing there.
  if as_root "$harness" --profile rootdir <"$work/synthetic.key" >"$work/c.out" 2>&1; then
    fail "sudo (c): root-owned folder refused" "the write succeeded"
  elif grep -q "POSIX error 13" "$work/c.out" && ! sudo test -e "$root_probe"; then
    pass "sudo (c): root-owned folder refused with a permission error; nothing created"
  else
    fail "sudo (c): root-owned folder refused" "$(tr '\n' ' ' <"$work/c.out"); exists=$(sudo test -e "$root_probe" && echo yes || echo no)"
  fi

  # (d) --overwrite onto root-owned files in your folder: 0644 (not writable
  # by you) and 0666 (writable, but owned by root) must both stay untouched.
  for mode in 644 666; do
    target="$work/keys/root-owned-$mode.key"
    sudo /bin/sh -c 'printf "root-owned original\n" > "$1" && chmod "$2" "$1"' sh "$target" "$mode"
    before="$(owner_and_mode "$target")"
    if as_root "$harness" --output "$target" --overwrite <"$work/synthetic.key" >"$work/d.out" 2>&1; then
      fail "sudo (d): root-owned $mode file not overwritten" "the write succeeded"
    elif [[ "$(cat "$target")" == "root-owned original" && "$(owner_and_mode "$target")" == "$before" && "$before" == "0 $mode" ]]; then
      pass "sudo (d): root-owned $mode file not overwritten ($(tr '\n' ' ' <"$work/d.out"))"
    else
      fail "sudo (d): root-owned $mode file not overwritten" "now $(owner_and_mode "$target"): $(cat "$target")"
    fi
  done
fi

echo ""
echo "passed: $passed  failed: $failed  skipped: $skipped"
[[ "$failed" -eq 0 ]]
