"""Security regression tests; use disposable repositories, no credentials/network."""
import base64
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("upstream_sync", Path(__file__).with_name("upstream_sync.py"))
sync = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sync)


class ValidationTests(unittest.TestCase):
    def state(self):
        return {"base": "a" * 40, "upstream": "b" * 40, "conflicts": {"src/main.rs": {}}}

    def artifact(self):
        return {"schema": 1, "base": "a" * 40, "upstream": "b" * 40,
                "files": {"src/main.rs": base64.b64encode(b"resolved\n").decode()}}

    def test_sha_is_not_shell_or_git_revision_expression(self):
        for value in ("HEAD", "-x", "a" * 39, "A" * 40, "a" * 40 + "\n", "$(touch BAD)", "main^{tree}"):
            with self.subTest(value=value), self.assertRaises(sync.SyncError):
                sync.sha(value)

    def test_path_traversal_control_and_git_metadata_rejected(self):
        for path in ("/tmp/BAD", "../BAD", "a/../../BAD", "a//b", "a/./b", "a\\b",
                     ".git/config", "a/.GiT/hooks/x", "a\nb", "a\0b"):
            with self.subTest(path=path), self.assertRaises(sync.SyncError):
                sync.path_safe(path)

    def test_protected_paths(self):
        for path in (".github/workflows/ci.yml", "scripts/automation/new.py", "scripts/dist.sh",
                     "rust-toolchain.toml", "scripts/ci/dispatch-alpha-warm.py",
                     "FORK_CHANGES.md", "a/AGENTS.md", ".env.production",
                     "config/key.pem", "auth.json", ".gitmodules", ".gitattributes"):
            with self.subTest(path=path):
                self.assertTrue(sync.protected(path))
        self.assertFalse(sync.protected("src/token_count.rs"))

    def test_bad_schema_sha_file_set_content_and_markers_fail_closed(self):
        mutations = [
            lambda obj: obj.update(base="c" * 40),
            lambda obj: obj.update(upstream="HEAD"),
            lambda obj: obj.update(extra="injected"),
            lambda obj: obj.update(files={"../../bad": "YQ=="}),
            lambda obj: obj.update(files={}),
            lambda obj: obj.update(files={"src/main.rs": "not base64"}),
            lambda obj: obj.update(files={"src/main.rs": base64.b64encode(b"\0").decode()}),
            lambda obj: obj.update(files={"src/main.rs": base64.b64encode(b"<<<<<<< ours\na\n=======\nb\n>>>>>>> theirs\n").decode()}),
        ]
        for mutate in mutations:
            obj = self.artifact()
            mutate(obj)
            with self.subTest(obj=obj), self.assertRaises((sync.SyncError, ValueError)):
                sync.validate_resolution(obj, self.state())
        self.assertEqual(sync.validate_resolution(self.artifact(), self.state()), {"src/main.rs": b"resolved\n"})

    def test_markers_include_diff3_and_diff_style(self):
        for content in (b"<<<<<<< HEAD\n", b"=======\n", b">>>>>>> upstream\n", b"||||||| base\n"):
            self.assertIsNotNone(sync.MARKER.search(content))
        self.assertIsNone(sync.MARKER.search(b'let s = "<<<<<<<";\n'))

    def test_duplicate_json_keys_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "data.json"
            path.write_text('{"schema":1,"schema":2}')
            with self.assertRaises(sync.SyncError):
                sync.json_read(str(path))

    def test_artifact_cannot_express_shell_or_git_commands(self):
        obj = self.artifact()
        content = b'$(touch /tmp/MUST_NOT_EXIST)\n; git push origin main\n'
        obj["files"]["src/main.rs"] = base64.b64encode(content).decode()
        self.assertEqual(sync.validate_resolution(obj, self.state())["src/main.rs"], content)


