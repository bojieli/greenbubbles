#!/usr/bin/env python3
"""Unit tests for the personal-memory driver.

Run with `python3 -m unittest discover -s scripts -p 'test_*.py' -v`.

Scope: the pure decision logic and the on-disk project setup. Nothing here
launches an agent, reads a corpus, or makes a network request, so the suite is
safe to run anywhere CI runs. The cases concentrate on behaviour that has
regressed before — the retry classifier, the stderr snippet, timestamp
comparison across timezones, the concurrency cap, and the files written into a
user project.
"""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path

_SOURCE = Path(__file__).resolve().parent / "personal-memory-parallel.py"
_SPEC = importlib.util.spec_from_file_location("personal_memory_parallel", _SOURCE)
driver = importlib.util.module_from_spec(_SPEC)
sys.modules["personal_memory_parallel"] = driver
_SPEC.loader.exec_module(driver)

# The driver narrates progress on stdout. Collect it instead of printing it,
# so a test run stays readable and so tests can assert on what was said.
LOGGED: list[str] = []
driver.log = LOGGED.append


class ApiErrorClassification(unittest.TestCase):
    """A transient API failure must be retried; a stalled agent must not be."""

    def test_a_successful_exit_is_never_an_api_error(self):
        self.assertFalse(driver._is_api_error(0, "429 rate limit exceeded"))

    def test_quota_and_rate_limit_failures_are_api_errors(self):
        for stderr in (
            "Error: 429 Too Many Requests",
            "status: RESOURCE_EXHAUSTED",
            "Quota exceeded for quota metric",
            "503 Service Unavailable",
            "User location is not supported for the API use",
        ):
            with self.subTest(stderr=stderr):
                self.assertTrue(driver._is_api_error(1, stderr))

    def test_network_level_failures_are_api_errors(self):
        # Without these a DNS or TLS blip looked like a stall, and the driver
        # advanced lastTickTime past messages it never processed.
        for stderr in (
            "TypeError: fetch failed",
            "connect ECONNREFUSED 127.0.0.1:443",
            "read ECONNRESET",
            "connect ETIMEDOUT",
            "socket hang up",
        ):
            with self.subTest(stderr=stderr):
                self.assertTrue(driver._is_api_error(1, stderr))

    def test_matching_is_case_insensitive(self):
        self.assertTrue(driver._is_api_error(1, "RESOURCE_EXHAUSTED"))

    def test_an_agent_logic_failure_is_not_an_api_error(self):
        self.assertFalse(
            driver._is_api_error(1, "Loop detected: the agent repeated a tool call")
        )


class FirstErrorLine(unittest.TestCase):
    """The logged snippet must name the cause, not the harness banner."""

    def test_informational_harness_notices_are_skipped(self):
        stderr = (
            "ripgrep is not available, falling back to grepTool\n"
            "Approval mode: yolo\n"
            "Error: 429 Too Many Requests\n"
        )
        self.assertEqual(driver._first_error_line(stderr), "Error: 429 Too Many Requests")

    def test_blank_lines_are_skipped(self):
        self.assertEqual(driver._first_error_line("\n\n  boom  \n"), "boom")

    def test_a_long_line_is_truncated(self):
        self.assertEqual(len(driver._first_error_line("x" * 500)), 200)

    def test_nothing_usable_reports_no_detail(self):
        self.assertEqual(driver._first_error_line("Approval mode: yolo\n"), "(no detail)")


