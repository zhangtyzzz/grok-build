import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("prune", Path(__file__).with_name("prune-caches.py"))
prune = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prune)


def cache(identifier, key, ref="refs/heads/main"):
    return {"id": identifier, "key": key, "ref": ref, "created_at": f"2026-10-{identifier:02d}T00:00:00Z"}


class PruneTests(unittest.TestCase):
    def test_removes_retired_archive_cache(self):
        stem = "sccache-disk-v1-Linux-X64-ci-rust-1.94.0-"
        entries = [cache(i, stem + str(i) * 64 + "-" + str(i) * 40) for i in (1, 2)]
        self.assertEqual([c["id"] for c in prune.obsolete(entries)], [1, 2])

    def test_isolates_platform_toolchain_and_branch(self):
        keys = ["cargo-v1-Linux-X64-1.94.0-", "cargo-v1-Linux-ARM64-1.94.0-",
                "cargo-v1-Linux-X64-1.95.0-"]
        entries = [cache(i, key + "a" * 64) for i, key in enumerate(keys, 1)]
        entries.append(cache(4, entries[0]["key"], "refs/heads/alpha"))
        self.assertEqual(prune.obsolete(entries), [])

    def test_ignores_unknown_keys_and_nonrelease_refs(self):
        entries = [cache(i, key) for i in (1, 2) for key in ("unrelated-cache", "sccache/f/c/b/hash")]
        for ref in ("refs/pull/12/merge", "refs/heads/codex/test", "refs/tags/v1.0.0"):
            entries += [cache(i, "cargo-v1-Linux-X64-1.94.0-" + "a" * 64, ref) for i in (1, 2)]
        self.assertEqual(prune.obsolete(entries), [])

    def test_source_and_exact_build_cache(self):
        source = "cargo-v1-Linux-X64-1.94.0-"
        release = "release-build-v3-aarch64-apple-darwin-rust-1.94.0-"
        entries = [cache(1, source + "a" * 64), cache(2, source + "b" * 64),
                   cache(3, release + "a" * 64 + "-" + "c" * 40),
                   cache(4, release + "b" * 64 + "-" + "d" * 40)]
        self.assertEqual([c["id"] for c in prune.obsolete(entries)], [1, 3])
