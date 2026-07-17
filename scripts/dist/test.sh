#!/usr/bin/env bash

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
DIST="$REPO_ROOT/scripts/dist.sh"

cleanup() {
    if [[ -n "${TEST_ROOT:-}" && -d "$TEST_ROOT" ]]; then
        case "$TEST_ROOT" in
            "${TMPDIR:-/tmp}"/grok-dist-test.*) rm -rf -- "$TEST_ROOT" ;;
            *) echo "refusing to remove unexpected test directory: $TEST_ROOT" >&2 ;;
        esac
    fi
}

TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/grok-dist-test.XXXXXX")"
trap cleanup EXIT

node "$HERE/policy.test.mjs"
node "$HERE/artifact-format.test.mjs"
if node "$HERE/manifest.mjs" \
    --stage-dir "$TEST_ROOT" \
    --version 0.0.0-test.0 \
    --target aarch64-apple-darwin \
    --git-commit 0000000000000000000000000000000000000000 \
    --source-rev 0000000000000000000000000000000000000000 \
    --source-date-epoch 1700000000 \
    --executable grok \
    --dirty invalid \
    --build-dirty true \
    --attestation-verified false >/dev/null 2>&1; then
    echo "invalid manifest boolean unexpectedly passed" >&2
    exit 1
fi
printf 'root/\nroot/file\nroot/file\n' |
    if node "$HERE/validate-archive-paths.mjs" >/dev/null 2>&1; then
        echo "duplicate normalized archive path unexpectedly passed" >&2
        exit 1
    fi
printf 'root/\nroot/./file\n' |
    if node "$HERE/validate-archive-paths.mjs" >/dev/null 2>&1; then
        echo "dot-component archive path unexpectedly passed" >&2
        exit 1
    fi
printf 'root/\nroot/file\n' | node "$HERE/validate-archive-paths.mjs"

[[ "$(node -e '
    const targets = require(process.argv[1]);
    process.stdout.write(String(Object.keys(targets).length));
' "$HERE/targets.json")" == "6" ]]
node "$HERE/target.mjs" aarch64-apple-darwin >/dev/null
node "$HERE/target.mjs" x86_64-apple-darwin >/dev/null
node "$HERE/target.mjs" aarch64-unknown-linux-gnu >/dev/null
node "$HERE/target.mjs" x86_64-unknown-linux-gnu >/dev/null
node "$HERE/target.mjs" aarch64-pc-windows-msvc >/dev/null
node "$HERE/target.mjs" x86_64-pc-windows-msvc >/dev/null
if node "$HERE/target.mjs" unsupported-target >/dev/null 2>&1; then
    echo "unsupported target unexpectedly succeeded" >&2
    exit 1
fi
if MACOSX_DEPLOYMENT_TARGET=not-a-version \
    bash "$HERE/prepare-release-tools.sh" \
        --target aarch64-apple-darwin \
        --output-dir "$TEST_ROOT/invalid-macos-tools" \
        --protoc-only >/dev/null 2>&1; then
    echo "invalid macOS deployment target unexpectedly passed" >&2
    exit 1
fi

FAKE_BINARY="$TEST_ROOT/fake-grok"
printf '#!/usr/bin/env sh\nexit 0\n' >"$FAKE_BINARY"
chmod 0755 "$FAKE_BINARY"

OUTPUT="$TEST_ROOT/output"
WRONG_ARCH_TOOL="$TEST_ROOT/x86_64-tool"
node -e '
    const fs = require("node:fs");
    const body = Buffer.alloc(32);
    body.writeUInt32LE(0xfeedfacf, 0);
    body.writeUInt32LE(0x01000007, 4);
    fs.writeFileSync(process.argv[1], body);
' "$WRONG_ARCH_TOOL"
if GROK_TOOLS_BUNDLE_RG_PATH="$WRONG_ARCH_TOOL" \
    GROK_TOOLS_BUNDLE_RG_VERSION="1.0.0" \
    GROK_TOOLS_BUNDLE_BFS_PATH="$WRONG_ARCH_TOOL" \
    GROK_TOOLS_BUNDLE_BFS_VERSION="1.0.0" \
    GROK_TOOLS_BUNDLE_UGREP_PATH="$WRONG_ARCH_TOOL" \
    GROK_TOOLS_BUNDLE_UGREP_VERSION="1.0.0" \
    "$DIST" package \
        --target aarch64-apple-darwin \
        --version 0.0.0-test.0 \
        --binary "$FAKE_BINARY" \
        --output-dir "$OUTPUT/wrong-tool-arch" \
        --source-date-epoch 1700000000 \
        --allow-dirty \
        --allow-unattested >/dev/null 2>&1; then
    echo "wrong-architecture bundled tool unexpectedly passed" >&2
    exit 1
fi

if GROK_DIST_RUSTC_VERSION="rustc 1.92.0 (test)" \
    GROK_DIST_CARGO_VERSION="cargo 1.92.0 (test)" \
    "$DIST" package \
        --target aarch64-apple-darwin \
        --version 0.0.0-test.1 \
        --binary "$FAKE_BINARY" \
        --output-dir "$OUTPUT/missing-attestation" \
        --source-date-epoch 1700000000 \
        --allow-dirty \
        --allow-unbundled-tools >/dev/null 2>&1; then
    echo "unattested package unexpectedly succeeded without diagnostic override" >&2
    exit 1
fi

ARCHIVE="$(
    GROK_DIST_RUSTC_VERSION="rustc 1.92.0 (test)" \
    GROK_DIST_CARGO_VERSION="cargo 1.92.0 (test)" \
    "$DIST" package \
        --target aarch64-apple-darwin \
        --version 0.0.0-test.1 \
        --binary "$FAKE_BINARY" \
        --output-dir "$OUTPUT" \
        --source-date-epoch 1700000000 \
        --allow-dirty \
        --allow-unbundled-tools \
        --allow-unattested
)"

[[ -f "$ARCHIVE" ]]
[[ -f "$OUTPUT/SHA256SUMS" ]]
"$DIST" verify "$ARCHIVE"
"$DIST" checksums --output-dir "$OUTPUT"

SECOND_OUTPUT="$TEST_ROOT/output-second"
SECOND_ARCHIVE="$(
    GROK_DIST_RUSTC_VERSION="rustc 1.92.0 (test)" \
    GROK_DIST_CARGO_VERSION="cargo 1.92.0 (test)" \
    "$DIST" package \
        --target aarch64-apple-darwin \
        --version 0.0.0-test.1 \
        --binary "$FAKE_BINARY" \
        --output-dir "$SECOND_OUTPUT" \
        --source-date-epoch 1700000000 \
        --allow-dirty \
        --allow-unbundled-tools \
        --allow-unattested
)"
cmp "$ARCHIVE" "$SECOND_ARCHIVE"
printf 'tamper' >>"$SECOND_ARCHIVE"
if "$DIST" verify "$SECOND_ARCHIVE" >/dev/null 2>&1; then
    echo "archive with a mismatched external checksum unexpectedly verified" >&2
    exit 1
