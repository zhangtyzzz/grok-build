import copy
import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("handoff", Path(__file__).with_name("release-handoff.py"))
handoff = importlib.util.module_from_spec(spec)
spec.loader.exec_module(handoff)


class HandoffTests(unittest.TestCase):
    def setUp(self):
        self.targets = ["target-" + str(i) for i in range(6)]
        self.commit = "a" * 40
        self.run = dict(repository={"full_name": "owner/repo"}, head_sha=self.commit,
                        head_branch="alpha", event="workflow_dispatch",
                        path=".github/workflows/auto-release.yml", status="in_progress",
                        conclusion=None, run_attempt=2)
        names = ["Validate exact release source / CI required"]
        names += ["Warm exact release binaries / Warm " + t for t in self.targets]
        self.jobs = [dict(name=n, status="completed", conclusion="success",
                          head_sha=self.commit, run_attempt=2) for n in names]
        self.artifacts = [dict(name="release-inputs-" + t, expired=False) for t in self.targets]

    def validate(self):
        handoff.validate(self.run, self.jobs, self.artifacts, "owner/repo", self.commit, self.targets)

    def test_dispatch_can_still_be_finishing(self):
        self.validate()
        self.run.update(head_branch="main", status="completed", conclusion="success")
        self.validate()

    def test_reject_untrusted_or_different_source(self):
        original = copy.deepcopy(self.run)
        for key, value in [("head_sha", "b" * 40), ("head_branch", "feature"),
                           ("event", "pull_request"), ("path", ".github/workflows/ci.yml"),
                           ("repository", {"full_name": "other/repo"}),
                           ("status", "queued"), ("conclusion", "failure")]:
            with self.subTest(key=key):
                self.run = dict(original, **{key: value})
                with self.assertRaises(ValueError):
                    self.validate()

    def test_every_current_attempt_gate_must_pass(self):
        original = copy.deepcopy(self.jobs)
        for index in range(len(original)):
            for key, value in [("conclusion", "skipped"), ("status", "in_progress"),
                               ("head_sha", "b" * 40), ("run_attempt", 1)]:
                with self.subTest(index=index, key=key):
                    self.jobs = copy.deepcopy(original)
                    self.jobs[index][key] = value
                    with self.assertRaises(ValueError):
                        self.validate()
        self.jobs = original[1:]
        with self.assertRaises(ValueError):
            self.validate()
        self.jobs = original + [original[0]]
        with self.assertRaises(ValueError):
            self.validate()

    def test_ci_reuse_requires_trusted_exact_success(self):
        run = dict(self.run, path=".github/workflows/ci.yml", event="push",
                   status="completed", conclusion="success")
        jobs = [dict(self.jobs[0], name="CI required")]
        self.assertTrue(handoff.successful_ci(run, jobs, "owner/repo", self.commit))
        for key, value in [("head_sha", "b" * 40), ("head_branch", "feature"),
                           ("event", "pull_request"), ("path", ".github/workflows/fake.yml"),
                           ("status", "in_progress"), ("conclusion", "failure")]:
            with self.subTest(key=key):
                self.assertFalse(handoff.successful_ci(dict(run, **{key: value}), jobs,
                                                      "owner/repo", self.commit))
        for key, value in [("run_attempt", 1), ("head_sha", "b" * 40),
                           ("conclusion", "skipped"), ("name", "Not CI required")]:
            with self.subTest(key=key):
                self.assertFalse(handoff.successful_ci(run, [dict(jobs[0], **{key: value})],
                                                      "owner/repo", self.commit))
        self.assertFalse(handoff.successful_ci(run, [], "owner/repo", self.commit))

    def test_missing_expired_duplicate_artifacts_fail_closed(self):
        original = copy.deepcopy(self.artifacts)
        for index in range(len(original)):
            self.artifacts = copy.deepcopy(original)
            self.artifacts[index]["expired"] = True
            with self.assertRaises(ValueError):
                self.validate()
        for value in [original[1:], original + [original[0]]]:
            self.artifacts = value
            with self.assertRaises(ValueError):
                self.validate()


if __name__ == "__main__":
    unittest.main()
