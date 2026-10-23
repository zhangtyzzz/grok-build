#!/usr/bin/env python3
"""Fail-closed fork sync transport. Run this helper from TRUSTED main, never AI output.

The artifact is bounded JSON containing regular-file bytes for the independently
recomputed original conflicts. It is not a shell script, patch, or git bundle.
Privileged jobs independently reconstruct both merge parents and the clean merge.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tempfile

UPSTREAM = "https://github.com/xai-org/grok-build.git"
MAX_ARTIFACT = 20 * 1024 * 1024
MAX_FILE = 2 * 1024 * 1024
SHA = re.compile(r"^[0-9a-f]{40}$")
MARKER = re.compile(rb"^(?:<{7,}|={7,}|>{7,}|\|{7,})(?:[ \t\r]|$)", re.MULTILINE)
# Only the executable carries the fork release number. Internal crate identities
# stay identical across main/alpha and upstream imports so release bumps do not
# invalidate the dependency graph. These are not runtime application versions.
BINARY_PACKAGE = "xai-grok-pager-bin"
INTERNAL_PACKAGES = ("xai-grok-pager", "xai-grok-shell", "xai-grok-version")
INTERNAL_VERSION = "0.0.0"
VERSION_PACKAGES = (BINARY_PACKAGE, *INTERNAL_PACKAGES)
VERSION_MANIFESTS = tuple(f"crates/codegen/{p}/Cargo.toml" for p in VERSION_PACKAGES)
VERSION_FILES = (*VERSION_MANIFESTS, "Cargo.lock")
MANIFEST_VERSION = re.compile(rb'^version = "([^"\n]+)"$', re.MULTILINE)
PACKAGE_NAME = re.compile(rb'^name = "([^"\n]+)"$', re.MULTILINE)
PACKAGE_SECTION = re.compile(rb'(?ms)^\[package\]\n(.*?)(?=^\[|\Z)')
LOCK_PACKAGE = re.compile(rb'(?ms)^\[\[package\]\]\n.*?(?=^\[\[package\]\]|\Z)')
STABLE_VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)")


class SyncError(RuntimeError):
    pass


def run(*args: str, check: bool = True, data: bytes | None = None) -> subprocess.CompletedProcess:
    result = subprocess.run(args, input=data, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if check and result.returncode:
        raise SyncError(f"command failed ({result.returncode}): {' '.join(args[:4])}\n"
                        + result.stderr.decode(errors="replace"))
    return result


def git(*args: str, **kwargs) -> bytes:
    return run("git", "-c", "core.hooksPath=/dev/null", *args, **kwargs).stdout


def sha(value: str) -> str:
    if not isinstance(value, str) or not SHA.fullmatch(value):
        raise SyncError("expected a full lowercase 40-character commit SHA")
    return value


def path_safe(path: str) -> str:
    if (not isinstance(path, str) or not path or "\\" in path or "\0" in path
            or any(ord(c) < 32 or ord(c) == 127 for c in path)
            or path.startswith("/") or any(p in ("", ".", "..") for p in path.split("/"))
            or any(p.casefold() == ".git" for p in path.split("/"))):
        raise SyncError(f"unsafe repository path: {path!r}")
    return path


def protected(path: str) -> bool:
    """Keep security contracts, workflows, release tooling and credentials manual."""
    path_safe(path)
    p = PurePosixPath(path)
    parts = tuple(x.casefold() for x in p.parts)
    name = p.name.casefold()
    return (parts[0] in {".github", ".claude", ".codex", ".agents"}
            or parts[:2] in {("scripts", "automation"), ("scripts", "dist")}
            or path.casefold() in {"scripts/dist.sh", "rust-toolchain.toml", "rust-toolchain"}
            or name in {"agents.md", "claude.md", "fork_changes.md", ".gitmodules", ".gitattributes"}
            or name.startswith(".env") or name.endswith((".pem", ".key", ".p12", ".pfx"))
            or any(x in {".ssh", ".aws", ".gnupg"} for x in parts)
            or name in {"auth.json", "credentials", "credentials.json", ".netrc", ".npmrc"})


def json_read(path: str) -> dict:
    file = Path(path)
    if file.is_symlink() or not file.is_file() or file.stat().st_size > MAX_ARTIFACT:
        raise SyncError("resolution artifact exceeds limit")
    raw = file.read_bytes()

    def unique(pairs):
        obj = {}
        for key, value in pairs:
            if key in obj:
                raise SyncError("duplicate JSON key")
            obj[key] = value
        return obj

    obj = json.loads(raw, object_pairs_hook=unique)
    if not isinstance(obj, dict):
        raise SyncError("resolution artifact must be a JSON object")
    return obj


def json_write(path: str, value: dict):
    Path(path).write_text(json.dumps(value, ensure_ascii=True, sort_keys=True) + "\n")


def output(**values):
    for key, value in values.items():
        value = str(value)
        if "\n" in value or "\r" in value:
            raise SyncError("invalid GitHub output")
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as stream:
                stream.write(f"{key}={value}\n")
        print(f"{key}={value}")


def fetch(base: str, upstream: str | None = None) -> tuple[str, str]:
    sha(base)
    git("fetch", "--no-tags", "origin", "main")
    current = git("rev-parse", "FETCH_HEAD").decode().strip()
    if current != base:
        raise SyncError("main advanced; rerun sync from current main")
    git("fetch", "--no-tags", UPSTREAM, "main")
    latest = sha(git("rev-parse", "FETCH_HEAD").decode().strip())
    if upstream is not None and latest != sha(upstream):
        raise SyncError("upstream advanced; rerun sync to lock a fresh pair of SHAs")
    git("checkout", "--detach", base)
    return base, latest


def changed_paths(base: str, upstream: str) -> list[str]:
    ancestor = git("merge-base", base, upstream).decode().strip()
    return [path_safe(p.decode("utf-8")) for p in
            git("diff", "--no-renames", "--name-only", "-z", ancestor, upstream).split(b"\0") if p]


def original_conflicts() -> dict[str, dict[str, str]]:
    stages = {}
    for entry in git("ls-files", "--unmerged", "-z").split(b"\0"):
        if not entry:
            continue
        metadata, name = entry.split(b"\t", 1)
        mode, object_id, stage = metadata.decode().split()
        path = path_safe(name.decode("utf-8"))
        stages.setdefault(path, {})[stage] = f"{mode}:{object_id}"
    for path, entries in stages.items():
        # Only normal, regular-text modify/modify or add/add conflicts are automatic.
        if protected(path) or not {"2", "3"}.issubset(entries):
            raise SyncError(f"manual conflict required: {path}")
        if any(not value.startswith("100644:") for value in entries.values()):
            raise SyncError(f"non-regular or executable conflict requires manual review: {path}")
        for value in entries.values():
            content = git("cat-file", "blob", value.split(":")[1])
            text_bytes(content)
    return stages


def text_bytes(content: bytes):
    if len(content) > MAX_FILE or b"\0" in content:
        raise SyncError("binary or oversized conflict requires manual review")
    try:
        content.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise SyncError("non UTF-8 conflict requires manual review") from exc


def safe_file(path: str) -> Path:
    path_safe(path)
    root = Path.cwd().resolve()
    file = root / path
    for parent in (file, *file.parents):
        if parent == root:
            break
        if parent.is_symlink():
            raise SyncError(f"symlink in resolution path: {path}")
    if not file.resolve().is_relative_to(root):
        raise SyncError("resolution escapes checkout")
    return file


def snapshot() -> dict[str, str]:
    """Capture tracked worktree bytes, including symlink targets; no code execution."""
    result = {}
    for raw in git("ls-files", "-z").split(b"\0"):
        if not raw:
            continue
        path = path_safe(raw.decode("utf-8"))
        p = Path(path)
        if p.is_symlink():
            value = b"symlink:" + os.readlink(p).encode()
        elif p.is_file():
            value = b"file:" + p.read_bytes() + str(p.stat().st_mode & 0o777).encode()
        elif not p.exists():
            value = b"missing"
        else:
            value = b"directory"
        result[path] = hashlib.sha256(value).hexdigest()
    return result


def manifest_package(path: str, content: bytes) -> tuple[re.Match, re.Match]:
    """Find only the owning package's version; never rewrite a dependency field."""
    sections = list(PACKAGE_SECTION.finditer(content))
    if len(sections) != 1:
        raise SyncError(f"missing or duplicate package section: {path}")
    section = sections[0]
    names = list(PACKAGE_NAME.finditer(section[1]))
    versions = list(MANIFEST_VERSION.finditer(section[1]))
    expected = PurePosixPath(path).parent.name.encode()
    if len(names) != 1 or names[0][1] != expected or len(versions) != 1:
        raise SyncError(f"missing, duplicate or renamed release package/version: {path}")
    return section, versions[0]