class VersionNormalizationTests(unittest.TestCase):
    def manifest(self, package, version="1.2.3"):
        return f'[package]\nname = "{package}"\nversion = "{version}"\n'.encode()

    def lock(self):
        return b"".join(b'[[package]]\nname = "' + p.encode() + b'"\nversion = "1.2.3"\n\n'
                        for p in sync.VERSION_PACKAGES)

    def test_only_package_version_changes_not_dependency_table_version(self):
        dependency = b'\n[dependencies.foo]\nversion = "4.5.6"\n'
        for package, path in zip(sync.VERSION_PACKAGES, sync.VERSION_MANIFESTS):
            with self.subTest(package=package):
                actual = sync.set_version(path, self.manifest(package) + dependency, "2.0.0")
                expected = "2.0.0" if package == sync.BINARY_PACKAGE else "0.0.0"
                self.assertEqual(actual, self.manifest(package, expected) + dependency)

    def test_missing_duplicate_or_renamed_manifest_identity_fails_closed(self):
        path = sync.VERSION_MANIFESTS[0]
        original = self.manifest(sync.BINARY_PACKAGE)
        invalid = (
            original.replace(b'version = "1.2.3"\n', b''),
            original + b'version = "4.5.6"\n',
            original.replace(sync.BINARY_PACKAGE.encode(), b"renamed-package"),
            original.replace(b'[package]\n', b''),
            original + original,
            original.replace(b'version = "1.2.3"\n', b'[dependencies.foo]\nversion = "1.2.3"\n'),
        )
        for content in invalid:
            with self.subTest(content=content), self.assertRaises(sync.SyncError):
                sync.set_version(path, content, "2.0.0")

    def test_missing_duplicate_or_sourced_lock_identity_fails_closed(self):
        original = self.lock()
        first = next(sync.LOCK_PACKAGE.finditer(original))[0]
        invalid = (
            original.replace(first, b''),
            original + first,
            original.replace(b'version = "1.2.3"', b'source = "registry+https://example.invalid"', 1),
            original.replace(b'version = "1.2.3"', b'version = "1.2.3"\nsource = "registry+https://example.invalid"', 1),
            original.replace(b'version = "1.2.3"', b'version = "1.2.3"\nversion = "4.5.6"', 1),
        )
        for content in invalid:
            with self.subTest(content=content), self.assertRaises(sync.SyncError):
                sync.set_version("Cargo.lock", content, "2.0.0")

    def test_lock_dependency_and_unrelated_package_bytes_are_preserved(self):
        dependency = b'dependencies = ["other"]\n'
        lock = self.lock().replace(b'version = "1.2.3"\n', b'version = "1.2.3"\n' + dependency)
        unrelated = b'[[package]]\nname = "other"\nversion = "9.9.9"\nsource = "registry+https://example.invalid"\n'
        updated = sync.set_version("Cargo.lock", lock + unrelated, "2.0.0")
        self.assertEqual(updated.count(dependency), len(sync.VERSION_PACKAGES))
        self.assertTrue(updated.endswith(unrelated))
        self.assertEqual(updated.count(b'version = "0.0.0"'), 3)
        self.assertEqual(updated.count(b'version = "2.0.0"'), 1)

    def test_second_release_changes_only_executable_identity(self):
        for package, path in zip(sync.VERSION_PACKAGES, sync.VERSION_MANIFESTS):
            initial = sync.set_version(path, self.manifest(package), "2.0.0")
            second = sync.set_version(path, initial, "2.0.1")
            with self.subTest(package=package):
                self.assertEqual(initial == second, package != sync.BINARY_PACKAGE)
        first = sync.set_version("Cargo.lock", self.lock(), "2.0.0")
        second = sync.set_version("Cargo.lock", first, "2.0.1")
        self.assertEqual(second, first.replace(b'version = "2.0.0"', b'version = "2.0.1"'))


class RepositoryFixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.cwd = Path.cwd()
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        os.chdir(self.repo)
        sync.git("init", "-q")
        sync.git("config", "user.name", "Test")
        sync.git("config", "user.email", "test@example.invalid")
        self.write("src/main.rs", "base\n")
        self.write("untouched.txt", "keep\n")
        self.write_versions("1.0.45")
        self.initial = self.commit("base")
        sync.git("checkout", "-q", "-b", "upstream-test")
        self.write("src/main.rs", "upstream\n")
        self.upstream = self.commit("upstream")
        sync.git("checkout", "-q", "--detach", self.initial)
        self.write("src/main.rs", "fork\n")
        self.base = self.commit("fork")
        self.state_path = self.root / "state.json"
        self.artifact_path = self.root / "artifact.json"

    def tearDown(self):
        os.chdir(self.cwd)
        self.temp.cleanup()

    def write_versions(self, version, dependency="1"):
        for package, path in zip(sync.VERSION_PACKAGES, sync.VERSION_MANIFESTS):
            self.write(path, f'[package]\nname = "{package}"\nversion = "{version}"\n\n[dependencies]\nfoo = "{dependency}"\n')
        self.write("Cargo.lock", "".join(
            f'[[package]]\nname = "{name}"\nversion = "{version}"\n\n' for name in sync.VERSION_PACKAGES))

    def versions_at(self, ref):
        found = {}
        for package, path in zip(sync.VERSION_PACKAGES, sync.VERSION_MANIFESTS):
            _, match = sync.manifest_package(path, sync.git("show", f"{ref}:{path}"))
            found[package] = match[1].decode()
        return found

    def write(self, path, content):
        file = Path(path)
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(content)

    def commit(self, message):
        sync.git("add", "--all")
        sync.git("commit", "-q", "-m", message)
        return sync.git("rev-parse", "HEAD").decode().strip()

    def start(self):
        state = sync.begin(self.base, self.upstream)
        sync.json_write(str(self.state_path), state)
        return state

    def export(self):
        sync.export(SimpleNamespace(state=str(self.state_path), artifact=str(self.artifact_path)))


