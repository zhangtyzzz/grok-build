#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DIST_HELPERS="$SCRIPT_DIR/dist"
TARGETS_FILE="$DIST_HELPERS/targets.json"
PROFILE_ROOT="$REPO_ROOT/packaging/profile/starter"

usage() {
    cat <<'EOF'
Build and package one Grok Build distribution target at a time.

Usage:
  scripts/dist.sh build \
    --target TARGET --version VERSION [--allow-unbundled-tools]

  scripts/dist.sh package \
    --target TARGET --version VERSION [--binary PATH] [--attestation PATH]
    [--output-dir DIR] [--source-date-epoch EPOCH] [--allow-dirty]
    [--allow-unbundled-tools] [--allow-unattested]

  scripts/dist.sh verify [--archive] PATH
  scripts/dist.sh checksums [--output-dir DIR]

Supported targets are defined in scripts/dist/targets.json.

Release builds require pinned, target-compatible tool binaries:
  GROK_TOOLS_BUNDLE_RG_PATH
  GROK_TOOLS_BUNDLE_RG_VERSION
  GROK_TOOLS_BUNDLE_BFS_PATH   (Unix targets)
  GROK_TOOLS_BUNDLE_BFS_VERSION
  GROK_TOOLS_BUNDLE_UGREP_PATH (Unix targets)
  GROK_TOOLS_BUNDLE_UGREP_VERSION

Use --allow-unbundled-tools only for local diagnostics, never for a published
release. `build` writes a cryptographic sidecar next to the binary; `package`
requires and verifies it unless --allow-unattested is used for diagnostics.
package verifies the completed archive before publishing it atomically.
EOF
}

die() {
    echo "dist: $*" >&2
    exit 1
}

note() {
    echo "dist: $*" >&2
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

validate_version() {
    [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z._-]+)?$ ]] ||
        die "invalid version '$1' (expected semver such as 0.2.101 or 0.2.101-alpha.1)"
}

target_field() {
    node "$DIST_HELPERS/target.mjs" "$1" "$2"
}

validate_target() {
    node "$DIST_HELPERS/target.mjs" "$1" >/dev/null
}