class TimestampComparison(unittest.TestCase):
    """`memory status` answers in local time; a scope is recorded in UTC."""

    def test_equivalent_instants_in_different_zones_compare_equal(self):
        self.assertEqual(
            driver._parse_ts("2026-08-25T08:00:00+08:00"),
            driver._parse_ts("2026-08-25T00:00:00Z"),
        )

    def test_none_survives(self):
        self.assertIsNone(driver._parse_ts(None))

    def test_an_unparseable_value_falls_back_to_the_raw_string(self):
        self.assertEqual(driver._parse_ts("not a timestamp"), "not a timestamp")

    def test_a_scope_is_bound_across_timezone_forms(self):
        status = {
            "scope": {
                "from": "2026-08-25T08:00:00+08:00",
                "through": "2026-09-01T07:59:59+08:00",
                "conversationFilterCount": 2,
            }
        }
        scope = {
            "from": "2026-08-25T00:00:00Z",
            "through": "2026-08-31T23:59:59Z",
            "conversations": ["C000001", "C000002"],
        }
        self.assertTrue(driver.scope_is_bound(status, scope))

    def test_a_different_conversation_count_is_not_bound(self):
        status = {"scope": {"from": None, "through": None, "conversationFilterCount": 1}}
        scope = {"from": None, "through": None, "conversations": ["C1", "C2"]}
        self.assertFalse(driver.scope_is_bound(status, scope))

    def test_an_unbound_state_is_not_bound(self):
        scope = {"from": None, "through": None, "conversations": []}
        self.assertFalse(driver.scope_is_bound({}, scope))


class TickConcurrency(unittest.TestCase):
    """Tick shards share one user project, so they never run concurrently."""

    def test_a_single_shard_run_is_sequential(self):
        self.assertEqual(driver.tick_parallelism(1, 1), 1)

    def test_a_higher_request_is_capped(self):
        self.assertEqual(driver.tick_parallelism(8, 4), 1)

    def test_a_nonsense_request_is_floored(self):
        self.assertEqual(driver.tick_parallelism(0, 4), 1)
        self.assertEqual(driver.tick_parallelism(-3, 0), 1)

    def test_capping_says_so(self):
        LOGGED.clear()
        driver.tick_parallelism(8, 4)
        self.assertTrue(any("capping at 1" in line for line in LOGGED), LOGGED)

    def test_an_uncapped_run_says_nothing(self):
        LOGGED.clear()
        driver.tick_parallelism(1, 4)
        self.assertEqual(LOGGED, [])


