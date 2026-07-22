#!/usr/bin/env bash

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="$HERE/tool-bundles.json"

usage() {
    cat <<'EOF'
Prepare checksum-pinned protoc and target-native search tools for a release build.

Usage:
  prepare-release-tools.sh --target TARGET --output-dir DIR [--github-env FILE]
                           [--protoc-only] [--reuse-existing]
EOF
}

die() {
    echo "prepare-release-tools: $*" >&2
    exit 1
}

json_field() {
    node -e '
        const manifest = require(process.argv[1]);
        let value = manifest;
        for (const part of process.argv[2].split(".")) value = value?.[part];
        if (typeof value !== "string") process.exit(2);
        process.stdout.write(value);
    ' "$MANIFEST" "$1"
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        die "sha256sum or shasum is required"
    fi
}

verify_sha256() {
    local file="$1"
    local expected="$2"
    local actual
    actual="$(sha256_file "$file")"
    [[ "$actual" == "$expected" ]] ||
        die "checksum mismatch for $(basename "$file"): expected $expected, got $actual"
}

extract_zip() {
    local archive="$1"
    local destination="$2"
    mkdir -p "$destination"
    if command -v unzip >/dev/null 2>&1; then
        unzip -q "$archive" -d "$destination"
    else
        tar -xf "$archive" -C "$destination"
    fi
}

append_env() {
    local name="$1"
    local value="$2"
    [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] ||
        die "environment value contains a newline"
    if [[ -n "$GITHUB_ENV_FILE" ]]; then
        printf '%s=%s\n' "$name" "$value" >>"$GITHUB_ENV_FILE"
    else
        printf 'export %s=%q\n' "$name" "$value"
    fi
}

append_path_env() {
    local name="$1"
    local value="$2"
    if [[ "$TARGET" == *-windows-msvc ]] && command -v cygpath >/dev/null 2>&1; then
        value="$(cygpath -w "$value")"
    fi
    append_env "$name" "$value"
}