absolute_from_repo() {
    case "$1" in
        /*) printf '%s\n' "$1" ;;
        *) printf '%s\n' "$REPO_ROOT/$1" ;;
    esac
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

sha256_stdin() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 | awk '{print $1}'
    else
        die "sha256sum or shasum is required"
    fi
}

environment_fingerprint() {
    local variable="$1"
    if [[ -z "${!variable+x}" ]]; then
        printf '%s\n' unset
        return
    fi
    local digest
    digest="$(printf '%s' "${!variable}" | sha256_stdin)"
    printf 'sha256:%s\n' "$digest"
}

safe_temp_dir() {
    local base="${TMPDIR:-/tmp}"
    mktemp -d "${base%/}/grok-dist.XXXXXX"
}

safe_remove_temp_dir() {
    local dir="$1"
    local base="${TMPDIR:-/tmp}"
    case "$dir" in
        "${base%/}"/grok-dist.*)
            rm -rf -- "$dir"
            ;;
        *)
            die "refusing to remove unexpected temporary directory: $dir"
            ;;
    esac
}

check_bundle_file() {
    local variable="$1"
    local value="${!variable:-}"
    [[ -n "$value" ]] || return 1
    [[ -f "$value" ]] || die "$variable does not name a regular file: $value"
    return 0
}

check_bundle_version() {
    local variable="$1"
    local value="${!variable:-}"
    [[ -n "$value" ]] || return 1
    [[ "$value" =~ ^[0-9A-Za-z][0-9A-Za-z._+-]*$ ]] ||
        die "$variable contains an invalid version label: $value"
    return 0
}

validate_bundled_tool_artifacts() {
    local target="$1"
    local platform="$2"
    if [[ -n "${GROK_TOOLS_BUNDLE_RG_PATH:-}" &&
        -n "${GROK_TOOLS_BUNDLE_RG_VERSION:-}" ]]; then
        node "$DIST_HELPERS/artifact-format.mjs" \
            "$target" "$GROK_TOOLS_BUNDLE_RG_PATH"
    fi
    if [[ "$platform" != "windows" &&
        -n "${GROK_TOOLS_BUNDLE_BFS_PATH:-}" &&
        -n "${GROK_TOOLS_BUNDLE_BFS_VERSION:-}" ]]; then
        node "$DIST_HELPERS/artifact-format.mjs" \
            "$target" "$GROK_TOOLS_BUNDLE_BFS_PATH"
    fi
    if [[ "$platform" != "windows" &&
        -n "${GROK_TOOLS_BUNDLE_UGREP_PATH:-}" &&
        -n "${GROK_TOOLS_BUNDLE_UGREP_VERSION:-}" ]]; then
        node "$DIST_HELPERS/artifact-format.mjs" \
            "$target" "$GROK_TOOLS_BUNDLE_UGREP_PATH"
    fi
}

build_target() {
    local target=""
    local version=""
    local allow_unbundled=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --target)
                [[ $# -ge 2 ]] || die "--target requires a value"
                target="$2"
                shift 2
                ;;
            --version)
                [[ $# -ge 2 ]] || die "--version requires a value"
                version="$2"
                shift 2
                ;;
            --allow-unbundled-tools)
                allow_unbundled=true
                shift
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                die "unknown build argument: $1"
                ;;
        esac
    done

    [[ -n "$target" ]] || die "build requires --target"
    [[ -n "$version" ]] || die "build requires --version"
    validate_target "$target"
    validate_version "$version"
    require_command cargo
    require_command git
    require_command node

    local platform
    platform="$(target_field "$target" platform)"
    local missing=()
    check_bundle_file GROK_TOOLS_BUNDLE_RG_PATH ||
        missing+=("GROK_TOOLS_BUNDLE_RG_PATH")
    check_bundle_version GROK_TOOLS_BUNDLE_RG_VERSION ||
        missing+=("GROK_TOOLS_BUNDLE_RG_VERSION")
    if [[ "$platform" != "windows" ]]; then
        check_bundle_file GROK_TOOLS_BUNDLE_BFS_PATH ||
            missing+=("GROK_TOOLS_BUNDLE_BFS_PATH")
        check_bundle_version GROK_TOOLS_BUNDLE_BFS_VERSION ||
            missing+=("GROK_TOOLS_BUNDLE_BFS_VERSION")
        check_bundle_file GROK_TOOLS_BUNDLE_UGREP_PATH ||
            missing+=("GROK_TOOLS_BUNDLE_UGREP_PATH")
        check_bundle_version GROK_TOOLS_BUNDLE_UGREP_VERSION ||
            missing+=("GROK_TOOLS_BUNDLE_UGREP_VERSION")
    fi
    if [[ ${#missing[@]} -gt 0 ]]; then
        if [[ "$allow_unbundled" == false ]]; then
            die "missing release tool bundle(s): ${missing[*]}"
        fi
        note "warning: building without release tool bundle(s): ${missing[*]}"
    fi
    validate_bundled_tool_artifacts "$target" "$platform"

    export GROK_VERSION="$version"
    export CARGO_INCREMENTAL=0
    if [[ "$target" == "aarch64-unknown-linux-gnu" ]]; then
        export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS
        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="$(
            target_field "$target" portableRustflags
        )"
    fi
    if [[ "$platform" == "macos" ]]; then
        export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
    fi

    local target_environment_suffix target_rustflags_name
    target_environment_suffix="$(
        printf '%s' "$target" | tr '[:lower:]-' '[:upper:]_'
    )"
    target_rustflags_name="CARGO_TARGET_${target_environment_suffix}_RUSTFLAGS"
    local rustflags_fingerprint encoded_rustflags_fingerprint
    local target_rustflags_fingerprint macosx_deployment_target
    rustflags_fingerprint="$(environment_fingerprint RUSTFLAGS)"
    encoded_rustflags_fingerprint="$(
        environment_fingerprint CARGO_ENCODED_RUSTFLAGS
    )"
    target_rustflags_fingerprint="$(
        environment_fingerprint "$target_rustflags_name"
    )"
    macosx_deployment_target="${MACOSX_DEPLOYMENT_TARGET:-unset}"
    [[ "$macosx_deployment_target" == unset ||
        "$macosx_deployment_target" =~ ^[0-9]+(\.[0-9]+){1,2}$ ]] ||
        die "MACOSX_DEPLOYMENT_TARGET must be a dotted numeric version"

    local git_commit_start source_rev_start status_start
    local dirty_start status_sha256_start
    git_commit_start="$(git -C "$REPO_ROOT" rev-parse HEAD)"
    source_rev_start="$(tr -d '[:space:]' <"$REPO_ROOT/SOURCE_REV")"
    status_start="$(
        git -C "$REPO_ROOT" status --porcelain=v1 --untracked-files=all
    )"
    dirty_start=false
    if [[ -n "$status_start" ]]; then
        dirty_start=true
    fi
    status_sha256_start="$(printf '%s' "$status_start" | sha256_stdin)"

    note "building grok-build $version for $target"
    (
        cd "$REPO_ROOT"
        cargo build \
            --locked \
            -p xai-grok-pager-bin \
            --profile release-dist \
            --features release-dist \
            --target "$target"
    )

    local target_dir="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
    target_dir="$(absolute_from_repo "$target_dir")"
    local cargo_executable
    cargo_executable="$(target_field "$target" cargoExecutable)"
    local artifact="$target_dir/$target/release-dist/$cargo_executable"
    [[ -f "$artifact" ]] || die "cargo succeeded but artifact is missing: $artifact"
    node "$DIST_HELPERS/artifact-format.mjs" "$target" "$artifact"
    local git_commit source_rev dirty source_date_epoch rustc_version cargo_version
    local git_commit_end source_rev_end status_end dirty_end status_sha256_end
    git_commit_end="$(git -C "$REPO_ROOT" rev-parse HEAD)"
    source_rev_end="$(tr -d '[:space:]' <"$REPO_ROOT/SOURCE_REV")"
    status_end="$(
        git -C "$REPO_ROOT" status --porcelain=v1 --untracked-files=all
    )"
    dirty_end=false
    if [[ -n "$status_end" ]]; then
        dirty_end=true
    fi
    status_sha256_end="$(printf '%s' "$status_end" | sha256_stdin)"
    [[ "$git_commit_start" == "$git_commit_end" ]] ||
        die "git HEAD changed while the release artifact was building"
    [[ "$source_rev_start" == "$source_rev_end" ]] ||
        die "SOURCE_REV changed while the release artifact was building"
    git_commit="$git_commit_end"
    source_rev="$source_rev_end"
    dirty=false
    if [[ "$dirty_start" == true || "$dirty_end" == true ]]; then
        dirty=true
    fi
    source_date_epoch="$(git -C "$REPO_ROOT" log -1 --format=%ct)"
    rustc_version="$(rustc --version)"
    cargo_version="$(cargo --version)"
    collect_bundled_tool_args "$platform"
    local attestation_args=(
        create
        --output "$artifact.build-attestation.json"
        --binary "$artifact"
        --version "$version"
        --target "$target"
        --git-commit "$git_commit"
        --source-rev "$source_rev"
        --dirty "$dirty"
        --build-start-git-commit "$git_commit_start"
        --build-start-source-rev "$source_rev_start"
        --build-start-dirty "$dirty_start"
        --build-start-status-sha256 "$status_sha256_start"
        --build-end-git-commit "$git_commit_end"
        --build-end-source-rev "$source_rev_end"
        --build-end-dirty "$dirty_end"
        --build-end-status-sha256 "$status_sha256_end"
        --source-date-epoch "$source_date_epoch"
        --profile release-dist
        --features default,jemalloc,sandbox-enforce,release-dist
        --rustc "$rustc_version"
        --cargo "$cargo_version"
        --rustflags "$rustflags_fingerprint"
        --cargo-encoded-rustflags "$encoded_rustflags_fingerprint"
        --target-rustflags-name "$target_rustflags_name"
        --target-rustflags "$target_rustflags_fingerprint"
        --macosx-deployment-target "$macosx_deployment_target"
    )
    if [[ ${#BUNDLED_TOOL_ARGS[@]} -gt 0 ]]; then
        attestation_args+=("${BUNDLED_TOOL_ARGS[@]}")
    fi
    node "$DIST_HELPERS/attestation.mjs" "${attestation_args[@]}"
    note "wrote $artifact.build-attestation.json"
    printf '%s\n' "$artifact"
}

declare -a BUNDLED_TOOL_ARGS=()

collect_bundled_tool_args() {
    local platform="$1"
    BUNDLED_TOOL_ARGS=()
    if [[ -n "${GROK_TOOLS_BUNDLE_RG_PATH:-}" &&
        -n "${GROK_TOOLS_BUNDLE_RG_VERSION:-}" ]]; then
        BUNDLED_TOOL_ARGS+=(
            --bundled-tool
            "ripgrep,$GROK_TOOLS_BUNDLE_RG_VERSION,$GROK_TOOLS_BUNDLE_RG_PATH"
        )
    fi
    if [[ "$platform" != "windows" &&
        -n "${GROK_TOOLS_BUNDLE_BFS_PATH:-}" &&
        -n "${GROK_TOOLS_BUNDLE_BFS_VERSION:-}" ]]; then
        BUNDLED_TOOL_ARGS+=(
            --bundled-tool
            "bfs,$GROK_TOOLS_BUNDLE_BFS_VERSION,$GROK_TOOLS_BUNDLE_BFS_PATH"
        )
    fi
    if [[ "$platform" != "windows" &&
        -n "${GROK_TOOLS_BUNDLE_UGREP_PATH:-}" &&
        -n "${GROK_TOOLS_BUNDLE_UGREP_VERSION:-}" ]]; then
        BUNDLED_TOOL_ARGS+=(
            --bundled-tool
            "ugrep,$GROK_TOOLS_BUNDLE_UGREP_VERSION,$GROK_TOOLS_BUNDLE_UGREP_PATH"
        )
    fi
}

create_tar_archive() {
    local staging="$1"
    local root_name="$2"
    local output="$3"
    require_command tar
    require_command gzip
    (
        cd "$staging"
        local owner_args=()
        if tar --version 2>&1 | grep -q '^bsdtar '; then
            owner_args=(--uid 0 --gid 0 --uname root --gname root --numeric-owner)
        else
            owner_args=(--owner=0 --group=0 --numeric-owner)
        fi
        find "$root_name" -print |
            LC_ALL=C sort |
            COPYFILE_DISABLE=1 tar \
                --format ustar \
                --no-recursion \
                "${owner_args[@]}" \
                -cf - \
                -T -
    ) | gzip -n -9 >"$output"
}

create_zip_archive() {
    local staging="$1"
    local root_name="$2"
    local output="$3"
    require_command zip
    (
        export TZ=UTC
        cd "$staging"
        find "$root_name" -print | LC_ALL=C sort | zip -X -q "$output" -@
    )
}

package_target() {
    local target=""
    local version=""
    local binary=""
    local attestation=""
    local output_dir=""
    local source_date_epoch="${SOURCE_DATE_EPOCH:-}"
    local allow_dirty=false
    local allow_unbundled=false
    local allow_unattested=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --target)
                [[ $# -ge 2 ]] || die "--target requires a value"
                target="$2"
                shift 2
                ;;
            --version)
                [[ $# -ge 2 ]] || die "--version requires a value"
                version="$2"
                shift 2
                ;;
            --binary)
                [[ $# -ge 2 ]] || die "--binary requires a value"
                binary="$2"
                shift 2
                ;;
            --attestation)
                [[ $# -ge 2 ]] || die "--attestation requires a value"
                attestation="$2"
                shift 2
                ;;
            --output-dir)
                [[ $# -ge 2 ]] || die "--output-dir requires a value"
                output_dir="$2"
                shift 2
                ;;
            --source-date-epoch)
                [[ $# -ge 2 ]] || die "--source-date-epoch requires a value"
                source_date_epoch="$2"
                shift 2
                ;;
            --allow-dirty)
                allow_dirty=true
                shift
                ;;
            --allow-unbundled-tools)
                allow_unbundled=true
                shift
                ;;
            --allow-unattested)
                allow_unattested=true
                shift
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                die "unknown package argument: $1"
                ;;
        esac
    done

    [[ -n "$target" ]] || die "package requires --target"
    [[ -n "$version" ]] || die "package requires --version"
    validate_target "$target"
    validate_version "$version"
    require_command node
    require_command git

    local cargo_executable executable archive_kind platform
    cargo_executable="$(target_field "$target" cargoExecutable)"
    executable="$(target_field "$target" executable)"
    archive_kind="$(target_field "$target" archive)"
    platform="$(target_field "$target" platform)"

    local missing=()
    check_bundle_file GROK_TOOLS_BUNDLE_RG_PATH ||
        missing+=("GROK_TOOLS_BUNDLE_RG_PATH")
    check_bundle_version GROK_TOOLS_BUNDLE_RG_VERSION ||
        missing+=("GROK_TOOLS_BUNDLE_RG_VERSION")
    if [[ "$platform" != "windows" ]]; then
        check_bundle_file GROK_TOOLS_BUNDLE_BFS_PATH ||
            missing+=("GROK_TOOLS_BUNDLE_BFS_PATH")
        check_bundle_version GROK_TOOLS_BUNDLE_BFS_VERSION ||
            missing+=("GROK_TOOLS_BUNDLE_BFS_VERSION")
        check_bundle_file GROK_TOOLS_BUNDLE_UGREP_PATH ||
            missing+=("GROK_TOOLS_BUNDLE_UGREP_PATH")
        check_bundle_version GROK_TOOLS_BUNDLE_UGREP_VERSION ||
            missing+=("GROK_TOOLS_BUNDLE_UGREP_VERSION")
    fi
    if [[ ${#missing[@]} -gt 0 ]]; then
        if [[ "$allow_unbundled" == false ]]; then
            die "missing bundled-tool metadata: ${missing[*]}"
        fi
        note "warning: packaging without bundled-tool metadata: ${missing[*]}"
    fi
    validate_bundled_tool_artifacts "$target" "$platform"

    if [[ -z "$binary" ]]; then
        local target_dir="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
        target_dir="$(absolute_from_repo "$target_dir")"
        binary="$target_dir/$target/release-dist/$cargo_executable"
    else
        binary="$(absolute_from_repo "$binary")"
    fi
    [[ -f "$binary" ]] || die "binary does not exist: $binary"
    if [[ -z "$attestation" ]]; then
        attestation="$binary.build-attestation.json"
    else
        attestation="$(absolute_from_repo "$attestation")"
    fi
    [[ -d "$PROFILE_ROOT" ]] || die "starter profile is missing: $PROFILE_ROOT"

    if [[ -z "$output_dir" ]]; then
        output_dir="$REPO_ROOT/dist/$version"
    else
        output_dir="$(absolute_from_repo "$output_dir")"
    fi
    mkdir -p "$output_dir"

    local dirty=false
    if [[ -n "$(git -C "$REPO_ROOT" status --porcelain --untracked-files=normal)" ]]; then
        dirty=true
    fi
    if [[ "$dirty" == true && "$allow_dirty" == false ]]; then
        die "refusing to package a dirty worktree; commit changes or pass --allow-dirty for diagnostics"
    fi

    if [[ -z "$source_date_epoch" ]]; then
        source_date_epoch="$(git -C "$REPO_ROOT" log -1 --format=%ct)"
    fi
    [[ "$source_date_epoch" =~ ^[0-9]+$ ]] ||
        die "invalid SOURCE_DATE_EPOCH: $source_date_epoch"

    local git_commit source_rev rustc_version cargo_version attestation_verified build_dirty
    git_commit="$(git -C "$REPO_ROOT" rev-parse HEAD)"
    source_rev="$(tr -d '[:space:]' <"$REPO_ROOT/SOURCE_REV")"
    collect_bundled_tool_args "$platform"
    attestation_verified=false
    if [[ -f "$attestation" ]]; then
        local verify_attestation_args=(
            verify
            --attestation "$attestation"
            --binary "$binary"
            --version "$version"
            --target "$target"
            --git-commit "$git_commit"
            --source-rev "$source_rev"
        )
        if [[ ${#BUNDLED_TOOL_ARGS[@]} -gt 0 ]]; then
            verify_attestation_args+=("${BUNDLED_TOOL_ARGS[@]}")
        fi
        node "$DIST_HELPERS/attestation.mjs" \
            "${verify_attestation_args[@]}" >/dev/null
        node "$DIST_HELPERS/artifact-format.mjs" "$target" "$binary"
        attestation_verified=true
        rustc_version="$(
            node "$DIST_HELPERS/attestation.mjs" field "$attestation" build.rustc
        )"
        cargo_version="$(
            node "$DIST_HELPERS/attestation.mjs" field "$attestation" build.cargo
        )"
        build_dirty="$(
            node "$DIST_HELPERS/attestation.mjs" field "$attestation" source.dirty
        )"
        local attested_source_date_epoch
        attested_source_date_epoch="$(
            node "$DIST_HELPERS/attestation.mjs" field \
                "$attestation" build.sourceDateEpoch
        )"
        [[ "$source_date_epoch" == "$attested_source_date_epoch" ]] ||
            die "SOURCE_DATE_EPOCH $source_date_epoch does not match build attestation $attested_source_date_epoch"
    elif [[ "$allow_unattested" == true ]]; then
        note "warning: packaging an unattested diagnostic binary"
        build_dirty=true
        if [[ -n "${GROK_DIST_RUSTC_VERSION:-}" ]]; then
            rustc_version="$GROK_DIST_RUSTC_VERSION"
        elif command -v rustc >/dev/null 2>&1; then
            rustc_version="$(rustc --version)"
        else
            die "rustc is unavailable; set GROK_DIST_RUSTC_VERSION"
        fi
        if [[ -n "${GROK_DIST_CARGO_VERSION:-}" ]]; then
            cargo_version="$GROK_DIST_CARGO_VERSION"
        elif command -v cargo >/dev/null 2>&1; then
            cargo_version="$(cargo --version)"
        else
            die "cargo is unavailable; set GROK_DIST_CARGO_VERSION"
        fi
    else
        die "build attestation is missing: $attestation (use --allow-unattested only for diagnostics)"
    fi

    local staging
    staging="$(safe_temp_dir)"
    DIST_ACTIVE_TEMP="$staging"
    trap cleanup_active_temp EXIT

    local root_name="grok-build-$version-$target"
    local staged_root="$staging/$root_name"
    mkdir -p "$staged_root/bin" "$staged_root/profiles"
    cp "$binary" "$staged_root/bin/$executable"
    chmod 0755 "$staged_root/bin/$executable"
    cp "$REPO_ROOT/LICENSE" "$staged_root/LICENSE"
    cp "$REPO_ROOT/THIRD-PARTY-NOTICES" "$staged_root/THIRD-PARTY-NOTICES"
    cp "$REPO_ROOT/crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md" \
        "$staged_root/BUNDLED-TOOLS-NOTICES.md"
    cp "$REPO_ROOT/SOURCE_REV" "$staged_root/SOURCE_REV"
    if [[ "$attestation_verified" == true ]]; then
        cp "$attestation" "$staged_root/build-attestation.json"
    fi
    cp -R "$PROFILE_ROOT" "$staged_root/profiles/starter"

    local manifest_args=(
        --stage-dir "$staged_root"
        --version "$version"
        --target "$target"
        --git-commit "$git_commit"
        --source-rev "$source_rev"
        --dirty "$dirty"
        --build-dirty "$build_dirty"
        --source-date-epoch "$source_date_epoch"
        --profile release-dist
        --features default,jemalloc,sandbox-enforce,release-dist
        --rustc "$rustc_version"
        --cargo "$cargo_version"
        --executable "$executable"
        --attestation-verified "$attestation_verified"
    )
    if [[ ${#BUNDLED_TOOL_ARGS[@]} -gt 0 ]]; then
        manifest_args+=("${BUNDLED_TOOL_ARGS[@]}")
    fi
    node "$DIST_HELPERS/manifest.mjs" "${manifest_args[@]}"
    node "$DIST_HELPERS/normalize-times.mjs" "$staged_root" "$source_date_epoch"

    local archive_name="$root_name.$archive_kind"
    local final_archive="$output_dir/$archive_name"
    local temporary_archive="$output_dir/.$archive_name.tmp.$$.$archive_kind"
    DIST_ACTIVE_ARCHIVE="$temporary_archive"
    case "$archive_kind" in
        tar.gz)
            create_tar_archive "$staging" "$root_name" "$temporary_archive"
            ;;
        zip)
            create_zip_archive "$staging" "$root_name" "$temporary_archive"
            ;;
        *)
            die "unsupported archive kind from $TARGETS_FILE: $archive_kind"
            ;;
    esac

    verify_path "$temporary_archive" false >&2
    mv -f "$temporary_archive" "$final_archive"
    DIST_ACTIVE_ARCHIVE=""
    write_checksums "$output_dir"
    verify_external_checksum_if_present "$final_archive"
    safe_remove_temp_dir "$staging"
    DIST_ACTIVE_TEMP=""
    trap - EXIT
    note "created $final_archive"
    printf '%s\n' "$final_archive"
}

DIST_ACTIVE_TEMP=""
DIST_ACTIVE_ARCHIVE=""

cleanup_active_temp() {
    if [[ -n "$DIST_ACTIVE_TEMP" && -d "$DIST_ACTIVE_TEMP" ]]; then
        safe_remove_temp_dir "$DIST_ACTIVE_TEMP"
    fi
    if [[ -n "$DIST_ACTIVE_ARCHIVE" && -f "$DIST_ACTIVE_ARCHIVE" ]]; then
        case "$(basename "$DIST_ACTIVE_ARCHIVE")" in
            .grok-build-*.tmp.*.tar.gz|.grok-build-*.tmp.*.zip)
                rm -f -- "$DIST_ACTIVE_ARCHIVE"
                ;;
            *)
                die "refusing to remove unexpected temporary archive: $DIST_ACTIVE_ARCHIVE"
                ;;
        esac
    fi
}

verify_extracted_root() {
    local extracted="$1"
    (
        shopt -s nullglob dotglob
        local entries=("$extracted"/*)
        [[ ${#entries[@]} -eq 1 ]] ||
            die "archive must contain exactly one top-level directory"
        [[ -d "${entries[0]}" ]] ||
            die "archive top-level entry must be a directory"
        node "$DIST_HELPERS/verify.mjs" "${entries[0]}"
    )
}

verify_external_checksum_if_present() {
    local archive="$1"
    local checksum_file
    checksum_file="$(dirname "$archive")/SHA256SUMS"
    [[ -f "$checksum_file" ]] || return 0

    local name
    name="$(basename "$archive")"
    local matches
    matches="$(awk -v name="$name" '$2 == name { print $1 }' "$checksum_file")"
    [[ -n "$matches" ]] ||
        die "SHA256SUMS is present but has no entry for $name"
    [[ "$(printf '%s\n' "$matches" | wc -l | tr -d '[:space:]')" == "1" ]] ||
        die "SHA256SUMS contains duplicate entries for $name"
    [[ "$matches" =~ ^[0-9a-f]{64}$ ]] ||
        die "SHA256SUMS contains an invalid digest for $name"
    [[ "$(sha256_file "$archive")" == "$matches" ]] ||
        die "external SHA256SUMS mismatch for $name"
}

verify_path() {
    local input="$1"
    local check_external_checksum="${2:-true}"
    [[ -e "$input" ]] || die "verification input does not exist: $input"
    require_command node

    if [[ -d "$input" ]]; then
        node "$DIST_HELPERS/verify.mjs" "$input"
        return
    fi

    if [[ "$check_external_checksum" == true ]]; then
        verify_external_checksum_if_present "$input"
    fi
    local extraction
    extraction="$(safe_temp_dir)"
    case "$input" in
        *.tar.gz|*.tar.gz.tmp.*|*.tmp.*.tar.gz)
            require_command tar
            tar -tzf "$input" | node "$DIST_HELPERS/validate-archive-paths.mjs"
            if ! tar -tvzf "$input" | awk '
                substr($1, 1, 1) == "l" || substr($1, 1, 1) == "h" { bad = 1 }
                END { exit bad }
            '; then
                safe_remove_temp_dir "$extraction"
                die "archive contains a link entry"
            fi
            tar -xzf "$input" -C "$extraction"
            ;;
        *.zip|*.zip.tmp.*|*.tmp.*.zip)
            require_command unzip
            unzip -Z1 "$input" | node "$DIST_HELPERS/validate-archive-paths.mjs"
            if ! unzip -Z -l "$input" | awk '
                substr($1, 1, 1) == "l" { bad = 1 }
                END { exit bad }
            '; then
                safe_remove_temp_dir "$extraction"
                die "archive contains a symbolic link entry"
            fi
            unzip -q "$input" -d "$extraction"
            ;;
        *)
            safe_remove_temp_dir "$extraction"
            die "verify expects a staged directory, .tar.gz, or .zip"
            ;;
    esac
    verify_extracted_root "$extraction"
    safe_remove_temp_dir "$extraction"
}

verify_command() {
    local input=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --archive|--path)
                [[ $# -ge 2 ]] || die "$1 requires a value"
                input="$2"
                shift 2
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                [[ -z "$input" ]] || die "verify accepts one path"
                input="$1"
                shift
                ;;
        esac
    done
    [[ -n "$input" ]] || die "verify requires a path"
    verify_path "$(absolute_from_repo "$input")"
}

write_checksums() {
    local output_dir="$1"
    [[ -d "$output_dir" ]] || die "checksum directory does not exist: $output_dir"
    (
        shopt -s nullglob
        local files=(
            "$output_dir"/*.tar.gz
            "$output_dir"/*.zip
            "$output_dir"/grok-[0-9]*
        )
        [[ -f "$output_dir/install.sh" ]] && files+=("$output_dir/install.sh")
        [[ -f "$output_dir/install.ps1" ]] && files+=("$output_dir/install.ps1")
        [[ ${#files[@]} -gt 0 ]] || die "no distribution archives found in $output_dir"
        local temporary="$output_dir/.SHA256SUMS.tmp.$$"
        : >"$temporary"
        local file
        for file in "${files[@]}"; do
            printf '%s  %s\n' "$(sha256_file "$file")" "$(basename "$file")" >>"$temporary"
        done
        LC_ALL=C sort -o "$temporary" "$temporary"
        mv -f "$temporary" "$output_dir/SHA256SUMS"
        note "wrote $output_dir/SHA256SUMS"
    )
}

checksums_command() {
    local output_dir=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --output-dir)
                [[ $# -ge 2 ]] || die "--output-dir requires a value"
                output_dir="$2"
                shift 2
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                [[ -z "$output_dir" ]] || die "checksums accepts one directory"
                output_dir="$1"
                shift
                ;;
        esac
    done
    [[ -n "$output_dir" ]] || die "checksums requires --output-dir DIR"
    write_checksums "$(absolute_from_repo "$output_dir")"
}

main() {
    [[ $# -gt 0 ]] || {
        usage
        exit 2
    }
    local command="$1"
    shift
    case "$command" in
        build) build_target "$@" ;;
        package) package_target "$@" ;;
        verify) verify_command "$@" ;;
        checksums) checksums_command "$@" ;;
        -h|--help|help)
            usage
            ;;
        *)
            die "unknown command: $command"
            ;;
    esac
}

main "$@"
