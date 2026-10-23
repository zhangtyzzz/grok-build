import copy
import importlib.util
import json
import os
import subprocess
import tempfile
import textwrap
from unittest.mock import Mock
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("warm", Path(__file__).with_name("dispatch-alpha-warm.py"))
warm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(warm)


class DispatchTests(unittest.TestCase):
    def setUp(self):
        self.sha = "a" * 40
        self.repo = {"id": 7, "full_name": "owner/repo"}
        self.run = dict(id=42, run_attempt=1, head_sha=self.sha, head_branch="alpha",
                        event="push", path=warm.CI, status="completed", conclusion="success",
                        repository=self.repo, head_repository=self.repo)
        self.event = {"action": "completed", "workflow_run": copy.deepcopy(self.run)}
        self.jobs = [dict(name="CI required", head_sha=self.sha, run_attempt=1,
                         status="completed", conclusion="success")]
        self.heads = [self.sha, self.sha]
        self.existing = []
        self.posts = []
        self.lookup_count = 0
        self.change_attempt = False
        self.post_response = {"workflow_run_id": 99}
        self.post_error = None
        self.visibility_delay = 0
        self.visibility_calls = 0
        self.visible_sha = self.sha
        self.sleep = Mock()
        self.run_pages = None
        targets = json.loads((Path(__file__).resolve().parents[1] / "dist/targets.json").read_text())
        self.warm_jobs = [dict(name="Warm exact release binaries / Warm " + target,
                               head_sha=self.sha, run_attempt=1, status="completed", conclusion="success")
                          for target in targets]
        self.coordinator_job_reads = 0

    def api(self, endpoint, method="GET", payload=None, paginate=False):
        if method == "POST":
            self.posts.append((endpoint, payload))
            if self.post_error:
                raise self.post_error
            return self.post_response
        if endpoint.endswith("/actions/runs/42"):
            self.lookup_count += 1
            return dict(self.run, run_attempt=2) if self.change_attempt and self.lookup_count > 1 else self.run
        if "/actions/runs/80/jobs?" in endpoint:
            self.assertTrue(paginate)
            self.coordinator_job_reads += 1
            # The final platforms are deliberately on a later API page.
            return [{"jobs": self.warm_jobs[:2]}, {"jobs": self.warm_jobs[2:]}]
        if "/actions/runs/42/jobs?" in endpoint:
            return [{"jobs": self.jobs}]
        if endpoint.endswith("/commits/alpha"):
            return {"sha": self.heads.pop(0)}
        if "/runs?" in endpoint:
            self.assertTrue(paginate)
            if self.posts and "/release-cache.yml/" in endpoint:
                self.visibility_calls += 1
                if self.visibility_calls > self.visibility_delay:
                    visible = dict(self.run, id=99, head_sha=self.visible_sha, path=warm.WARM,
                                   event="workflow_dispatch", status="queued", conclusion=None)
                    return [{"workflow_runs": self.existing + [visible]}]
            return self.run_pages or [{"workflow_runs": self.existing}]
        raise AssertionError(endpoint)

    def dispatch(self):
        return warm.dispatch(self.api, self.event, "owner/repo", 7, sleep=self.sleep)

    def test_dispatch_is_only_warm_with_exact_commit(self):
        self.dispatch()
        self.assertEqual(self.posts, [("repos/owner/repo/actions/workflows/release-cache.yml/dispatches",
                                      {"ref": "alpha", "inputs": {"commit": self.sha}})])

    def test_untrusted_or_failed_ci_never_dispatches(self):
        for changes in (dict(event="pull_request"), dict(head_branch="main"), dict(conclusion="failure"),
                        dict(path=warm.COORDINATOR), dict(head_sha="b" * 40), dict(run_attempt=2),
                        dict(head_repository={"id": 9, "full_name": "other/fork"}),
                        dict(repository={"id": 9, "full_name": "owner/repo"})):
            self.setUp()
            self.run.update(changes)
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.dispatch()
            self.assertEqual(self.posts, [])

    def test_missing_failed_stale_or_duplicate_required_gate_rejected(self):
        for jobs in ([], [dict(self.jobs[0], conclusion="failure")],
                     [dict(self.jobs[0], run_attempt=2)], self.jobs * 2):
            self.setUp()
            self.jobs = jobs
            with self.subTest(jobs=jobs), self.assertRaises(ValueError):
                self.dispatch()
            self.assertEqual(self.posts, [])

    def test_branch_advance_before_or_during_dispatch_skips(self):
        for heads in (["b" * 40], [self.sha, "b" * 40]):
            self.setUp()
            self.heads = heads
            self.assertIn("advanced", self.dispatch())
            self.assertEqual(self.posts, [])

    def test_ci_rerun_after_validation_blocks_dispatch(self):
        self.change_attempt = True
        with self.assertRaises(ValueError):
            self.dispatch()
        self.assertEqual(self.posts, [])

    def test_active_or_successful_exact_warm_or_coordinator_skips(self):
        for path in (warm.WARM, warm.COORDINATOR):
            for status in ("queued", "waiting", "in_progress", "completed"):
                self.setUp()
                self.existing = [dict(self.run, id=80, path=path, event="workflow_dispatch", status=status)]
                self.assertIn("already", self.dispatch())
                self.assertEqual(self.posts, [])
                self.assertEqual(self.coordinator_job_reads,
                                 int(path == warm.COORDINATOR and status == "completed"))

    def test_tag_already_exists_coordinator_success_does_not_suppress_warm(self):
        self.existing = [dict(self.run, id=80, path=warm.COORDINATOR, event="workflow_dispatch")]
        self.warm_jobs = [dict(name="Detect unreleased branch version", head_sha=self.sha,
                               run_attempt=1, status="completed", conclusion="success"),
                          dict(name="Warm exact release binaries", head_sha=self.sha,
                               run_attempt=1, status="completed", conclusion="skipped"),
                          dict(name="Validate exact release source", head_sha=self.sha,
                               run_attempt=1, status="completed", conclusion="skipped")]
        self.dispatch()
        self.assertGreater(self.coordinator_job_reads, 0)
        self.assertEqual(len(self.posts), 1)
        self.assertIn("/release-cache.yml/dispatches", self.posts[0][0])

    def test_coordinator_requires_every_current_attempt_warm_job(self):
        invalid_jobs = [[], self.warm_jobs[:-1], self.warm_jobs + [self.warm_jobs[0]]]
        for changes in (dict(conclusion="skipped"), dict(conclusion="failure"),
                        dict(status="in_progress"), dict(head_sha="b" * 40), dict(run_attempt=2)):
            invalid_jobs.append(self.warm_jobs[:-1] + [dict(self.warm_jobs[-1], **changes)])
        for jobs in invalid_jobs:
            self.setUp()
            self.existing = [dict(self.run, id=80, path=warm.COORDINATOR, event="workflow_dispatch")]
            self.warm_jobs = jobs
            with self.subTest(jobs=jobs):
                self.dispatch()
                self.assertEqual(len(self.posts), 1)

    def test_failed_or_different_commit_warm_does_not_block(self):
        for changes in (dict(conclusion="failure"), dict(head_sha="b" * 40)):
            self.setUp()
            self.existing = [dict(self.run, path=warm.WARM, event="workflow_dispatch", **changes)]
            self.dispatch()
            self.assertEqual(len(self.posts), 1)

    def test_duplicate_on_later_api_page_skips(self):
        existing = dict(self.run, id=80, path=warm.WARM, event="workflow_dispatch", status="queued")
        self.run_pages = [{"workflow_runs": []}, {"workflow_runs": [existing]}]
        self.assertIn("already", self.dispatch())
        self.assertEqual(self.posts, [])

    def test_untrusted_existing_run_cannot_suppress_warm(self):
        for changes in (dict(head_repository={"id": 8, "full_name": "other/fork"}),
                        dict(head_branch="main"), dict(event="pull_request"),
                        dict(path=".github/workflows/fake.yml"), dict(conclusion="cancelled")):
            self.setUp()
            existing = dict(self.run, path=warm.WARM, event="workflow_dispatch")
            existing.update(changes)
            self.existing = [existing]
            self.dispatch()
            self.assertEqual(len(self.posts), 1)

    def test_visibility_confirmation_waits_without_repeating_dispatch(self):
        self.visibility_delay = 2
        self.assertIn("run 99", self.dispatch())
        self.assertEqual(self.sleep.call_count, 2)
        self.assertEqual(len(self.posts), 1)

    def test_empty_historical_dispatch_response_uses_new_visible_run(self):
        self.post_response = None
        self.assertIn("run 99", self.dispatch())
        self.assertEqual(len(self.posts), 1)

    def test_visibility_timeout_or_changed_ref_never_repeats_dispatch(self):
        self.visible_sha = "b" * 40
        with self.assertRaisesRegex(ValueError, "not visible"):
            self.dispatch()
        self.assertEqual(self.sleep.call_count, 5)
        self.assertEqual(len(self.posts), 1)

    def test_ambiguous_post_error_is_never_retried(self):
        self.post_error = subprocess.CalledProcessError(1, ["gh", "api"])
        with self.assertRaises(subprocess.CalledProcessError):
            self.dispatch()
        self.assertEqual(len(self.posts), 1)
        self.sleep.assert_not_called()