def fork_version(base: str) -> str:
    path = VERSION_MANIFESTS[0]
    _, match = manifest_package(path, git("show", f"{base}:{path}"))
    version = match[1].decode()
    if not STABLE_VERSION.fullmatch(version):
        raise SyncError("main executable version is not stable SemVer")
    return version


def next_patch(version: str) -> str:
    major, minor, patch = version.split(".")
    return f"{major}.{minor}.{int(patch) + 1}"


def set_version(path: str, content: bytes, version: str) -> bytes:
    """Normalize executable and internal identities without resolving other edits."""
    if path == "Cargo.lock":
        seen = set()

        def replace_package(match):
            block = match[0]
            names = list(PACKAGE_NAME.finditer(block))
            if not names or names[0][1].decode() not in VERSION_PACKAGES:
                return block
            package = names[0][1].decode()
            versions = list(MANIFEST_VERSION.finditer(block))
            if (len(names) != 1 or len(versions) != 1 or package in seen
                    or re.search(rb"^source\s*=", block, re.MULTILINE)):
                raise SyncError(f"ambiguous workspace package in Cargo.lock: {package}")
            seen.add(package)
            desired = version if package == BINARY_PACKAGE else INTERNAL_VERSION
            return MANIFEST_VERSION.sub(f'version = "{desired}"'.encode(), block, count=1)

        content = LOCK_PACKAGE.sub(replace_package, content)
        if seen != set(VERSION_PACKAGES):
            raise SyncError("Cargo.lock is missing a release workspace package version")
        return content
    if path not in VERSION_MANIFESTS:
        raise SyncError(f"unexpected version file: {path}")
    section, match = manifest_package(path, content)
    desired = version if path == VERSION_MANIFESTS[0] else INTERNAL_VERSION
    start, end = section.start(1) + match.start(), section.start(1) + match.end()
    return content[:start] + f'version = "{desired}"'.encode() + content[end:]