TARGET=""
OUTPUT_DIR=""
GITHUB_ENV_FILE=""
PROTOC_ONLY=false
REUSE_EXISTING=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)
            [[ $# -ge 2 ]] || die "--target requires a value"
            TARGET="$2"
            shift 2
            ;;
        --output-dir)
            [[ $# -ge 2 ]] || die "--output-dir requires a value"
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --github-env)
            [[ $# -ge 2 ]] || die "--github-env requires a value"
            GITHUB_ENV_FILE="$2"
            shift 2
            ;;
        --protoc-only)
            PROTOC_ONLY=true
            shift
            ;;
        --reuse-existing)
            REUSE_EXISTING=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1"
            ;;
    esac
done

[[ -n "$TARGET" ]] || die "--target is required"
[[ -n "$OUTPUT_DIR" ]] || die "--output-dir is required"
node "$HERE/target.mjs" "$TARGET" >/dev/null
command -v curl >/dev/null 2>&1 || die "curl is required"
command -v node >/dev/null 2>&1 || die "node is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

if [[ "$TARGET" == *-apple-darwin ]]; then
    export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
    [[ "$MACOSX_DEPLOYMENT_TARGET" =~ ^[0-9]+(\.[0-9]+){1,2}$ ]] ||
        die "MACOSX_DEPLOYMENT_TARGET must be a dotted numeric version"
    export CPPFLAGS="${CPPFLAGS:+$CPPFLAGS }-Werror=unguarded-availability-new"
fi

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

if [[ "$REUSE_EXISTING" == true ]]; then
    PROTOC_VERSION="$(json_field protoc.version)"
    PROTOC_NAME="protoc"
    RG_NAME="rg"
    if [[ "$TARGET" == *-windows-msvc ]]; then
        PROTOC_NAME="protoc.exe"
        RG_NAME="rg.exe"
    fi
    PROTOC_OUTPUT="$OUTPUT_DIR/protoc/bin/$PROTOC_NAME"
    [[ -x "$PROTOC_OUTPUT" ]] || die "cached $PROTOC_NAME is missing or not executable"
    [[ -d "$OUTPUT_DIR/protoc/include" ]] || die "cached protobuf includes are missing"
    PROTOC_ACTUAL_VERSION="$("$PROTOC_OUTPUT" --version)"
    PROTOC_ACTUAL_VERSION="${PROTOC_ACTUAL_VERSION%$'\r'}"
    [[ "$PROTOC_ACTUAL_VERSION" == "libprotoc $PROTOC_VERSION" ]] ||
        die "unexpected cached protoc version: $PROTOC_ACTUAL_VERSION"
    append_path_env PROTOC "$PROTOC_OUTPUT"

    if [[ "$PROTOC_ONLY" == true ]]; then
        printf '%s\n' "$PROTOC_ACTUAL_VERSION"
        exit 0
    fi

    RG_VERSION="$(json_field ripgrep.version)"
    RG_OUTPUT="$OUTPUT_DIR/$RG_NAME"
    [[ -x "$RG_OUTPUT" ]] || die "cached $RG_NAME is missing or not executable"
    node "$HERE/artifact-format.mjs" "$TARGET" "$RG_OUTPUT"
    append_path_env GROK_TOOLS_BUNDLE_RG_PATH "$RG_OUTPUT"
    append_env GROK_TOOLS_BUNDLE_RG_VERSION "$RG_VERSION"

    RG_ACTUAL_VERSION="$("$RG_OUTPUT" --version)"
    RG_VERSION_LINE="${RG_ACTUAL_VERSION%%$'\n'*}"
    RG_VERSION_LINE="${RG_VERSION_LINE%$'\r'}"
    [[ "$RG_VERSION_LINE" == "ripgrep $RG_VERSION"* ]] ||
        die "unexpected cached ripgrep version: $RG_VERSION_LINE"

    printf '%s\n' "$PROTOC_ACTUAL_VERSION"
    printf '%s\n' "$RG_ACTUAL_VERSION"
    if [[ "$TARGET" != *-windows-msvc ]]; then
        BFS_VERSION="$(json_field bfs.version)"
        BFS_OUTPUT="$OUTPUT_DIR/bfs"
        [[ -x "$BFS_OUTPUT" ]] || die "cached bfs is missing or not executable"
        node "$HERE/artifact-format.mjs" "$TARGET" "$BFS_OUTPUT"
        append_path_env GROK_TOOLS_BUNDLE_BFS_PATH "$BFS_OUTPUT"
        append_env GROK_TOOLS_BUNDLE_BFS_VERSION "$BFS_VERSION"

        UGREP_VERSION="$(json_field ugrep.version)"
        UGREP_OUTPUT="$OUTPUT_DIR/ugrep"
        [[ -x "$UGREP_OUTPUT" ]] || die "cached ugrep is missing or not executable"
        node "$HERE/artifact-format.mjs" "$TARGET" "$UGREP_OUTPUT"
        append_path_env GROK_TOOLS_BUNDLE_UGREP_PATH "$UGREP_OUTPUT"
        append_env GROK_TOOLS_BUNDLE_UGREP_VERSION "$UGREP_VERSION"

        BFS_ACTUAL_VERSION="$("$BFS_OUTPUT" --version)"
        BFS_VERSION_LINE="${BFS_ACTUAL_VERSION%%$'\n'*}"
        BFS_VERSION_LINE="${BFS_VERSION_LINE%$'\r'}"
        [[ "$BFS_VERSION_LINE" == "bfs $BFS_VERSION" ]] ||
            die "unexpected cached bfs version: $BFS_VERSION_LINE"

        UGREP_ACTUAL_VERSION="$("$UGREP_OUTPUT" --version)"
        UGREP_VERSION_LINE="${UGREP_ACTUAL_VERSION%%$'\n'*}"
        UGREP_VERSION_LINE="${UGREP_VERSION_LINE%$'\r'}"
        [[ "$UGREP_VERSION_LINE" == "ugrep $UGREP_VERSION "* ]] ||
            die "unexpected cached ugrep version: $UGREP_VERSION_LINE"

        printf '%s\n' "$BFS_ACTUAL_VERSION"
        printf '%s\n' "$UGREP_ACTUAL_VERSION"
    fi
    exit 0
fi

TEMP_ROOT="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
if command -v cygpath >/dev/null 2>&1 && [[ "$TEMP_ROOT" =~ ^[A-Za-z]:[\\/].* ]]; then
    TEMP_ROOT="$(cygpath -u "$TEMP_ROOT")"
fi
TEMP_ROOT="${TEMP_ROOT%/}"
[[ -n "$TEMP_ROOT" ]] || TEMP_ROOT="/"
WORK_DIR="$(mktemp -d "$TEMP_ROOT/grok-release-tools.XXXXXX")"
cleanup() {
    case "$WORK_DIR" in
        "$TEMP_ROOT"/grok-release-tools.*)
            rm -rf -- "$WORK_DIR"
            ;;
        *)
            echo "prepare-release-tools: refusing to remove $WORK_DIR" >&2
            ;;
    esac
}
trap cleanup EXIT

PROTOC_VERSION="$(json_field protoc.version)"
PROTOC_ARCHIVE="$(json_field "protoc.assets.$TARGET.archive")"
PROTOC_SHA256="$(json_field "protoc.assets.$TARGET.sha256")"
PROTOC_URL="https://github.com/protocolbuffers/protobuf/releases/download/v$PROTOC_VERSION/$PROTOC_ARCHIVE"
curl --fail --location --proto '=https' --tlsv1.2 \
    --retry 5 --retry-all-errors --output "$WORK_DIR/$PROTOC_ARCHIVE" "$PROTOC_URL"