class Planning(unittest.TestCase):
    """Scope packing decides what one agent binding covers."""

    def _conversation(self, months):
        return driver.Conversation(
            alias="C000001", source_id="wxid_x", label="chat", kind="direct",
            messages=sum(months.values()), self_messages=1, months=dict(months),
        )

    def test_a_conversation_under_the_limit_is_one_task(self):
        tasks = driver.split_conversation(self._conversation({"2026-01": 10}), 100)
        self.assertEqual(len(tasks), 1)
        self.assertEqual((tasks[0].from_month, tasks[0].through_month), ("2026-01", "2026-01"))

    def test_an_oversized_conversation_splits_on_month_boundaries(self):
        conversation = self._conversation({"2026-01": 60, "2026-02": 60, "2026-03": 60})
        tasks = driver.split_conversation(conversation, 100)
        self.assertEqual(len(tasks), 3)
        self.assertEqual([t.messages for t in tasks], [60, 60, 60])

    def test_a_single_month_heavier_than_the_limit_stays_whole(self):
        # The corpus records activity per month, so this is the finest cut
        # available without guessing at distribution inside the month.
        tasks = driver.split_conversation(self._conversation({"2026-01": 5000}), 100)
        self.assertEqual(len(tasks), 1)
        self.assertEqual(tasks[0].messages, 5000)

    def test_tasks_sharing_a_window_fuse_into_one_scope(self):
        tasks = [
            driver.Task(["C1"], 10, ["a"], "2026-01", "2026-01"),
            driver.Task(["C2"], 5, ["b"], "2026-01", "2026-01"),
            driver.Task(["C3"], 7, ["c"], "2026-02", "2026-02"),
        ]
        fused = driver.merge_same_window(tasks)
        self.assertEqual(len(fused), 2)
        january = next(t for t in fused if t.from_month == "2026-01")
        self.assertEqual(sorted(january.conversations), ["C1", "C2"])
        self.assertEqual(january.messages, 15)

    def test_merging_does_not_mutate_its_input(self):
        original = driver.Task(["C1"], 10, ["a"], None, None)
        driver.merge_same_window([original, driver.Task(["C2"], 5, ["b"], None, None)])
        self.assertEqual(original.conversations, ["C1"])

    def test_packing_balances_shards_on_message_count(self):
        tasks = [driver.Task([f"C{i}"], load, [f"c{i}"]) for i, load in
                 enumerate([100, 90, 20, 10])]
        shards = driver.pack_shards(tasks, 2)
        self.assertEqual(len(shards), 2)
        self.assertEqual(sorted(sum(t.messages for t in s) for s in shards), [110, 110])

    def test_packing_returns_no_empty_shards(self):
        shards = driver.pack_shards([driver.Task(["C1"], 5, ["a"])], 8)
        self.assertEqual(len(shards), 1)

    def test_clamping_keeps_the_narrower_bound(self):
        self.assertEqual(
            driver.clamp_lower("2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z"),
            "2026-02-01T00:00:00Z",
        )
        self.assertEqual(
            driver.clamp_upper("2026-03-01T00:00:00Z", "2026-02-01T00:00:00Z"),
            "2026-02-01T00:00:00Z",
        )

    def test_clamping_against_nothing_keeps_what_there_is(self):
        self.assertEqual(driver.clamp_lower(None, "2026-02-01T00:00:00Z"),
                         "2026-02-01T00:00:00Z")
        self.assertEqual(driver.clamp_upper("2026-02-01T00:00:00Z", None),
                         "2026-02-01T00:00:00Z")
        self.assertIsNone(driver.clamp_lower(None, None))

    def test_month_bounds_are_inclusive_and_zoned(self):
        start, end = driver.month_bounds("2026-02", "Asia/Shanghai")
        self.assertEqual(start, "2026-02-01T00:00:00+08:00")
        self.assertEqual(end, "2026-02-28T23:59:59+08:00")

    def test_december_rolls_into_the_next_year(self):
        _, end = driver.month_bounds("2026-12", "UTC")
        self.assertEqual(end, "2026-12-31T23:59:59+00:00")

    def test_batch_bounds_stay_within_the_page_protocol(self):
        text_bytes, messages = driver.batch_bounds(1_048_576)
        self.assertLessEqual(text_bytes, driver.MAXIMUM_NEXT_TEXT_BYTES)
        self.assertGreaterEqual(text_bytes, driver.MINIMUM_NEXT_TEXT_BYTES)
        self.assertGreaterEqual(messages, 1)
        self.assertLessEqual(messages, driver.MAXIMUM_BATCH_MESSAGES)

    def test_a_tiny_context_still_gets_a_usable_floor(self):
        text_bytes, messages = driver.batch_bounds(8_192)
        self.assertEqual(text_bytes, driver.MINIMUM_NEXT_TEXT_BYTES)
        self.assertGreaterEqual(messages, 1)


class HarnessSelection(unittest.TestCase):
    """A subscription harness must never be pushed onto a metered key."""

    def _args(self, **overrides):
        base = dict(
            agent="claude", agent_command=None, agent_arg=None, model=None,
            provider=None, base_url=None, api_key_env="OPENROUTER_API_KEY",
            context_window=1_048_576, max_output_tokens=65536,
            api_type=driver.DEFAULT_API_TYPE, env=None, skill="inline",
            skill_dir=None, pi_package=driver.DEFAULT_PI_PACKAGE, cwd=".",
            greenbubbles=None,
        )
        base.update(overrides)
        return Namespace(**base)

    def test_a_subscription_harness_gets_no_model_by_default(self):
        for agent in ("claude", "codex", "gemini"):
            with self.subTest(agent=agent):
                self.assertIsNone(driver.HARNESS_DEFAULT_MODEL[agent])

    def test_pi_defaults_to_the_recommended_model(self):
        self.assertEqual(driver.HARNESS_DEFAULT_MODEL["pi"], driver.DEFAULT_MODEL)

    def test_an_explicit_model_reaches_the_command_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            command, _ = driver.agent_command(
                self._args(agent="gemini", model="gemini-3.8-flash"),
                Path(tmp), "prompt", [], tmp,
            )
            self.assertIn("gemini-3.8-flash", command)

    def test_no_model_flag_appears_when_none_was_chosen(self):
        with tempfile.TemporaryDirectory() as tmp:
            command, _ = driver.agent_command(
                self._args(agent="claude"), Path(tmp), "prompt", [], tmp,
            )
            self.assertNotIn("--model", command)

    def test_an_api_key_is_passed_by_name_not_by_value(self):
        with tempfile.TemporaryDirectory() as tmp:
            args = self._args(agent="pi", base_url="https://router.example/v1")
            config = driver.router_config(Path(tmp), args)
            body = (config / "models.json").read_text(encoding="utf-8")
            self.assertIn("$OPENROUTER_API_KEY", body)
            self.assertEqual((config / "models.json").stat().st_mode & 0o777, 0o600)
            self.assertEqual(config.stat().st_mode & 0o777, 0o700)

    def test_a_base_url_for_an_unknown_harness_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            args = self._args(agent="command", base_url="https://router.example/v1")
            with self.assertRaises(SystemExit):
                driver.agent_environment(args, Path(tmp))

    def test_the_claude_output_ceiling_clears_one_evidence_page(self):
        with tempfile.TemporaryDirectory() as tmp:
            environment = driver.agent_environment(self._args(agent="claude"), Path(tmp))
            self.assertGreaterEqual(int(environment["BASH_MAX_OUTPUT_LENGTH"]), 49_152)