fi

WINDOWS_OUTPUT="$OUTPUT"
WINDOWS_ARCHIVE="$(
    GROK_DIST_RUSTC_VERSION="rustc 1.92.0 (test)" \
    GROK_DIST_CARGO_VERSION="cargo 1.92.0 (test)" \
    "$DIST" package \
        --target x86_64-pc-windows-msvc \
        --version 0.0.0-test.1 \
        --binary "$FAKE_BINARY" \
        --output-dir "$WINDOWS_OUTPUT" \
        --source-date-epoch 1700000000 \
        --allow-dirty \
        --allow-unbundled-tools \
        --allow-unattested
)"
[[ "$WINDOWS_ARCHIVE" == *.zip ]]
"$DIST" verify "$WINDOWS_ARCHIVE"
[[ "$(wc -l <"$OUTPUT/SHA256SUMS" | tr -d '[:space:]')" == "2" ]]
"$DIST" verify "$ARCHIVE"

WINDOWS_TZ_OUTPUT="$TEST_ROOT/output-windows-other-tz"
WINDOWS_TZ_ARCHIVE="$(
    TZ=Asia/Shanghai \
    GROK_DIST_RUSTC_VERSION="rustc 1.92.0 (test)" \
    GROK_DIST_CARGO_VERSION="cargo 1.92.0 (test)" \
    "$DIST" package \
        --target x86_64-pc-windows-msvc \
        --version 0.0.0-test.1 \
        --binary "$FAKE_BINARY" \
        --output-dir "$WINDOWS_TZ_OUTPUT" \
        --source-date-epoch 1700000000 \
        --allow-dirty \
        --allow-unbundled-tools \
        --allow-unattested
)"
cmp "$WINDOWS_ARCHIVE" "$WINDOWS_TZ_ARCHIVE"
"$DIST" verify "$WINDOWS_TZ_ARCHIVE"

EXTRACTED="$TEST_ROOT/extracted"
mkdir -p "$EXTRACTED"
tar -xzf "$ARCHIVE" -C "$EXTRACTED"
STAGED_ROOT="$EXTRACTED/$(basename "$ARCHIVE" .tar.gz)"
"$DIST" verify "$STAGED_ROOT"
[[ -s "$STAGED_ROOT/BUNDLED-TOOLS-NOTICES.md" ]]