verify_sha256 "$WORK_DIR/$PROTOC_ARCHIVE" "$PROTOC_SHA256"
extract_zip "$WORK_DIR/$PROTOC_ARCHIVE" "$WORK_DIR/protoc"
PROTOC_NAME="protoc"
if [[ "$TARGET" == *-windows-msvc ]]; then
    PROTOC_NAME="protoc.exe"
fi
PROTOC_SOURCE="$WORK_DIR/protoc/bin/$PROTOC_NAME"
[[ -f "$PROTOC_SOURCE" ]] || die "$PROTOC_NAME was not found in $PROTOC_ARCHIVE"
[[ -d "$WORK_DIR/protoc/include" ]] || die "protobuf includes were not found in $PROTOC_ARCHIVE"
PROTOC_ROOT="$OUTPUT_DIR/protoc"
[[ ! -e "$PROTOC_ROOT" ]] || die "protoc output already exists: $PROTOC_ROOT"
mkdir -p "$PROTOC_ROOT/bin" "$PROTOC_ROOT/include"
cp "$PROTOC_SOURCE" "$PROTOC_ROOT/bin/$PROTOC_NAME"
cp -R "$WORK_DIR/protoc/include/." "$PROTOC_ROOT/include/"
PROTOC_OUTPUT="$PROTOC_ROOT/bin/$PROTOC_NAME"
chmod 0755 "$PROTOC_OUTPUT" 2>/dev/null || true
PROTOC_ACTUAL_VERSION="$("$PROTOC_OUTPUT" --version)"
PROTOC_ACTUAL_VERSION="${PROTOC_ACTUAL_VERSION%$'\r'}"
[[ "$PROTOC_ACTUAL_VERSION" == "libprotoc $PROTOC_VERSION" ]] ||
    die "unexpected protoc version: $PROTOC_ACTUAL_VERSION"
append_path_env PROTOC "$PROTOC_OUTPUT"

if [[ "$PROTOC_ONLY" == true ]]; then
    printf '%s\n' "$PROTOC_ACTUAL_VERSION"
    exit 0
fi

RG_VERSION="$(json_field ripgrep.version)"
RG_ARCHIVE="$(json_field "ripgrep.assets.$TARGET.archive")"
RG_SHA256="$(json_field "ripgrep.assets.$TARGET.sha256")"
RG_URL="https://github.com/BurntSushi/ripgrep/releases/download/$RG_VERSION/$RG_ARCHIVE"
curl --fail --location --proto '=https' --tlsv1.2 \
    --retry 5 --retry-all-errors --output "$WORK_DIR/$RG_ARCHIVE" "$RG_URL"
verify_sha256 "$WORK_DIR/$RG_ARCHIVE" "$RG_SHA256"
mkdir -p "$WORK_DIR/rg"
if [[ "$RG_ARCHIVE" == *.zip ]]; then
    extract_zip "$WORK_DIR/$RG_ARCHIVE" "$WORK_DIR/rg"
else
    tar -xf "$WORK_DIR/$RG_ARCHIVE" -C "$WORK_DIR/rg"
fi
RG_NAME="rg"
if [[ "$TARGET" == *-windows-msvc ]]; then
    RG_NAME="rg.exe"
fi
RG_SOURCE="$(find "$WORK_DIR/rg" -type f -name "$RG_NAME" -print -quit)"
[[ -n "$RG_SOURCE" ]] || die "$RG_NAME was not found in $RG_ARCHIVE"
RG_OUTPUT="$OUTPUT_DIR/$RG_NAME"
cp "$RG_SOURCE" "$RG_OUTPUT"
chmod 0755 "$RG_OUTPUT" 2>/dev/null || true
node "$HERE/artifact-format.mjs" "$TARGET" "$RG_OUTPUT"
append_path_env GROK_TOOLS_BUNDLE_RG_PATH "$RG_OUTPUT"
append_env GROK_TOOLS_BUNDLE_RG_VERSION "$RG_VERSION"