def apply_fork_version(base: str) -> bool:
    """Pin version files to main's next patch; resolve conflicts caused only by versions.

    Each side receives the same package-specific versions before a three-way merge,
    so version-only conflicts disappear while dependency/content conflicts remain.
    Returns whether a conflict was resolved.
    """
    version = next_patch(fork_version(base))
    stages = {}
    for entry in git("ls-files", "--unmerged", "-z").split(b"\0"):
        if entry:
            metadata, name = entry.split(b"\t", 1)
            mode, object_id, stage = metadata.decode().split()
            stages.setdefault(name.decode("utf-8"), {})[stage] = (mode, object_id)
    resolved = False
    for path in VERSION_FILES:
        file = safe_file(path)
        if path not in stages:
            if not file.is_file() or file.is_symlink() or file.stat().st_mode & 0o111:
                raise SyncError(f"missing or non-regular version file: {path}")
            file.write_bytes(set_version(path, file.read_bytes(), version))
            git("add", "--", path)
            continue
        entries = stages[path]
        if set(entries) != {"1", "2", "3"} or any(m != "100644" for m, _ in entries.values()):
            continue
        with tempfile.TemporaryDirectory() as temp:
            sides = []
            for stage in ("2", "1", "3"):
                side = Path(temp) / stage
                side.write_bytes(set_version(path, git("cat-file", "blob", entries[stage][1]), version))
                sides.append(str(side))
            merged = run("git", "merge-file", "-p", "--quiet", *sides, check=False)
        if merged.returncode == 0:
            file.write_bytes(merged.stdout)
            git("add", "--", path)
            resolved = True
    return resolved