class RepositoryTests(RepositoryFixture):
    def test_real_merge_original_conflicts_and_worktree_markers(self):
        state = self.start()
        self.assertEqual(set(state["conflicts"]), {"src/main.rs"})
        # HEAD has no markers: the worktree does, and must be rejected.
        self.assertNotIn(b"<<<<<<<", sync.git("show", "HEAD:src/main.rs"))
        with self.assertRaises(sync.SyncError):
            self.export()
        self.write("src/main.rs", "fork + upstream\n")
        self.export()
        obj = sync.json_read(str(self.artifact_path))
        self.assertEqual(set(obj["files"]), {"src/main.rs"})

    def test_ai_cannot_change_nonconflicting_file(self):
        self.start()
        self.write("src/main.rs", "resolved\n")
        self.write("untouched.txt", "unexpected\n")
        with self.assertRaises(sync.SyncError):
            self.export()

    def test_ai_cannot_stage_or_add_files(self):
        self.start()
        self.write("src/main.rs", "resolved\n")
        sync.git("add", "src/main.rs")
        with self.assertRaises(sync.SyncError):
            self.export()

    def test_ai_cannot_replace_conflict_with_symlink(self):
        self.start()
        Path("src/main.rs").unlink()
        Path("src/main.rs").symlink_to(self.state_path)
        with self.assertRaises(sync.SyncError):
            self.export()

    def test_upstream_clean_protected_change_is_manual(self):
        sync.git("checkout", "-q", "--detach", self.upstream)
        self.write(".github/workflows/new.yml", "untrusted workflow\n")
        upstream = self.commit("workflow")
        sync.git("checkout", "-q", "--detach", self.base)
        with self.assertRaises(sync.SyncError):
            sync.begin(self.base, upstream)

    def test_reconstruction_uses_real_parents_and_only_resolution_contents(self):
        self.start()
        self.write("src/main.rs", "resolved\n")
        self.export()
        sync.git("merge", "--abort")
        args = SimpleNamespace(base=self.base, upstream=self.upstream, artifact=str(self.artifact_path))
        with patch.object(sync, "fetch", return_value=(self.base, self.upstream)), patch.object(sync, "output") as outputs:
            sync.reconstruct(args)
        call = outputs.call_args.kwargs
        parents = sync.git("show", "-s", "--format=%P", call["head"]).decode().strip().split()
        self.assertEqual(parents, [self.base, self.upstream])
        self.assertEqual(sync.git("show", f'{call["head"]}:src/main.rs'), b"resolved\n")
        self.assertEqual(sync.git("show", f'{call["head"]}:untouched.txt'), b"keep\n")

    def test_malicious_artifact_cannot_inject_protected_files(self):
        state = self.start()
        obj = {"schema": 1, "base": self.base, "upstream": self.upstream,
               "files": {"src/main.rs": "YQ==", ".github/workflows/pwn.yml": "YQ=="}}
        sync.json_write(str(self.artifact_path), obj)
        sync.git("merge", "--abort")
        args = SimpleNamespace(base=self.base, upstream=self.upstream, artifact=str(self.artifact_path))
        with patch.object(sync, "fetch", return_value=(self.base, self.upstream)), self.assertRaises(sync.SyncError):
            sync.reconstruct(args)
        self.assertFalse(Path(".github/workflows/pwn.yml").exists())


