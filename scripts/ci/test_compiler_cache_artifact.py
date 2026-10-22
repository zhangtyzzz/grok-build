"""Compiler snapshots may cross trusted branches, but never PR trust boundaries."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("compiler_cache_artifact", Path(__file__).with_name("compiler-cache-artifact.py"))
cache = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cache)

REPOSITORY = "owner/repo"
NAME = cache.PREFIX + "Linux-X64-release-target-rust-1.94.0-sccache-0.18"


def artifact(ident=1, branch="main", name=NAME):
    return {"id": ident, "name": name, "created_at": f"2026-10-22T12:00:{ident:02d}Z", "expired": False,
            "workflow_run": {"id": ident, "head_branch": branch, "head_sha": "a" * 40,
                             "repository_id": 7, "head_repository_id": 7}}


def run(ident=1, branch="main"):
    return {"id": ident, "head_branch": branch, "head_sha": "a" * 40, "event": "workflow_dispatch",
            "path": ".github/workflows/release-cache.yml", "status": "completed", "conclusion": "success",
            "repository": {"id": 7, "full_name": REPOSITORY},
            "head_repository": {"id": 7, "full_name": REPOSITORY}}


class CompilerSnapshotTests(unittest.TestCase):
    def test_alpha_can_restore_main_but_main_does_not_trust_alpha(self):
        self.assertTrue(cache.trusted(artifact(), run(), REPOSITORY, {"main", "alpha"}))
        self.assertFalse(cache.trusted(artifact(branch="alpha"), run(branch="alpha"), REPOSITORY, {"main"}))

    def test_pull_request_from_a_trusted_named_branch_is_rejected(self):
        source = run()
        source["event"] = "pull_request"
        self.assertFalse(cache.trusted(artifact(), source, REPOSITORY, {"main"}))

    def test_fork_unknown_workflow_and_wrong_repository_are_rejected(self):
        for mutate in [lambda a, r: a["workflow_run"].update(head_repository_id=8),
                       lambda a, r: r.update(path=".github/workflows/untrusted.yml"),
                       lambda a, r: r["repository"].update(full_name="other/repo"),
                       lambda a, r: r["head_repository"].update(full_name="other/repo")]:
            with self.subTest(mutate=mutate):
                item, source = artifact(), run()
                mutate(item, source)
                self.assertFalse(cache.trusted(item, source, REPOSITORY, {"main"}))

    def test_expired_mismatched_and_missing_source_metadata_are_rejected(self):
        for mutate in [lambda a, r: a.update(expired=True),
                       lambda a, r: r.update(head_sha="b" * 40),
                       lambda a, r: r.update(id=99),
                       lambda a, r: r.update(head_branch="alpha"),
                       lambda a, r: a["workflow_run"].update(repository_id=8, head_repository_id=8),
                       lambda a, r: a["workflow_run"].update(head_sha="")]:
            with self.subTest(mutate=mutate):
                item, source = artifact(), run()
                mutate(item, source)
                self.assertFalse(cache.trusted(item, source, REPOSITORY, {"main", "alpha"}))

    def test_lookup_skips_other_workloads_and_untrusted_newer_artifacts(self):
        items = [artifact(1), artifact(2, name=NAME + "-different"), artifact(3, branch="feature")]
        selected = list(cache.candidates(items, REPOSITORY, {"main", "alpha"}, run, NAME))
        self.assertEqual([a["id"] for a in selected], [1])

    def test_alpha_prefers_its_own_snapshot_then_falls_back_to_main(self):
        items = [artifact(1), artifact(2, branch="alpha"), artifact(3)]
        runs = {1: run(1), 2: run(2, "alpha"), 3: run(3)}
        selected = list(cache.candidates(items, REPOSITORY, {"main", "alpha"}, runs.__getitem__, NAME, preferred_branch="alpha"))
        self.assertEqual([a["id"] for a in selected], [2, 3, 1])
        items[1]["expired"] = True
        selected = list(cache.candidates(items, REPOSITORY, {"main", "alpha"}, runs.__getitem__, NAME, preferred_branch="alpha"))
        self.assertEqual([a["id"] for a in selected], [3, 1])

    def test_cleanup_retains_two_per_workload_and_branch(self):
        items = [artifact(i) for i in range(1, 5)] + [artifact(5, "alpha"), artifact(6, name=NAME + "-ci")]
        self.assertEqual({a["id"] for a in cache.obsolete(items)}, {1, 2})

    def test_partial_run_does_not_reuse_release_verdicts(self):
        # The cache only avoids repeated compiler work; CI and publication gates
        # still execute. A successful sibling job can seed a failed run's retry.
        source = run()
        source.update(status="in_progress", conclusion=None)
        self.assertTrue(cache.trusted(artifact(), source, REPOSITORY, {"main"}))


if __name__ == "__main__":
    unittest.main()