def begin(base: str, upstream: str) -> dict:
    for path in changed_paths(base, upstream):
        if protected(path):
            raise SyncError(f"upstream touches a protected path; synchronize manually: {path}")
    if run("git", "merge-base", "--is-ancestor", upstream, base, check=False).returncode == 0:
        return {"base": base, "upstream": upstream, "noop": True}
    result = run("git", "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false",
                 "merge", "--no-commit", "--no-ff", upstream, check=False)
    if result.returncode not in (0, 1):
        raise SyncError("merge failed for a reason other than a merge conflict")
    versions_resolved = apply_fork_version(base)
    conflicts = original_conflicts()
    if result.returncode and not conflicts and not versions_resolved:
        raise SyncError("merge failed without supported conflicts")
    return {"base": base, "upstream": upstream, "noop": False,
            "conflicts": conflicts, "snapshot": snapshot()}


def check_markers(paths):
    for path in paths:
        p = Path(path)
        if p.is_file() and not p.is_symlink():
            content = p.read_bytes()
            # Binary/non-UTF8 files from an ordinary clean merge are left to CI.
            if b"\0" not in content and MARKER.search(content):
                raise SyncError(f"unresolved conflict marker in worktree: {path}")


def validate_resolution(obj: dict, expected: dict) -> dict[str, bytes]:
    if (set(obj) != {"schema", "base", "upstream", "files"}
            or type(obj["schema"]) is not int or obj["schema"] != 1):
        raise SyncError("unexpected resolution schema")
    if sha(obj["base"]) != expected["base"] or sha(obj["upstream"]) != expected["upstream"]:
        raise SyncError("resolution SHA does not match independently locked commits")
    files = obj["files"]
    if not isinstance(files, dict) or set(files) != set(expected["conflicts"]):
        raise SyncError("resolution paths must exactly equal the original conflict set")
    decoded = {}
    for path, encoded in files.items():
        path_safe(path)
        if protected(path) or not isinstance(encoded, str):
            raise SyncError("protected path or invalid resolution content")
        try:
            content = base64.b64decode(encoded, validate=True)
        except (ValueError, TypeError) as exc:
            raise SyncError("invalid base64 resolution") from exc
        text_bytes(content)
        if MARKER.search(content):
            raise SyncError(f"unresolved conflict marker in artifact: {path}")
        decoded[path] = content
    return decoded


def prepare(args):
    base, upstream = fetch(args.base)
    state = begin(base, upstream)
    json_write(args.state, state)
    output(base=base, upstream=upstream, changed=str(not state["noop"]).lower(),
           conflicts=str(bool(state.get("conflicts"))).lower())


def export(args):
    state = json_read(args.state)
    current = snapshot()
    allowed = set(state["conflicts"])
    if set(current) != set(state["snapshot"]):
        raise SyncError("AI changed the tracked file set")
    if any(current[p] != digest for p, digest in state["snapshot"].items() if p not in allowed):
        raise SyncError("AI modified a file outside the original conflicts")
    if git("ls-files", "--others", "--exclude-standard", "-z"):
        raise SyncError("unexpected untracked files after resolution")
    # The agent is not allowed to commit, stage, or manipulate the index.
    if original_conflicts() != state["conflicts"]:
        raise SyncError("AI altered the unmerged index; only edit file contents")
    if git("rev-parse", "HEAD").decode().strip() != state["base"]:
        raise SyncError("AI changed HEAD")
    obj = {"schema": 1, "base": state["base"], "upstream": state["upstream"], "files": {}}
    for path in sorted(allowed):
        obj["files"][path] = base64.b64encode(safe_file(path).read_bytes()).decode()
    validate_resolution(obj, state)
    check_markers(git("diff", "--name-only", "HEAD", "-z").decode().strip("\0").split("\0"))
    json_write(args.artifact, obj)


