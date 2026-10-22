#!/usr/bin/env bash
set -euo pipefail
trap 'echo "Release input archive test failed at line $LINENO" >&2' ERR

helper="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release-inputs.sh"
test_root="$(mktemp -d)"
trap 'rm -rf -- "$test_root"' EXIT
cd "$test_root"
target=fixture-target
cache_dir="target/release-build-cache/$target"
archive_dir="$test_root/archive directory"
mkdir -p "$cache_dir/tools" "$archive_dir/release-inputs"
printf '#!/usr/bin/env bash\nprintf fixture\n' > "$cache_dir/grok"
chmod 0755 "$cache_dir/grok"
printf '{"target":"fixture-target"}\n' > "$cache_dir/grok.build-attestation.json"
printf 'tool payload\n' > "$cache_dir/tools/path with spaces"
cp -R "$cache_dir" expected
native_temp="$archive_dir"
if command -v cygpath >/dev/null 2>&1; then
    native_temp="$(cygpath -w "$archive_dir")"
    [[ "$native_temp" =~ ^[a-zA-Z]: ]]
fi
RUNNER_TEMP="$native_temp" bash "$helper" archive "$target"
mv "$archive_dir/release-inputs-$target.tar.gz" "$archive_dir/release-inputs/"
rm -rf -- "$cache_dir"
RUNNER_TEMP="$native_temp" bash "$helper" extract "$target"
cmp expected/grok "$cache_dir/grok"
cmp expected/grok.build-attestation.json "$cache_dir/grok.build-attestation.json"
cmp "expected/tools/path with spaces" "$cache_dir/tools/path with spaces"
test -x "$cache_dir/grok"
echo 'Release input archive round trip passed, including spaced paths and executable mode'