FORGED_ROOT="$TEST_ROOT/forged-missing-license"
cp -R "$STAGED_ROOT" "$FORGED_ROOT"
node -e '
    const crypto = require("node:crypto");
    const fs = require("node:fs");
    const path = require("node:path");
    const root = process.argv[1];
    fs.rmSync(path.join(root, "LICENSE"));
    const manifestPath = path.join(root, "build-manifest.json");
    const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
    manifest.files = manifest.files.filter((record) => record.path !== "LICENSE");
    fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
    const files = [];
    const walk = (directory) => {
      for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
        const file = path.join(directory, entry.name);
        if (entry.isDirectory()) walk(file);
        else if (entry.name !== "MANIFEST.sha256") files.push(file);
      }
    };
    walk(root);
    const checksums = files
      .map((file) => {
        const digest = crypto
          .createHash("sha256")
          .update(fs.readFileSync(file))
          .digest("hex");
        const relative = path.relative(root, file).split(path.sep).join("/");
        return `${digest}  ${relative}`;
      })
      .sort()
      .join("\n");
    fs.writeFileSync(path.join(root, "MANIFEST.sha256"), `${checksums}\n`);
' "$FORGED_ROOT"
if "$DIST" verify "$FORGED_ROOT" >/dev/null 2>&1; then
    echo "payload with a self-consistent missing license unexpectedly verified" >&2
    exit 1
fi

if [[ "$(node -p 'process.platform')" != "win32" ]]; then
    chmod 0755 "$STAGED_ROOT/LICENSE"
    if "$DIST" verify "$STAGED_ROOT" >/dev/null 2>&1; then
        echo "payload with a tampered executable bit unexpectedly verified" >&2
        exit 1
    fi
    chmod 0644 "$STAGED_ROOT/LICENSE"
fi

printf '\ntampered\n' >>"$STAGED_ROOT/LICENSE"
if "$DIST" verify "$STAGED_ROOT" >/dev/null 2>&1; then
    echo "tampered payload unexpectedly verified" >&2
    exit 1
fi

grep -F "$(basename "$ARCHIVE")" "$OUTPUT/SHA256SUMS" >/dev/null
CHECKSUM_COPY="$TEST_ROOT/checksum-copy"
mkdir -p "$CHECKSUM_COPY"
cp "$ARCHIVE" "$CHECKSUM_COPY/$(basename "$ARCHIVE")"
printf '%064d  another-archive.tar.gz\n' 0 >"$CHECKSUM_COPY/SHA256SUMS"
if "$DIST" verify "$CHECKSUM_COPY/$(basename "$ARCHIVE")" >/dev/null 2>&1; then
    echo "archive missing from adjacent SHA256SUMS unexpectedly verified" >&2
    exit 1
fi

ATTESTED_BINARY="$TEST_ROOT/attested-grok"
node -e '
    const fs = require("node:fs");
    const body = Buffer.alloc(4096);
    body.writeUInt32LE(0xfeedfacf, 0);
    body.writeUInt32LE(0x0100000c, 4);
    body.writeUInt32LE(3, 8);
    body.writeUInt32LE(2, 12);
    body.writeUInt32LE(3, 16);
    body.writeUInt32LE(120, 20);
    body.writeUInt32LE(0x19, 32);
    body.writeUInt32LE(72, 36);
    Buffer.from("__TEXT").copy(body, 40);
    body.writeBigUInt64LE(0x100000000n, 56);
    body.writeBigUInt64LE(4096n, 64);
    body.writeBigUInt64LE(0n, 72);
    body.writeBigUInt64LE(4096n, 80);
    body.writeUInt32LE(7, 88);
    body.writeUInt32LE(5, 92);
    body.writeUInt32LE(0x80000028, 104);
    body.writeUInt32LE(24, 108);
    body.writeBigUInt64LE(160n, 112);
    body.writeUInt32LE(0x32, 128);
    body.writeUInt32LE(24, 132);
    body.writeUInt32LE(1, 136);
    body.writeUInt32LE(0x000b0000, 140);
    body.writeUInt32LE(0x000b0000, 144);
    body.writeUInt32LE(0, 148);
    fs.writeFileSync(process.argv[1], body);