def reconstruct(args):
    # Artifact is untrusted. Fetch known repositories and reconstruct the actual
    # merge independently before accepting any content from it.
    base, upstream = fetch(args.base, args.upstream)
    state = begin(base, upstream)
    if state["noop"]:
        raise SyncError("no merge needed")
    obj = json_read(args.artifact)
    version = next_patch(fork_version(base))
    for path, content in validate_resolution(obj, state).items():
        p = safe_file(path)
        # A resolved version file still gets the fork's version, whatever the artifact says.
        p.write_bytes(set_version(path, content, version) if path in VERSION_FILES else content)
        git("add", "--", path)
    if git("ls-files", "--unmerged", "-z"):
        raise SyncError("unmerged index entries remain")
    changed = [p.decode() for p in git("diff", "--cached", "--name-only", "-z").split(b"\0") if p]
    check_markers(changed)
    git("diff", "--cached", "--check")
    git("diff", "--check")
    tree = sha(git("write-tree").decode().strip())
    # Plumbing creates exactly two independently verified parents. Never import
    # AI's git objects, config, index, commit metadata or executable files.
    commit = sha(git("-c", "user.name=github-actions[bot]", "-c",
                     "user.email=41898282+github-actions[bot]@users.noreply.github.com",
                     "commit-tree", tree, "-p", base, "-p", upstream,
                     data=f"Merge upstream {upstream[:12]}\n".encode()).decode().strip())
    output(head=commit, tree=tree)


def api(endpoint, method="GET", payload=None, paginate=False):
    args = ["gh", "api", "--method", method, endpoint]
    if paginate:
        args.extend(["--paginate", "--slurp"])
    raw = None
    if payload is not None:
        args.extend(["--input", "-"])
        raw = json.dumps(payload).encode()
    return json.loads(run(*args, data=raw).stdout or b"null")


def release_guard(repo: str):
    for workflow in ("auto-release.yml", "release-cache.yml", "release.yml"):
        # Include waiting/requested/queued runs, not only in_progress.
        pages = api(f"repos/{repo}/actions/workflows/{workflow}/runs?per_page=100", paginate=True)
        if any(r["status"] != "completed" for page in pages for r in page["workflow_runs"]):
            raise SyncError(f"{workflow} is active; wait for publication before syncing main")


def guard(args):
    release_guard(args.repo)