class PublicationTests(unittest.TestCase):
    def args(self):
        return SimpleNamespace(repo="owner/fork", base="a" * 40, head="b" * 40,
                               tree="c" * 40, pr="42")

    def responses(self, endpoint, method="GET", payload=None, **kwargs):
        args = self.args()
        if endpoint.endswith("/pulls/42"):
            return {"state": "open", "draft": False,
                    "head": {"sha": args.head, "repo": {"full_name": args.repo}},
                    "base": {"ref": "main", "sha": args.base, "repo": {"full_name": args.repo}}}
        if endpoint.endswith("/commits/main"):
            return {"sha": "d" * 40 if self.merged else args.base}
        if endpoint.endswith("/pulls/42/merge"):
            self.assertEqual(payload["sha"], args.head)
            self.merged = True
            return {"merged": True, "sha": "d" * 40}
        if "/git/commits/" in endpoint:
            return {"tree": {"sha": args.tree}}
        if "/contents/" in endpoint:
            self.assertTrue(endpoint.endswith("?ref=" + "d" * 40))
            self.assertIn("/xai-grok-pager-bin/Cargo.toml?", endpoint)
            return {"encoding": "base64", "content": base64.encodebytes(
                b'[package]\nname = "xai-grok-pager-bin"\nversion = "1.2.3"\n').decode()}
        if "/git/matching-refs/" in endpoint:
            return self.tags
        if endpoint.endswith("/dispatches"):
            self.dispatched.append(payload)
            return None
        raise AssertionError(endpoint)

    def setUp(self):
        self.merged = False
        self.tags = []
        self.dispatched = []

    def test_release_dispatch_binds_exact_tested_merged_tree(self):
        with patch.object(sync, "release_guard"), patch.object(sync, "api", side_effect=self.responses):
            sync.merge_release(self.args())
        self.assertEqual(self.dispatched, [{"ref": "main", "inputs": {"expected_commit": "d" * 40}}])

    def test_existing_version_does_not_dispatch_duplicate_release(self):
        self.tags = [{"ref": "refs/tags/v1.2.3"}]
        with patch.object(sync, "release_guard"), patch.object(sync, "api", side_effect=self.responses):
            sync.merge_release(self.args())
        self.assertTrue(self.merged)
        self.assertEqual(self.dispatched, [])

    def test_invalid_executable_version_or_identity_never_dispatches(self):
        for text in ('[package]\nname = "xai-grok-pager-bin"\n',
                     '[package]\nname = "renamed"\nversion = "1.2.3"\n',
                     '[package]\nname = "xai-grok-pager-bin"\nversion = "1.2.3-alpha.1"\n'):
            self.setUp()
            def changed(endpoint, *args, **kwargs):
                if "/contents/" in endpoint:
                    return {"encoding": "base64", "content": base64.b64encode(text.encode()).decode()}
                return self.responses(endpoint, *args, **kwargs)
            with self.subTest(text=text), patch.object(sync, "release_guard"), patch.object(
                    sync, "api", side_effect=changed), self.assertRaises(sync.SyncError):
                sync.merge_release(self.args())
            self.assertEqual(self.dispatched, [])

    def test_merge_tree_mismatch_never_dispatches(self):
        def changed(endpoint, *args, **kwargs):
            if endpoint.endswith("/git/commits/" + "d" * 40):
                return {"tree": {"sha": "e" * 40}}
            return self.responses(endpoint, *args, **kwargs)
        with patch.object(sync, "release_guard"), patch.object(sync, "api", side_effect=changed), self.assertRaises(sync.SyncError):
            sync.merge_release(self.args())
        self.assertEqual(self.dispatched, [])

    def test_changed_pr_head_prevents_merge(self):
        def changed(endpoint, *args, **kwargs):
            result = self.responses(endpoint, *args, **kwargs)
            if endpoint.endswith("/pulls/42"):
                result["head"]["sha"] = "f" * 40
            return result
        with patch.object(sync, "release_guard"), patch.object(sync, "api", side_effect=changed), self.assertRaises(sync.SyncError):
            sync.merge_release(self.args())
        self.assertFalse(self.merged)

    def test_release_guard_checks_queued_runs_on_later_pages(self):
        pages = [{"workflow_runs": [{"status": "completed"}]},
                 {"workflow_runs": [{"status": "queued"}]}]
        with patch.object(sync, "api", return_value=pages), self.assertRaises(sync.SyncError):
            sync.release_guard("owner/fork")