' "$ATTESTED_BINARY"
chmod 0755 "$ATTESTED_BINARY"
GIT_COMMIT="$(git -C "$REPO_ROOT" rev-parse HEAD)"
SOURCE_REV="$(tr -d '[:space:]' <"$REPO_ROOT/SOURCE_REV")"
ATTESTATION="$TEST_ROOT/fake.build-attestation.json"
node "$HERE/attestation.mjs" create \
    --output "$ATTESTATION" \
    --binary "$ATTESTED_BINARY" \
    --version 0.0.0-test.2 \
    --target aarch64-apple-darwin \
    --git-commit "$GIT_COMMIT" \
    --source-rev "$SOURCE_REV" \
    --dirty true \
    --build-start-git-commit "$GIT_COMMIT" \
    --build-start-source-rev "$SOURCE_REV" \
    --build-start-dirty true \
    --build-start-status-sha256 0000000000000000000000000000000000000000000000000000000000000000 \
    --build-end-git-commit "$GIT_COMMIT" \
    --build-end-source-rev "$SOURCE_REV" \
    --build-end-dirty true \
    --build-end-status-sha256 0000000000000000000000000000000000000000000000000000000000000000 \
    --source-date-epoch 1700000000 \
    --profile release-dist \
    --features default,jemalloc,sandbox-enforce,release-dist \
    --rustc "rustc 1.92.0 (test)" \
    --cargo "cargo 1.92.0 (test)" \
    --rustflags unset \
    --cargo-encoded-rustflags unset \
    --target-rustflags-name CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS \
    --target-rustflags unset \
    --macosx-deployment-target 11.0

node -e '
    const fs = require("node:fs");
    const value = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    if (value.build.environment.rustflags !== "unset"
        || value.build.environment.cargoEncodedRustflags !== "unset"
        || value.build.environment.targetRustflags.fingerprint !== "unset"
        || value.build.environment.macosxDeploymentTarget !== "11.0"
        || !value.source.buildStart
        || !value.source.buildEnd) {
        throw new Error("attestation did not record sanitized build inputs");
    }
' "$ATTESTATION"

INVALID_ENV_ATTESTATION="$TEST_ROOT/invalid-environment-attestation.json"
cp "$ATTESTATION" "$INVALID_ENV_ATTESTATION"
node -e '
    const fs = require("node:fs");
    const file = process.argv[1];
    const value = JSON.parse(fs.readFileSync(file, "utf8"));
    value.build.environment.rustflags = "-C link-arg=/private/host/path";
    fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
' "$INVALID_ENV_ATTESTATION"
if node "$HERE/attestation.mjs" verify \
    --attestation "$INVALID_ENV_ATTESTATION" \
    --binary "$ATTESTED_BINARY" \
    --version 0.0.0-test.2 \
    --target aarch64-apple-darwin \
    --git-commit "$GIT_COMMIT" \
    --source-rev "$SOURCE_REV" >/dev/null 2>&1; then
    echo "raw compiler flags unexpectedly passed attestation validation" >&2
    exit 1
fi

ATTESTED_OUTPUT="$TEST_ROOT/output-attested"
ATTESTED_ARCHIVE="$(
    "$DIST" package \
        --target aarch64-apple-darwin \
        --version 0.0.0-test.2 \
        --binary "$ATTESTED_BINARY" \
        --attestation "$ATTESTATION" \
        --output-dir "$ATTESTED_OUTPUT" \
        --source-date-epoch 1700000000 \
        --allow-dirty \
        --allow-unbundled-tools
)"
"$DIST" verify "$ATTESTED_ARCHIVE"

TAMPERED_BINARY="$TEST_ROOT/tampered-grok"
cp "$ATTESTED_BINARY" "$TAMPERED_BINARY"
printf 'tamper' >>"$TAMPERED_BINARY"
if "$DIST" package \
    --target aarch64-apple-darwin \
    --version 0.0.0-test.2 \
    --binary "$TAMPERED_BINARY" \
    --attestation "$ATTESTATION" \
    --output-dir "$TEST_ROOT/tampered-output" \
    --source-date-epoch 1700000000 \
    --allow-dirty \
    --allow-unbundled-tools >/dev/null 2>&1; then
    echo "binary that does not match its attestation unexpectedly packaged" >&2
    exit 1
fi
if "$DIST" package \
    --target x86_64-apple-darwin \
    --version 0.0.0-test.2 \
    --binary "$ATTESTED_BINARY" \
    --attestation "$ATTESTATION" \
    --output-dir "$TEST_ROOT/wrong-target-output" \
    --source-date-epoch 1700000000 \
    --allow-dirty \
    --allow-unbundled-tools >/dev/null 2>&1; then
    echo "wrong-target attestation unexpectedly packaged" >&2
    exit 1
fi

echo "distribution integration tests passed"
