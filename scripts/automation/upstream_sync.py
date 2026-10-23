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


# This is deliberately a bounded migration, not a Rust formatter or compiler. It
# recognizes upstream's old version API while keeping comments/literals opaque.
# Unknown const expressions, ambiguous bindings and module aliases require manual
# synchronization; unsupported macro expansion/pattern contexts remain CI's job.
RUNTIME_NAMES = {"VERSION": "version", "IS_DEV_BUILD": "is_dev_build",
                 "PAGER_CLIENT_VERSION": "pager_client_version",
                 "CLIENT_LEADER_VERSION": "client_leader_version"}
RUNTIME_HINT = re.compile(rb"VERSION|IS_DEV_BUILD")
RUST_RAW = re.compile(r'(?:br|cr|r)(#*)"')
RUST_QUOTED = re.compile(r'(?:b|c)?"')
RUST_CHAR = re.compile(r"b?'(?:[^'\\\n]|\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.))'")
RUST_IDENT = re.compile(r"(?:r#)?(?:[A-Za-z_]|[^\x00-\x7f])(?:[A-Za-z_0-9]|[^\x00-\x7f])*")


def runtime_source(path: str) -> bool:
    return (path.startswith("crates/") and path.endswith(".rs")
            and not path.startswith("crates/codegen/xai-grok-version/"))


def rust_tokens(source: str) -> list[tuple[str, int, int, str]]:
    """Token spans only: skip nested comments; keep Rust literals indivisible."""
    tokens = []
    i = 0
    while i < len(source):
        start = i
        if source[i].isspace():
            i += 1
            continue
        if source.startswith("//", i):
            end = source.find("\n", i)
            i = len(source) if end < 0 else end + 1
            continue
        if source.startswith("/*", i):
            depth, i = 1, i + 2
            while depth and i < len(source):
                if source.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif source.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            if depth:
                raise SyncError("unterminated Rust comment in runtime version migration")
            continue
        raw = RUST_RAW.match(source, i)
        if raw:
            closing = '"' + raw[1]
            end = source.find(closing, raw.end())
            if end < 0:
                raise SyncError("unterminated Rust raw literal in runtime version migration")
            i = end + len(closing)
            tokens.append((source[start:i], start, i, "string"))
            continue
        quoted = RUST_QUOTED.match(source, i)
        # A lifetime is not a char literal. Rust chars contain one character or an
        # escape; consume a quoted char only when its closing quote is present.
        char = RUST_CHAR.match(source, i)
        if char:
            i = char.end()
            tokens.append((source[start:i], start, i, "literal"))
            continue
        if quoted:
            i = quoted.end()
            while i < len(source) and source[i] != '"':
                i += 2 if source[i] == "\\" else 1
            if i >= len(source):
                raise SyncError("unterminated Rust string in runtime version migration")
            i += 1
            tokens.append((source[start:i], start, i, "string"))
            continue
        ident = RUST_IDENT.match(source, i)
        if ident:
            i = ident.end()
            kind = "ident"
        else:
            i += 2 if source.startswith("::", i) else 1
            kind = "punct"
        tokens.append((source[start:i], start, i, kind))
    return tokens


def runtime_getter(path: str, names: list[str]) -> str | None:
    """Resolve only the known legacy API paths, never arbitrary VERSION names."""
    if len(names) == 2 and names[0] == "xai_grok_version":
        return {"VERSION": "version", "IS_DEV_BUILD": "is_dev_build"}.get(names[-1])
    if names == ["xai_grok_tools", "VERSION"]:
        return "version"
    if (len(names) >= 2 and names[-2:] == ["client_identity", "PAGER_CLIENT_VERSION"]
            and names[0] in {"crate", "super", "xai_grok_pager"}):
        return "pager_client_version"
    if (names[-1:] == ["CLIENT_LEADER_VERSION"]
            and path.startswith("crates/codegen/xai-grok-shell/src/leader/")
            and names[:-1] in (["super"], ["crate", "leader"])):
        return "client_leader_version"
    if (names == ["crate", "VERSION"]
            and path.startswith("crates/codegen/xai-grok-tools/")):
        return "version"
    return None


