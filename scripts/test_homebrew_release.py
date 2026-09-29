"""Pin release artifacts and prevent unsafe or regressive tap updates."""
import base64
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import re
import json
from unittest.mock import patch
import unittest

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("homebrew_release", ROOT / "scripts/homebrew-release.py")
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


class HomebrewRelease(unittest.TestCase):
    def existing(self, formula):
        return {"sha": "existing-blob", "content": base64.b64encode(formula.encode()).decode()}

    def test_checked_in_formula_matches_generator(self):
        formula = (ROOT / "Formula/greenbubbles.rb").read_text()
        version = release.formula_version(formula)
        digest = re.search(r'^  sha256 "([^"]+)"$', formula, re.MULTILINE)[1]
        self.assertEqual(formula, release.render(version, digest, "bojieli/greenbubbles"))

    def test_legacy_explicit_version_must_agree_with_download(self):
        formula = release.render("0.3.0", "a" * 64, "bojieli/greenbubbles")
        legacy = formula + '  version "0.3.0"\n'
        self.assertEqual(release.formula_version(legacy), "0.3.0")
        self.assertIsNotNone(release.publication_payload(self.existing(legacy), formula, "0.3.0", "main"))
        with self.assertRaisesRegex(ValueError, "consistent release version"):
            release.formula_version(formula + '  version "0.2.0"\n')

    def test_publisher_verifies_downloads_before_updating_only_formula(self):
        archive_name = "greenbubbles-0.3.1-macos-arm64.zip"
        checksum_name = "SHA256SUMS-0.3.1.txt"
        content = b"synthetic signed release"
        digest = hashlib.sha256(content).hexdigest()
        calls = []
        old = release.render("0.3.0", "b" * 64, "bojieli/greenbubbles")
        def fake_gh(*args, payload=None):
            calls.append((args, payload))
            if args[0] == "release":
                directory = Path(args[args.index("--dir") + 1])
                (directory / archive_name).write_bytes(content)
                (directory / checksum_name).write_text(f"{digest}  {archive_name}\n")
                return ""
            if "releases/tags" in args[1]:
                return json.dumps({"draft": False, "tag_name": "v0.3.1", "assets": [
                    {"name": archive_name, "digest": f"sha256:{digest}"}, {"name": checksum_name}]})
            if args[1].endswith("/contents/Formula/greenbubbles.rb"):
                return "{}" if payload else json.dumps(self.existing(old))
            return '{"default_branch":"main"}'
        with patch.object(release, "gh", side_effect=fake_gh), patch("builtins.print"):
            release.publish("0.3.1", "bojieli/greenbubbles")
        writes = [(args, payload) for args, payload in calls if payload is not None]
        self.assertEqual(len(writes), 1)
        args, payload = writes[0]
        self.assertEqual(args, ("api", "repos/bojieli/greenbubbles/contents/Formula/greenbubbles.rb", "-X", "PUT"))
        self.assertIn(digest, base64.b64decode(payload["content"]).decode())
        self.assertEqual(payload["branch"], "main")

    def test_draft_release_does_not_download_or_publish(self):
        with patch.object(release, "gh", return_value='{"draft":true,"tag_name":"v0.3.1"}') as gh:
            with self.assertRaisesRegex(ValueError, "published release"):
                release.publish("0.3.1", "bojieli/greenbubbles")
            self.assertEqual(gh.call_count, 1)

    def test_archive_digest_must_match_one_checksum_entry(self):
        with tempfile.TemporaryDirectory() as temporary:
            archive = Path(temporary) / "greenbubbles-0.3.0-macos-arm64.zip"
            archive.write_bytes(b"synthetic archive content")
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            checksums = Path(temporary) / "SHA256SUMS.txt"
            line = f"{digest}  {archive.name}\n"
            checksums.write_text(line)
            self.assertEqual(release.verify_checksum(archive, checksums), digest)
            for invalid in ("", line + line, f"{'0' * 64}  {archive.name}\n"):
                checksums.write_text(invalid)
                with self.assertRaises(ValueError):
                    release.verify_checksum(archive, checksums)

    def test_update_is_idempotent_and_never_downgrades(self):
        formula = release.render("0.3.1", "a" * 64, "bojieli/greenbubbles")
        self.assertIsNone(release.publication_payload(self.existing(formula), formula, "0.3.1", "main"))
        older = release.render("0.3.0", "b" * 64, "bojieli/greenbubbles")
        self.assertIsNone(release.publication_payload(self.existing(formula), older, "0.3.0", "main"))
        update = release.publication_payload(self.existing(older), formula, "0.3.1", "main")
        self.assertEqual(update["sha"], "existing-blob")
        self.assertEqual(base64.b64decode(update["content"]).decode(), formula)

    def test_same_version_cannot_silently_change_release_checksum(self):
        old = release.render("0.3.0", "a" * 64, "bojieli/greenbubbles")
        new = release.render("0.3.0", "b" * 64, "bojieli/greenbubbles")
        with self.assertRaisesRegex(ValueError, "checksum changed"):
            release.publication_payload(self.existing(old), new, "0.3.0", "main")

    def test_identifiers_cannot_inject_ruby_or_paths(self):
        for version, digest, repo in (("0.3.0\"", "a" * 64, "bojieli/greenbubbles"),
                                       ("0.3.0", "invalid", "bojieli/greenbubbles"),
                                       ("0.3.0", "a" * 64, "bojieli/greenbubbles/../../")):
            with self.assertRaises(ValueError):
                release.render(version, digest, repo)


if __name__ == "__main__":
    unittest.main()