if [[ "$TARGET" != *-windows-msvc ]]; then
    command -v make >/dev/null 2>&1 || die "make is required for bfs/ugrep"

    BFS_VERSION="$(json_field bfs.version)"
    BFS_URL="$(json_field bfs.source)"
    BFS_SHA256="$(json_field bfs.sha256)"
    curl --fail --location --proto '=https' --tlsv1.2 \
        --retry 5 --retry-all-errors --output "$WORK_DIR/bfs.tar.gz" "$BFS_URL"
    verify_sha256 "$WORK_DIR/bfs.tar.gz" "$BFS_SHA256"
    mkdir -p "$WORK_DIR/bfs-source"
    tar -xzf "$WORK_DIR/bfs.tar.gz" -C "$WORK_DIR/bfs-source" --strip-components=1
    (
        cd "$WORK_DIR/bfs-source"
        PKG_CONFIG=false ./configure --enable-release --without-oniguruma
        make -j"${MAKE_JOBS:-2}"
    )
    BFS_SOURCE="$WORK_DIR/bfs-source/bin/bfs"
    [[ -x "$BFS_SOURCE" ]] || die "bfs build did not produce bin/bfs"
    BFS_OUTPUT="$OUTPUT_DIR/bfs"
    cp "$BFS_SOURCE" "$BFS_OUTPUT"
    chmod 0755 "$BFS_OUTPUT"
    node "$HERE/artifact-format.mjs" "$TARGET" "$BFS_OUTPUT"
    append_path_env GROK_TOOLS_BUNDLE_BFS_PATH "$BFS_OUTPUT"
    append_env GROK_TOOLS_BUNDLE_BFS_VERSION "$BFS_VERSION"

    UGREP_VERSION="$(json_field ugrep.version)"
    UGREP_URL="$(json_field ugrep.source)"
    UGREP_SHA256="$(json_field ugrep.sha256)"
    curl --fail --location --proto '=https' --tlsv1.2 \
        --retry 5 --retry-all-errors --output "$WORK_DIR/ugrep.tar.gz" "$UGREP_URL"
    verify_sha256 "$WORK_DIR/ugrep.tar.gz" "$UGREP_SHA256"
    mkdir -p "$WORK_DIR/ugrep-source"
    tar -xzf "$WORK_DIR/ugrep.tar.gz" -C "$WORK_DIR/ugrep-source" --strip-components=1
    (
        cd "$WORK_DIR/ugrep-source"
        ./configure \
            --enable-color \
            --disable-dependency-tracking \
            --disable-silent-rules \
            --without-brotli \
            --without-lz4 \
            --without-lzma \
            --without-pcre2 \
            --without-zlib \
            --without-zstd
        make -j"${MAKE_JOBS:-2}"
    )
    UGREP_SOURCE="$WORK_DIR/ugrep-source/src/ugrep"
    [[ -x "$UGREP_SOURCE" ]] || die "ugrep build did not produce src/ugrep"
    UGREP_OUTPUT="$OUTPUT_DIR/ugrep"
    cp "$UGREP_SOURCE" "$UGREP_OUTPUT"
    chmod 0755 "$UGREP_OUTPUT"
    node "$HERE/artifact-format.mjs" "$TARGET" "$UGREP_OUTPUT"
    append_path_env GROK_TOOLS_BUNDLE_UGREP_PATH "$UGREP_OUTPUT"
    append_env GROK_TOOLS_BUNDLE_UGREP_VERSION "$UGREP_VERSION"
fi

RG_ACTUAL_VERSION="$("$RG_OUTPUT" --version)"
RG_VERSION_LINE="${RG_ACTUAL_VERSION%%$'\n'*}"
RG_VERSION_LINE="${RG_VERSION_LINE%$'\r'}"
[[ "$RG_VERSION_LINE" == "ripgrep $RG_VERSION"* ]] ||
    die "unexpected ripgrep version: $RG_VERSION_LINE"

printf '%s\n' "$PROTOC_ACTUAL_VERSION"
printf '%s\n' "$RG_ACTUAL_VERSION"
if [[ "$TARGET" != *-windows-msvc ]]; then
    BFS_ACTUAL_VERSION="$("$BFS_OUTPUT" --version)"
    BFS_VERSION_LINE="${BFS_ACTUAL_VERSION%%$'\n'*}"
    BFS_VERSION_LINE="${BFS_VERSION_LINE%$'\r'}"
    [[ "$BFS_VERSION_LINE" == "bfs $BFS_VERSION" ]] ||
        die "unexpected bfs version: $BFS_VERSION_LINE"

    UGREP_ACTUAL_VERSION="$("$UGREP_OUTPUT" --version)"
    UGREP_VERSION_LINE="${UGREP_ACTUAL_VERSION%%$'\n'*}"
    UGREP_VERSION_LINE="${UGREP_VERSION_LINE%$'\r'}"
    [[ "$UGREP_VERSION_LINE" == "ugrep $UGREP_VERSION "* ]] ||
        die "unexpected ugrep version: $UGREP_VERSION_LINE"

    printf '%s\n' "$BFS_ACTUAL_VERSION"
    printf '%s\n' "$UGREP_ACTUAL_VERSION"
fi