class ReceivingWorkflowTests(unittest.TestCase):
    """Execute the receiving workflow's real shell guard, with hermetic git/node."""
    @classmethod
    def setUpClass(cls):
        root = Path(__file__).resolve().parents[2]
        cls.workflow = (root / ".github/workflows/release-cache.yml").read_text()
        step = cls.workflow.index("      - name: Require an exact release branch commit")
        start = cls.workflow.index("        run: |\n", step) + len("        run: |\n")
        end = cls.workflow.index("\n  build:", start)
        cls.commands = textwrap.dedent(cls.workflow[start:end])

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.commands_log = self.root / "commands.log"
        self.output = self.root / "output"
        git = self.root / "git"
        git.write_text("""#!/bin/sh
printf 'git %s\\n' "$*" >>"$COMMAND_LOG"
case "$1" in
  rev-parse) printf '%s\\n' "$CHECKOUT_SHA" ;;
  ls-remote)
    test "$REMOTE_FAIL" != 1 || exit 7
    printf '%s\\t%s\\n' "$REMOTE_SHA" "$3" ;;
  status) printf '%s' "$DIRTY" ;;
  *) exit 97 ;;
esac
""")
        git.chmod(0o755)
        node = self.root / "node"
        node.write_text("""#!/bin/sh
printf 'node %s\\n' "$*" >>"$COMMAND_LOG"
printf '%s\\n' '1.0.47-alpha.3'
""")
        node.chmod(0o755)
        self.sha = "a" * 40
        self.env = dict(PATH=str(self.root) + os.pathsep + os.environ.get("PATH", ""),
                        COMMAND_LOG=str(self.commands_log), GITHUB_OUTPUT=str(self.output),
                        REQUESTED_COMMIT=self.sha, WORKFLOW_COMMIT=self.sha,
                        REF_NAME="refs/heads/alpha", CHECKOUT_SHA=self.sha,
                        REMOTE_SHA=self.sha, REMOTE_FAIL="0", DIRTY="")

    def run_guard(self, **changes):
        return subprocess.run(["bash", "-c", self.commands], cwd=self.root,
                              env=dict(self.env, **changes), text=True, capture_output=True)

    def assert_guard_rejects(self, **changes):
        result = self.run_guard(**changes)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.output.exists())
        log = self.commands_log.read_text() if self.commands_log.exists() else ""
        self.assertNotIn("node ", log)

    def test_matching_alpha_and_reusable_main_commit_succeed(self):
        for ref in ("refs/heads/alpha", "refs/heads/main"):
            with self.subTest(ref=ref):
                self.output.unlink(missing_ok=True)
                result = self.run_guard(REF_NAME=ref)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.output.read_text(), f"version=1.0.47-alpha.3\ncommit={self.sha}\n")

    def test_manual_empty_input_uses_workflow_commit(self):
        result = self.run_guard(REQUESTED_COMMIT="")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"commit={self.sha}\n", self.output.read_text())

    def test_input_must_equal_workflow_sha_before_running_checkout_code(self):
        self.assert_guard_rejects(REQUESTED_COMMIT="b" * 40)

    def test_checkout_must_equal_workflow_sha(self):
        self.assert_guard_rejects(CHECKOUT_SHA="b" * 40)

    def test_current_branch_must_still_equal_workflow_sha(self):
        self.assert_guard_rejects(REMOTE_SHA="b" * 40)

    def test_missing_branch_or_remote_failure_rejects(self):
        self.assert_guard_rejects(REMOTE_SHA="")
        self.assert_guard_rejects(REMOTE_FAIL="1")

    def test_non_release_refs_are_rejected_even_with_exact_input(self):
        for ref in ("refs/heads/feature", "refs/tags/v1.0.47", "alpha", ""):
            with self.subTest(ref=ref):
                self.assert_guard_rejects(REF_NAME=ref)

    def test_non_full_or_injected_sha_is_rejected(self):
        for sha in ("HEAD", "a" * 39, "A" * 40, "a" * 40 + "\n", "$(touch BAD)"):
            with self.subTest(sha=sha):
                self.assert_guard_rejects(REQUESTED_COMMIT=sha)
        self.assertFalse((self.root / "BAD").exists())

    def test_receiver_declares_dispatch_input_and_build_depends_on_guard(self):
        dispatch = self.workflow.split("  workflow_dispatch:\n", 1)[1].split("  workflow_call:", 1)[0]
        self.assertIn("      commit:\n", dispatch)
        self.assertIn("          WORKFLOW_COMMIT: ${{ github.sha }}", self.workflow)
        build = self.workflow.split("\n  build:\n", 1)[1]
        self.assertIn("    needs: metadata\n", build)


if __name__ == "__main__":
    unittest.main()
