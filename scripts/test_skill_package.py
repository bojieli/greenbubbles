"""Portable installation, conflict preservation, and content-free diagnostics."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


installer = load("installer", ROOT / "scripts/install-skills.py")
doctor = load("doctor", ROOT / "skills/greenbubbles-setup/scripts/doctor.py")


class PortableInstallation(unittest.TestCase):
    def test_bundle_installs_after_relocation_without_checkout(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            bundle = base / "bundle"
            subprocess.run([sys.executable, str(ROOT / "scripts/install-skills.py"),
                            "--bundle", str(bundle)], check=True, capture_output=True)
            relocated = base / "folder with spaces"
            bundle.rename(relocated)
            for agent, directory in (("codex", ".agents"), ("claude", ".claude"),
                                     ("opencode", ".opencode"), ("kimi", ".kimi-code"),
                                     ("gemini", ".gemini"), ("grok", ".grok")):
                project = base / agent
                command = [sys.executable, str(relocated / "scripts/install-skills.py"),
                           "--agent", agent, "--project", str(project)]
                subprocess.run(command, cwd=base, check=True, capture_output=True)
                subprocess.run(command, cwd=base, check=True, capture_output=True)
                installed = project / directory / "skills"
                for name in installer.NAMES:
                    self.assertEqual(installer.inventory(relocated / "skills" / name),
                                     installer.inventory(installed / name))
                result = subprocess.run([sys.executable,
                    str(installed / "greenbubbles-setup/scripts/doctor.py"),
                    "--greenbubbles", str(base / "missing")], capture_output=True, text=True)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(json.loads(result.stdout)["checks"]["cli"], "missing")

    def test_user_discovery_paths_and_kimi_override(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            with patch.object(installer.Path, "home", return_value=home), \
                 patch.dict(installer.os.environ, {}, clear=True):
                for agent, relative in {
                    "codex": ".agents/skills", "claude": ".claude/skills",
                    "opencode": ".config/opencode/skills", "kimi": ".kimi-code/skills",
                    "gemini": ".gemini/skills", "grok": ".grok/skills",
                }.items():
                    self.assertEqual(installer.discovery_directory(agent), home / relative)
            with patch.dict(installer.os.environ, {"KIMI_CODE_HOME": str(home / "isolated") }):
                self.assertEqual(installer.discovery_directory("kimi"), home / "isolated/skills")
                self.assertEqual(installer.discovery_directory("kimi", home / "project"),
                                 home / "project/.kimi-code/skills")

    def test_update_preserves_local_changes_and_preflights_all_destinations(self):
        with tempfile.TemporaryDirectory() as temporary:
            import shutil
            root = Path(temporary)
            source, dest = root / "source", root / "installed"
            shutil.copytree(ROOT / "skills", source)
            installer.install(source, dest)
            changed = source / installer.NAMES[0] / "SKILL.md"
            changed.write_text(changed.read_text() + "\nNew upstream guidance.\n")
            local = dest / installer.NAMES[-1] / "SKILL.md"
            local.write_text(local.read_text() + "\nMy customization.\n")
            original = (dest / installer.NAMES[0] / "SKILL.md").read_text()
            with self.assertRaisesRegex(ValueError, "Locally edited"):
                installer.install(source, dest, update=True)
            self.assertEqual((dest / installer.NAMES[0] / "SKILL.md").read_text(), original)
            self.assertIn("My customization", local.read_text())
            local.write_text((source / installer.NAMES[-1] / "SKILL.md").read_text())
            installer.install(source, dest, update=True)
            self.assertEqual((dest / installer.NAMES[0] / "SKILL.md").read_text(), changed.read_text())

    def test_unmanaged_directory_and_symlink_are_not_replaced(self):
        with tempfile.TemporaryDirectory() as temporary:
            dest = Path(temporary)
            conflict = dest / installer.NAMES[0]
            conflict.mkdir()
            (conflict / "mine").write_text("keep")
            with self.assertRaises(ValueError):
                installer.install(ROOT / "skills", dest, update=True)
            self.assertEqual((conflict / "mine").read_text(), "keep")
            (conflict / "mine").unlink()
            conflict.rmdir()
            conflict.symlink_to(ROOT / "skills" / installer.NAMES[0], target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symlink"):
                installer.install(ROOT / "skills", dest, update=True)


class Diagnostics(unittest.TestCase):
    def test_failure_does_not_release_subprocess_output(self):
        secret = "PRIVATE-SENTINEL"
        with patch.object(doctor.shutil, "which", return_value="/fake/greenbubbles"), \
             patch.object(doctor, "probe", side_effect=[
                 (0, "prepare next page acknowledge commit --format"), (1, secret)]):
            result = doctor.check("greenbubbles", "live")
        self.assertFalse(result["ok"])
        self.assertNotIn(secret, json.dumps(result))

    def test_live_source_status_is_enough_without_a_profile(self):
        with patch.object(doctor.shutil, "which", return_value="/fake/greenbubbles"), \
             patch.object(doctor, "probe", side_effect=[
                 (0, "prepare next page acknowledge commit --format"),
                 (0, '{"ok":true,"schema":"greenbubbles.query.v1"}')]) as probe:
            self.assertTrue(doctor.check("greenbubbles")["ok"])
            self.assertEqual(probe.call_args.args[1], ["source", "status"])

    def test_named_profile_uses_source_status(self):
        with patch.object(doctor.shutil, "which", return_value="/fake/greenbubbles"), \
             patch.object(doctor, "probe", side_effect=[
                 (0, "prepare next page acknowledge commit --format"),
                 (0, '{"ok":true,"schema":"greenbubbles.query.v1"}')]) as probe:
            self.assertTrue(doctor.check("greenbubbles", "archive")["ok"])
            self.assertEqual(probe.call_args.args[1], ["source", "status", "--profile", "archive"])

    def test_wrong_cli_does_not_try_source(self):
        with patch.object(doctor.shutil, "which", return_value="/fake/greenbubbles"), \
             patch.object(doctor, "probe", return_value=(0, "Swift discovery")) as probe:
            self.assertFalse(doctor.check("greenbubbles")["ok"])
            self.assertEqual(probe.call_count, 1)


if __name__ == "__main__":
    unittest.main()