def publish(args):
    sha(args.head)
    sha(args.tree)
    sha(args.base)
    sha(args.upstream)
    release_guard(args.repo)
    if api(f"repos/{args.repo}/commits/main")["sha"] != args.base:
        raise SyncError("main advanced before PR creation")
    parents = git("show", "-s", "--format=%P", args.head).decode().strip().split()
    if parents != [args.base, args.upstream]:
        raise SyncError("candidate parents do not match locked commits")
    if git("rev-parse", f"{args.head}^{{tree}}").decode().strip() != args.tree:
        raise SyncError("candidate tree mismatch")
    branch = f"automation/upstream-{args.upstream[:12]}"
    # No force pushes: a collision is intentionally actionable/manual.
    if git("ls-remote", "--heads", "origin", f"refs/heads/{branch}").strip():
        raise SyncError(f"{branch} already exists; review/close the earlier sync PR first")
    # Push credentials exist only in this trusted process, never in AI's job.
    token = os.environ["GH_TOKEN"]
    auth = base64.b64encode(f"x-access-token:{token}".encode()).decode()
    push_env = os.environ.copy()
    push_env.update({"GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": "http.https://github.com/.extraheader",
                     "GIT_CONFIG_VALUE_0": f"AUTHORIZATION: basic {auth}"})
    result = subprocess.run(["git", "-c", "core.hooksPath=/dev/null", "push", "origin",
                             f"{args.head}:refs/heads/{branch}"], env=push_env,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if result.returncode:
        raise SyncError("candidate push failed (credentials and output withheld)")
    pr = api(f"repos/{args.repo}/pulls", "POST", {
        "head": branch, "base": "main", "title": f"Sync upstream {args.upstream[:12]}",
        "body": f"Automated upstream sync.\n\nFork base: `{args.base}`\nUpstream: `{args.upstream}`\n"
                "Full reusable CI must pass on the exact candidate before merging. "
                "Protected paths require manual synchronization.\n"})
    if pr["head"]["sha"] != args.head:
        raise SyncError("created PR head mismatch")
    output(pr=pr["number"], head=args.head, tree=args.tree)


def merge_release(args):
    for value in (args.base, args.head, args.tree):
        sha(value)
    if not args.pr.isdecimal():
        raise SyncError("invalid PR number")
    release_guard(args.repo)
    pr = api(f"repos/{args.repo}/pulls/{args.pr}")
    if (pr["state"] != "open" or pr["draft"] or pr["base"]["ref"] != "main"
            or pr["base"]["repo"]["full_name"] != args.repo
            or pr["head"]["repo"]["full_name"] != args.repo
            or pr["head"]["sha"] != args.head
            or pr["base"]["sha"] != args.base):
        raise SyncError("PR changed while CI was running; rerun/review manually")
    if api(f"repos/{args.repo}/commits/main")["sha"] != args.base:
        raise SyncError("main advanced during CI")
    candidate = api(f"repos/{args.repo}/git/commits/{args.head}")
    if candidate["tree"]["sha"] != args.tree:
        raise SyncError("tested candidate tree mismatch")
    merged = api(f"repos/{args.repo}/pulls/{args.pr}/merge", "PUT", {
        "sha": args.head, "merge_method": "merge"})
    if not merged.get("merged"):
        raise SyncError("GitHub refused the merge")
    merged_sha = sha(merged["sha"])
    merged_commit = api(f"repos/{args.repo}/git/commits/{merged_sha}")
    if merged_commit["tree"]["sha"] != args.tree:
        raise SyncError("merged tree differs from tested tree; no release dispatched")
    if api(f"repos/{args.repo}/commits/main")["sha"] != merged_sha:
        raise SyncError("main advanced after merge; no release dispatched")
    path = VERSION_MANIFESTS[0]
    manifest = api(f"repos/{args.repo}/contents/{path}?ref={merged_sha}")
    if manifest.get("encoding") != "base64":
        raise SyncError("unexpected release manifest encoding")
    # GitHub wraps content API base64 with newlines.
    content = base64.b64decode("".join(manifest["content"].split()), validate=True)
    _, match = manifest_package(path, content)
    version = match[1].decode()
    if not STABLE_VERSION.fullmatch(version):
        raise SyncError("main executable version is not stable SemVer")
    tags = api(f"repos/{args.repo}/git/matching-refs/tags/v{version}")
    if any(ref["ref"] == f"refs/tags/v{version}" for ref in tags):
        print("Version already tagged; sync merged without a duplicate release")
        return
    # The coordinator rechecks the executable version, tag absence and expected
    # commit. A push racing dispatch must fail closed, never publish it.
    api(f"repos/{args.repo}/actions/workflows/auto-release.yml/dispatches", "POST", {
        "ref": "main", "inputs": {"expected_commit": merged_sha}})
    print(f"Merged {merged_sha}; dispatched existing warm/tag/release pipeline")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("prepare", "export", "reconstruct", "guard", "publish", "merge-release"):
        cmd = commands.add_parser(name)
        if name in ("prepare", "reconstruct", "publish", "merge-release"):
            cmd.add_argument("--base", required=True)
        if name in ("reconstruct", "publish"):
            cmd.add_argument("--upstream", required=True)
        if name in ("prepare", "export"):
            cmd.add_argument("--state", required=True)
        if name in ("export", "reconstruct"):
            cmd.add_argument("--artifact", required=True)
        if name in ("guard", "publish", "merge-release"):
            cmd.add_argument("--repo", required=True)
        if name in ("publish", "merge-release"):
            cmd.add_argument("--head", required=True)
            cmd.add_argument("--tree", required=True)
        if name == "merge-release":
            cmd.add_argument("--pr", required=True)
    args = parser.parse_args()
    try:
        globals()[args.command.replace("-", "_")](args)
    except (SyncError, ValueError, KeyError, OSError) as exc:
        print(f"upstream-sync stopped: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