class UserProject(unittest.TestCase):
    """What the driver writes into the account holder's own project."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.project = Path(self._tmp.name) / "me"
        environment = {
            "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
            "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid",
        }
        self._saved = {k: os.environ.get(k) for k in environment}
        os.environ.update(environment)

    def tearDown(self):
        for key, value in self._saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        self._tmp.cleanup()

    def test_the_first_run_creates_a_git_repo(self):
        self.assertTrue(driver.init_user_project(self.project, "python"))
        self.assertTrue((self.project / ".git").is_dir())

    def test_a_second_run_reuses_the_repo(self):
        driver.init_user_project(self.project, "python")
        self.assertFalse(driver.init_user_project(self.project, "python"))

    def test_the_project_is_owner_only(self):
        driver.init_user_project(self.project, "python")
        self.assertEqual(self.project.stat().st_mode & 0o777, 0o700)

    def test_driver_process_files_are_ignored(self):
        # These are the driver's lock and log. Committing them would put
        # process state into the account holder's memory history.
        driver.init_user_project(self.project, "markdown")
        ignored = (self.project / ".gitignore").read_text(encoding="utf-8").split()
        self.assertIn(".greenbubbles-tick.lock", ignored)
        self.assertIn(".greenbubbles-revise.log", ignored)
        self.assertIn(".greenbubbles-runs/", ignored)

    def test_the_documented_gitignore_matches_what_is_written(self):
        driver.init_user_project(self.project, "python")
        written = set((self.project / ".gitignore").read_text(encoding="utf-8").split())
        for reference in ("format-python.md", "format-markdown.md"):
            path = (Path(__file__).resolve().parent.parent / "skills"
                    / "greenbubbles-personal-memory" / "references" / reference)
            documented = path.read_text(encoding="utf-8")
            block = documented.split("## `.gitignore`", 1)[1].split("```")[1]
            with self.subTest(reference=reference):
                self.assertEqual(set(block.split()), written)

    def test_a_second_tick_cannot_take_the_lock(self):
        driver.init_user_project(self.project, "python")
        held = driver.acquire_project_lock(self.project)
        try:
            probe = subprocess.run(
                [sys.executable, "-c",
                 "import fcntl,sys;"
                 "f=open(sys.argv[1],'w');"
                 "fcntl.flock(f.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)",
                 str(self.project / ".greenbubbles-tick.lock")],
                capture_output=True, text=True,
            )
            self.assertNotEqual(probe.returncode, 0)
        finally:
            held.close()

    def test_the_lock_is_released_when_the_handle_closes(self):
        driver.init_user_project(self.project, "python")
        driver.acquire_project_lock(self.project).close()
        driver.acquire_project_lock(self.project).close()

    def test_a_commit_records_changes_and_a_clean_tree_commits_nothing(self):
        driver.init_user_project(self.project, "markdown")
        (self.project / "manifest.md").write_text("# Personal Memory Manifest\n",
                                                  encoding="utf-8")
        self.assertTrue(driver.git_commit_user_project(self.project, "memory update"))
        self.assertFalse(driver.git_commit_user_project(self.project, "memory update"))

    def test_a_public_remote_is_reported(self):
        driver.init_user_project(self.project, "python")
        subprocess.run(
            ["git", "-C", str(self.project), "remote", "add", "origin",
             "https://github.com/someone/memory.git"],
            capture_output=True, check=True,
        )
        LOGGED.clear()
        driver.check_remote_privacy(self.project)
        self.assertTrue(any("WARNING" in line for line in LOGGED), LOGGED)

    def test_no_remote_is_quiet(self):
        driver.init_user_project(self.project, "python")
        LOGGED.clear()
        driver.check_remote_privacy(self.project)
        self.assertEqual(LOGGED, [])


class WikiMerge(unittest.TestCase):
    """Merging shard output must not duplicate lines or stack headings."""

    def test_a_missing_page_is_taken_whole(self):
        self.assertEqual(driver.merge_markdown(None, "line\n"), "line\n")

    def test_repeated_lines_are_dropped(self):
        merged = driver.merge_markdown("a\nb\n", "b\nc\n")
        self.assertEqual(merged, "a\nb\nc\n")

    def test_nothing_new_leaves_the_page_alone(self):
        self.assertEqual(driver.merge_markdown("a\nb\n", "b\n"), "a\nb\n")


class LanguageDetection(unittest.TestCase):
    """detect_os_language() maps locale codes to human-readable names."""

    def _detect(self, env: dict) -> str:
        old = {k: os.environ.get(k) for k in ("LANGUAGE", "LANG", "LC_ALL", "LC_MESSAGES")}
        try:
            for k in old:
                os.environ.pop(k, None)
            os.environ.update(env)
            return driver.detect_os_language()
        finally:
            for k, v in old.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v

    def test_zh_cn_maps_to_chinese_simplified(self):
        self.assertEqual(self._detect({"LANG": "zh_CN.UTF-8"}), "Chinese (Simplified)")

    def test_zh_tw_maps_to_chinese_traditional(self):
        self.assertEqual(self._detect({"LANG": "zh_TW.UTF-8"}), "Chinese (Traditional)")

    def test_ja_maps_to_japanese(self):
        self.assertEqual(self._detect({"LANG": "ja_JP.UTF-8"}), "Japanese")

    def test_en_us_maps_to_english(self):
        self.assertEqual(self._detect({"LANG": "en_US.UTF-8"}), "English")

    def test_c_locale_defaults_to_english(self):
        self.assertEqual(self._detect({"LANG": "C"}), "English")

    def test_unknown_locale_returns_readable_code(self):
        result = self._detect({"LANG": "xy_ZZ.UTF-8"})
        self.assertIn("xy", result.lower())

    def test_language_env_takes_priority_over_lang(self):
        result = self._detect({"LANGUAGE": "zh_CN", "LANG": "en_US.UTF-8"})
        self.assertEqual(result, "Chinese (Simplified)")


class LanguagePromptInjection(unittest.TestCase):
    """tick_agent_prompt injects the language line when language is set."""

    def _make_prompt(self, language: str) -> str:
        from pathlib import Path
        return driver.tick_agent_prompt(
            binary="/usr/local/bin/greenbubbles",
            corpus=Path("/corpus"),
            state=Path("/state.json"),
            user_project=Path("/project"),
            scope_args=[],
            fmt="markdown",
            max_text_bytes=327680,
            max_messages=2520,
            skill="",
            language=language,
        )

    def test_language_appears_in_prompt(self):
        prompt = self._make_prompt("Chinese (Simplified)")
        self.assertIn("Chinese (Simplified)", prompt)
        self.assertIn("Language:", prompt)

    def test_no_language_omits_language_line(self):
        prompt = self._make_prompt("")
        self.assertNotIn("Language:", prompt)

    def test_language_covers_all_output_kinds(self):
        prompt = self._make_prompt("Japanese")
        # The instruction must name domain files, manifest, and prose broadly
        self.assertIn("domain file", prompt)
        self.assertIn("manifest", prompt)


if __name__ == "__main__":
    unittest.main()