def normalize_runtime_version(path: str, content: bytes) -> bytes:
    """Mechanically migrate known old runtime identities, including AI output.

    No repository code/tools are executed. This same byte transform runs on clean
    merged files, each original conflict side, and independently accepted output.
    Names inside comments and ordinary strings are never rewritten. The version
    library's API implementation/build script remains a manual architecture change.
    """
    if not runtime_source(path) or not RUNTIME_HINT.search(content):
        return content
    text_bytes(content)
    source = content.decode()
    tokens = rust_tokens(source)
    values = [t[0] for t in tokens]
    edits = {}
    covered = set()
    bindings = {}
    declarations = set()
    binding_getters = {}
    use_bindings = []
    introduced = set()
    pairs, stack = {}, []
    for i, value in enumerate(values):
        if value in ("(", "[", "{"):
            stack.append(i)
        elif value in (")", "]", "}"):
            if not stack or values[stack[-1]] != {")": "(", "]": "[", "}": "{"}[value]:
                raise SyncError(f"unbalanced Rust delimiters: {path}")
            opening = stack.pop()
            pairs[opening] = i
    if stack:
        raise SyncError(f"unbalanced Rust delimiters: {path}")

    def edit(first, last, replacement):
        span = (tokens[first][1], tokens[last][2])
        if span in edits and edits[span] != replacement:
            raise SyncError(f"ambiguous runtime version replacement: {path}")
        edits[span] = replacement

    def bind(old, new, declaration, getter, existing=False):
        if old in bindings and (bindings[old] != new or binding_getters[old] != getter):
            raise SyncError(f"ambiguous runtime version binding {old}: {path}")
        bindings[old] = new
        binding_getters[old] = getter
        declarations.add(declaration)
        if not existing and old != new:
            introduced.add(new)

    # Parse use trees, including grouped/nested imports and explicit item aliases.
    # Module aliases/globs from the version crate are intentionally not guessed.
    def use_tree(start, end, prefix):
        if start == end:
            return
        names, cursor = list(prefix), start
        if values[cursor] == "::":
            cursor += 1
        while cursor < end:
            if values[cursor] == "{":
                closing = pairs[cursor]
                part, j = cursor + 1, cursor + 1
                while j <= closing:
                    if j == closing or values[j] == ",":
                        use_tree(part, j, names)
                        part = j + 1
                    elif values[j] == "{":
                        j = pairs[j]
                    j += 1
                if closing != end - 1:
                    raise SyncError(f"unsupported use tree in runtime migration: {path}")
                return
            name_at = cursor
            names.append(values[cursor])
            cursor += 1
            if cursor < end and values[cursor] == "::":
                cursor += 1
                continue
            alias_at = None
            if cursor < end and values[cursor] == "as" and cursor + 2 == end:
                alias_at = cursor + 1
                cursor = end
            if cursor != end:
                raise SyncError(f"unsupported use syntax in runtime migration: {path}")
            getter = runtime_getter(path, names)
            local_at = alias_at if alias_at is not None else name_at
            use_bindings.append((values[local_at], local_at))
            if getter:
                old = values[alias_at] if alias_at is not None else names[-1]
                new = old if alias_at is not None else getter
                if old == "_":
                    raise SyncError(f"anonymous legacy version import: {path}")
                edit(name_at, name_at, getter)
                bind(old, new, local_at, getter)
            else:
                # A clean merge can retain the fork's getter import while adding
                # upstream references to its old constant. Recover only these
                # known canonical bindings, not arbitrary similarly named locals.
                local = values[local_at]
                for legacy, current in RUNTIME_NAMES.items():
                    canonical = runtime_getter(path, names[:-1] + [legacy])
                    if local == current and canonical == names[-1]:
                        bind(legacy, current, local_at, canonical, existing=True)
                if names == ["xai_grok_version", "version"]:
                    for legacy in ("PAGER_CLIENT_VERSION", "CLIENT_LEADER_VERSION"):
                        if local == RUNTIME_NAMES[legacy]:
                            bind(legacy, local, local_at, "version", existing=True)
            if names[0] == "xai_grok_version" and names[-1] in {"xai_grok_version", "self", "*"}:
                raise SyncError(f"version module alias/glob requires manual migration: {path}")
            return

    for i, value in enumerate(values):
        if value != "use" or values[i + 1:i + 2] == ["<"]:
            # `impl Trait + use<T>` is a precise capture bound, not an import.
            continue
        end = i + 1
        while end < len(tokens) and values[end] != ";":
            end += 1
        if end == len(tokens):
            raise SyncError(f"unterminated Rust use: {path}")
        use_tree(i + 1, end, [])
        covered.update(range(i, end + 1))

    # Alias constants whose entire initializer is a known version identity become
    # imports of the getter. Preserve visibility and the original alias spelling,
    # except for the two public contracts already migrated in this fork.
    for i, value in enumerate(values):
        if value not in {"const", "static"} or i in covered:
            continue
        end = i + 1
        while end < len(tokens) and values[end] not in {";", "{"}:
            end += 1
        if end == len(tokens) or values[end] != ";" or "=" not in values[i:end]:
            continue
        equal = values.index("=", i, end)
        rhs = values[equal + 1:end]
        getter = runtime_getter(path, rhs[::2]) if rhs[1::2] == ["::"] * (len(rhs) // 2) else None
        imported = len(rhs) == 1 and rhs[0] in bindings
        if not getter and not imported:
            if any(name in bindings for name in rhs) or any(
                    runtime_getter(path, rhs[j:j + 3:2]) for j in range(max(0, len(rhs) - 2))
                    if rhs[j + 1] == "::"):
                raise SyncError(f"nontrivial runtime version constant requires manual migration: {path}")
            continue
        if imported:
            # Reexporting directly would orphan the original private import;
            # reexporting that private alias can also change visibility. This is
            # outside upstream's known fully-qualified constant declarations.
            raise SyncError(f"constant through an imported version alias requires manual migration: {path}")
        if value != "const" or values[i + 2:equal] not in ([":", "&", "str"], [":", "bool"]):
            raise SyncError(f"nontrivial runtime version constant requires manual migration: {path}")
        declaration_text = source[tokens[i][1]:tokens[end][2]]
        if "//" in declaration_text or "/*" in declaration_text:
            raise SyncError(f"comment inside version constant requires manual migration: {path}")
        old = values[i + 1]
        new = RUNTIME_NAMES.get(old, old) if old in {"PAGER_CLIENT_VERSION", "CLIENT_LEADER_VERSION"} else old
        target = "is_dev_build" if getter == "is_dev_build" else "version"
        edit(i, end, f"use xai_grok_version::{target} as {new};")
        covered.update(range(i, end + 1))
        bind(old, new, i + 1, target)

    for name, declaration in use_bindings:
        if name in bindings and declaration not in declarations:
            raise SyncError(f"ambiguous runtime version import {name}: {path}")
    for i, value in enumerate(values):
        if value in introduced and i not in covered:
            if i == 0 or values[i - 1] not in {".", "::"}:
                raise SyncError(f"runtime getter name already bound {value}: {path}")

    # Refuse lexical shadowing instead of guessing which same-named binding a
    # reference denotes. Fields/path members and explicitly named format args are
    # excluded below; other unfamiliar forms must pass the ordinary Rust CI gates.
    for i, value in enumerate(values):
        if value not in bindings or i in covered or i in declarations:
            continue
        previous = values[i - 1] if i else ""
        following = values[i + 1] if i + 1 < len(tokens) else ""
        if previous in {"let", "mut", "fn", "const", "static", "struct", "enum", "type", "mod", "as"} or following == ":":
            raise SyncError(f"shadowed/ambiguous runtime version binding {value}: {path}")

    # Qualified values and references to recognized imports/constant aliases.
    i = 0
    while i < len(tokens):
        if i in covered or tokens[i][3] != "ident":
            i += 1
            continue
        end, names = i, [values[i]]
        while end + 2 < len(tokens) and values[end + 1] == "::" and tokens[end + 2][3] == "ident":
            end += 2
            names.append(values[end])
        getter = runtime_getter(path, names)
        if getter:
            edit(end, end, getter + "()")
            i = end + 1
            continue
        previous = values[i - 1] if i else ""
        following = values[i + 1] if i + 1 < len(tokens) else ""
        if len(names) == 1 and values[i] in bindings and previous not in {".", "::", "$"} and not (following == "=" and values[i + 2:i + 3] != ["="]):
            edit(i, i, bindings[values[i]] + "()")
        i = end + 1

    # Only existing shell/pager application metadata is changed; other crates'
    # independent package versions and the binary's stamp inputs remain intact.
    application_library = any(path.startswith(f"crates/codegen/{p}/") for p in INTERNAL_PACKAGES[:2])
    binary = path.startswith("crates/codegen/xai-grok-pager-bin/")
    for i, value in enumerate(values):
        if i in covered:
            continue
        if (application_library and values[i:i + 5] == ["env", "!", "(", '"CARGO_PKG_VERSION"', ")"]):
            edit(i, i + 4, "xai_grok_version::version()")
        if (not binary and values[i:i + 9] in (
                ["option_env", "!", "(", '"GROK_VERSION"', ")", ".", "is_none", "(", ")"],
                ["option_env", "!", "(", '"GROK_VERSION"', ")", ".", "is_some", "(", ")"])):
            prefix = "!" if values[i + 6] == "is_some" else ""
            edit(i, i + 8, prefix + "xai_grok_version::is_dev_build()")

    # Preserve format strings byte-for-byte. Give known captured old bindings
    # explicit named arguments in standard formatting/assertion macros only.
    format_at = {"format": 0, "format_args": 0, "println": 0, "eprintln": 0,
                 "print": 0, "eprint": 0, "panic": 0, "write": 1, "writeln": 1,
                 "assert": 1, "debug_assert": 1, "assert_eq": 2, "assert_ne": 2,
                 "debug_assert_eq": 2, "debug_assert_ne": 2}
    for opening, closing in pairs.items():
        if opening < 2 or values[opening - 1] != "!" or values[opening - 2] not in format_at:
            continue
        args, j, start = [], opening + 1, opening + 1
        while j <= closing:
            if j == closing or values[j] == ",":
                if start < j:
                    args.append((start, j))
                start = j + 1
            elif j in pairs:
                j = pairs[j]
            j += 1
        index = format_at[values[opening - 2]]
        if len(args) <= index:
            continue
        first, end = args[index]
        if end != first + 1 or tokens[first][3] != "string":
            continue
        literal = values[first]
        # Escaped braces are literal text, including raw strings.
        captures = set(re.findall(r"\{([A-Za-z_][A-Za-z_0-9]*)(?=[:}])", literal.replace("{{", "").replace("}}", "")))
        explicit = {values[a] for a, b in args[index + 1:] if b > a + 1 and values[a + 1] == "="}
        needed = sorted(captures.intersection(bindings).difference(explicit))
        if needed:
            prefix = " " if values[closing - 1] == "," else ", "
            addition = prefix + ", ".join(f"{name} = {bindings[name]}()" for name in needed)
            at = tokens[closing][1]
            edits[(at, at)] = addition

    ordered = sorted(edits.items())
    for ((_, end), _), ((start, _), _) in zip(ordered, ordered[1:]):
        if end > start:
            raise SyncError(f"overlapping runtime migrations require manual review: {path}")
    for (start, end), replacement in reversed(ordered):
        source = source[:start] + replacement + source[end:]
    return source.encode()


def apply_runtime_versions() -> bool:
    """Normalize clean Rust sources and retry genuine three-way text conflicts."""
    stages = {}
    for entry in git("ls-files", "--stage", "-z").split(b"\0"):
        if entry:
            metadata, name = entry.split(b"\t", 1)
            mode, object_id, stage = metadata.decode().split()
            path = path_safe(name.decode("utf-8"))
            if runtime_source(path) and not protected(path):
                stages.setdefault(path, {})[stage] = (mode, object_id)
    resolved = False
    for path, entries in stages.items():
        if set(entries) == {"0"}:
            if entries["0"][0] != "100644":
                # Never follow symlinks or rewrite executable sources.
                continue
            file = safe_file(path)
            content = file.read_bytes()
            normalized = normalize_runtime_version(path, content)
            if normalized != content:
                file.write_bytes(normalized)
                git("add", "--", path)
        elif set(entries) == {"1", "2", "3"} and all(m == "100644" for m, _ in entries.values()):
            with tempfile.TemporaryDirectory() as temp:
                sides, changed = [], False
                for stage in ("2", "1", "3"):
                    content = git("cat-file", "blob", entries[stage][1])
                    normalized = normalize_runtime_version(path, content)
                    changed |= normalized != content
                    side = Path(temp) / stage
                    side.write_bytes(normalized)
                    sides.append(str(side))
                if not changed:
                    continue
                merged = run("git", "merge-file", "-p", "--quiet", *sides, check=False)
            if merged.returncode == 0:
                safe_file(path).write_bytes(merged.stdout)
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
    runtime_resolved = apply_runtime_versions()
    conflicts = original_conflicts()
    if result.returncode and not conflicts and not (versions_resolved or runtime_resolved):
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
        p.write_bytes(set_version(path, content, version) if path in VERSION_FILES
                      else normalize_runtime_version(path, content))
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