class ForkVersionTests(RepositoryFixture):
    """main numbers its own releases; a sync always lands main's next patch."""

    def assert_versions(self, ref, executable):
        expected = {p: "0.0.0" for p in sync.INTERNAL_PACKAGES}
        expected[sync.BINARY_PACKAGE] = executable
        self.assertEqual(self.versions_at(ref), expected)
        lock = sync.git("show", f"{ref}:Cargo.lock")
        actual = {}
        for match in sync.LOCK_PACKAGE.finditer(lock):
            name = sync.PACKAGE_NAME.search(match[0])
            version = sync.MANIFEST_VERSION.search(match[0])
            if name and name[1].decode() in expected:
                actual[name[1].decode()] = version[1].decode()
        self.assertEqual(actual, expected)

    def reconstruct_head(self):
        args = SimpleNamespace(base=self.base, upstream=self.upstream, artifact=str(self.artifact_path))
        with patch.object(sync, "fetch", return_value=(self.base, self.upstream)), patch.object(sync, "output") as outputs:
            sync.reconstruct(args)
        return outputs.call_args.kwargs["head"]

    def rebuild(self, upstream_version, fork_version, upstream_dependency="1"):
        sync.git("checkout", "-q", "--detach", self.upstream)
        self.write_versions(upstream_version, upstream_dependency)
        self.upstream = self.commit("upstream bump")
        sync.git("checkout", "-q", "--detach", self.base)
        self.write_versions(fork_version)
        self.base = self.commit("fork bump")

    def test_version_only_conflict_resolves_to_next_fork_patch(self):
        self.rebuild("1.0.46", "1.0.50", upstream_dependency="2")
        state = self.start()
        self.assertEqual(set(state["conflicts"]), {"src/main.rs"})
        self.write("src/main.rs", "resolved\n")
        self.export()
        sync.git("merge", "--abort")
        head = self.reconstruct_head()
        self.assert_versions(head, "1.0.51")
        manifest = sync.git("show", f"{head}:{sync.VERSION_MANIFESTS[0]}").decode()
        self.assertIn('foo = "2"', manifest)

    def test_clean_merge_still_bumps_fork_patch(self):
        sync.git("checkout", "-q", "--detach", self.base)
        self.write("src/main.rs", "upstream\n")
        self.base = self.commit("fork matches upstream source")
        state = self.start()
        self.assertEqual(state["conflicts"], {})
        sync.json_write(str(self.artifact_path), {"schema": 1, "base": self.base, "upstream": self.upstream, "files": {}})
        sync.git("merge", "--abort")
        self.assert_versions(self.reconstruct_head(), "1.0.46")

    def test_ai_resolution_cannot_choose_the_version(self):
        sync.git("checkout", "-q", "--detach", self.upstream)
        path = sync.VERSION_MANIFESTS[0]
        self.write(path, '[package]\nname = "xai-grok-pager-bin"\nversion = "1.0.46"\n\n[dependencies]\nfoo = "upstream"\n')
        self.upstream = self.commit("upstream dependency")
        sync.git("checkout", "-q", "--detach", self.base)
        self.write(path, '[package]\nname = "xai-grok-pager-bin"\nversion = "1.0.45"\n\n[dependencies]\nfoo = "fork"\n')
        self.base = self.commit("fork dependency")
        state = self.start()
        self.assertIn(path, state["conflicts"])
        self.write("src/main.rs", "resolved\n")
        self.write(path, '[package]\nname = "xai-grok-pager-bin"\nversion = "9.9.9"\n\n[dependencies]\nfoo = "both"\n')
        self.export()
        sync.git("merge", "--abort")
        head = self.reconstruct_head()
        self.assert_versions(head, "1.0.46")
        self.assertIn('foo = "both"', sync.git("show", f"{head}:{path}").decode())

    def test_prerelease_main_version_fails_closed(self):
        sync.git("checkout", "-q", "--detach", self.base)
        self.write_versions("1.0.46-alpha.1")
        self.base = self.commit("alpha on main")
        with self.assertRaises(sync.SyncError):
            self.start()


    def test_upstream_far_ahead_does_not_choose_fork_release_number(self):
        self.rebuild("99.50.70", "1.0.50")
        self.start()
        self.write("src/main.rs", "resolved\n")
        self.export()
        sync.git("merge", "--abort")
        self.assert_versions(self.reconstruct_head(), "1.0.51")

    def test_version_conflicts_alone_resolve_without_ai(self):
        self.rebuild("99.50.70", "1.0.50")
        self.write("src/main.rs", "upstream\n")
        self.base = self.commit("same source")
        state = self.start()
        self.assertEqual(state["conflicts"], {})
        self.export()
        sync.git("merge", "--abort")
        self.assert_versions(self.reconstruct_head(), "1.0.51")

    def test_internal_dependency_conflict_stays_and_ai_cannot_change_identity(self):
        package = sync.INTERNAL_PACKAGES[0]
        path = sync.VERSION_MANIFESTS[1]
        sync.git("checkout", "-q", "--detach", self.upstream)
        self.write(path, f'[package]\nname = "{package}"\nversion = "10.0.0"\n\n[dependencies]\nfoo = "upstream"\n')
        self.upstream = self.commit("upstream internal dependency")
        sync.git("checkout", "-q", "--detach", self.base)
        self.write(path, f'[package]\nname = "{package}"\nversion = "0.0.0"\n\n[dependencies]\nfoo = "fork"\n')
        self.base = self.commit("fork internal dependency")
        state = self.start()
        self.assertIn(path, state["conflicts"])
        self.write("src/main.rs", "resolved\n")
        self.write(path, f'[package]\nname = "{package}"\nversion = "99.9.9"\n\n[dependencies]\nfoo = "both"\n')
        self.export()
        sync.git("merge", "--abort")
        head = self.reconstruct_head()
        self.assert_versions(head, "1.0.46")
        self.assertIn(b'foo = "both"', sync.git("show", f"{head}:{path}"))

    def test_lock_dependency_conflict_stays_and_resolution_versions_are_overridden(self):
        def lock_with_dependency(value):
            raw = sync.git("show", f"{self.initial}:Cargo.lock")
            return raw.replace(b'version = "1.0.45"\n',
                               b'version = "99.9.9"\ndependencies = ["' + value + b'"]\n', 1)
        sync.git("checkout", "-q", "--detach", self.upstream)
        Path("Cargo.lock").write_bytes(lock_with_dependency(b"upstream"))
        self.upstream = self.commit("upstream lock dependency")
        sync.git("checkout", "-q", "--detach", self.base)
        Path("Cargo.lock").write_bytes(lock_with_dependency(b"fork"))
        self.base = self.commit("fork lock dependency")
        state = self.start()
        self.assertIn("Cargo.lock", state["conflicts"])
        self.write("src/main.rs", "resolved\n")
        Path("Cargo.lock").write_bytes(lock_with_dependency(b"both"))
        self.export()
        sync.git("merge", "--abort")
        head = self.reconstruct_head()
        self.assert_versions(head, "1.0.46")
        self.assertIn(b'dependencies = ["both"]', sync.git("show", f"{head}:Cargo.lock"))

    def test_missing_upstream_manifest_version_fails_closed(self):
        sync.git("checkout", "-q", "--detach", self.upstream)
        path = sync.VERSION_MANIFESTS[1]
        self.write(path, f'[package]\nname = "{sync.INTERNAL_PACKAGES[0]}"\n')
        self.upstream = self.commit("missing version")
        sync.git("checkout", "-q", "--detach", self.base)
        with self.assertRaises(sync.SyncError):
            self.start()

    def test_missing_upstream_lock_package_fails_closed(self):
        sync.git("checkout", "-q", "--detach", self.upstream)
        lock = Path("Cargo.lock")
        blocks = list(sync.LOCK_PACKAGE.finditer(lock.read_bytes()))
        lock.write_bytes(b"".join(m[0] for m in blocks[1:]))
        self.upstream = self.commit("missing lock package")
        sync.git("checkout", "-q", "--detach", self.base)
        with self.assertRaises(sync.SyncError):
            self.start()

    def test_symlink_or_executable_version_file_is_rejected(self):
        path = sync.VERSION_MANIFESTS[1]
        for symlink in (False, True):
            with self.subTest(symlink=symlink):
                sync.git("checkout", "-q", "--detach", self.upstream)
                file = Path(path)
                if symlink:
                    file.unlink()
                    file.symlink_to("../../../untouched.txt")
                else:
                    file.chmod(0o755)
                upstream = self.commit("unsupported version file mode")
                sync.git("checkout", "-q", "--detach", self.base)
                with self.assertRaises(sync.SyncError):
                    sync.begin(self.base, upstream)
                sync.git("merge", "--abort")


if __name__ == "__main__":
    unittest.main()
