#!/usr/bin/env bash
set -euo pipefail

mode="${1:?expected archive or extract}"
target="${2:?expected distribution target}"
[[ "$target" =~ ^[a-zA-Z0-9_-]+$ ]] || exit 2
archive_dir="${RUNNER_TEMP:?RUNNER_TEMP is required}"
if command -v cygpath >/dev/null 2>&1; then
    # GNU tar interprets a native Windows drive prefix as a remote host.
    archive_dir="$(cygpath -u "$archive_dir")"
fi
cache_dir="target/release-build-cache/$target"
case "$mode" in
    archive)
        tar -czf "$archive_dir/release-inputs-$target.tar.gz" -C "$cache_dir" .
        ;;
    extract)
        mkdir -p "$cache_dir"
        tar -xzf "$archive_dir/release-inputs/release-inputs-$target.tar.gz" -C "$cache_dir"
        ;;
    *) exit 2 ;;
esac
