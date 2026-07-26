#!/usr/bin/env bash

set -uo pipefail

export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:/Applications/ChatGPT.app/Contents/Resources"

FORK_REPO="${GROK_DAILY_FORK_REPO:-zhangtyzzz/grok-build}"
UPSTREAM_REPO="${GROK_DAILY_UPSTREAM_REPO:-xai-org/grok-build}"
STATE_DIR="${GROK_DAILY_STATE_DIR:-$HOME/Library/Application Support/grok-build-daily}"
CARGO_TARGET_DIR="${GROK_DAILY_CARGO_TARGET_DIR:-$STATE_DIR/cargo-target}"
CODEX_BIN="${CODEX_BIN:-/Applications/ChatGPT.app/Contents/Resources/codex}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_SCHEMA="$SCRIPT_DIR/daily-upstream-release-output.schema.json"
LAST_RESULT="$STATE_DIR/last-result.json"

if ! mkdir -p "$STATE_DIR" "$CARGO_TARGET_DIR"; then
  printf 'Could not create state directory: %s\n' "$STATE_DIR" >&2
  exit 1
fi
export CARGO_TARGET_DIR

log() {
  printf '%s %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*"
}

notify() {
  /usr/bin/osascript \
    -e 'on run argv' \
    -e 'display notification (item 2 of argv) with title (item 1 of argv)' \
    -e 'end run' \
    -- "$1" "$2" >/dev/null 2>&1 || true
}

fail() {
  log "ERROR: $*"
  notify "grok-build daily sync failed" "$*"
  exit 1
}

command -v gh >/dev/null 2>&1 || fail "gh is unavailable"
command -v git >/dev/null 2>&1 || fail "git is unavailable"
command -v jq >/dev/null 2>&1 || fail "jq is unavailable"
test -x "$CODEX_BIN" || fail "Codex CLI is unavailable at $CODEX_BIN"
test -f "$OUTPUT_SCHEMA" || fail "result schema is missing at $OUTPUT_SCHEMA"

compare_json="$(
  gh api "repos/$FORK_REPO/compare/main...${UPSTREAM_REPO%%/*}:main"
)" || fail "GitHub compare request failed"
ahead_by="$(printf '%s' "$compare_json" | jq -er '.ahead_by')"
case "$ahead_by" in
  0)
    log "No upstream commits; nothing to do"
    exit 0
    ;;
  ''|*[!0-9]*)
    fail "GitHub compare returned an invalid ahead_by value: $ahead_by"
    ;;
esac

log "Upstream is ahead by $ahead_by commit(s); starting Codex maintenance"

run_root="$(mktemp -d "${TMPDIR:-/tmp/}grok-build-daily.XXXXXX")" ||
  fail "Could not create a temporary workspace"
checkout="$run_root/repo"

if ! gh repo clone "$FORK_REPO" "$checkout" -- --quiet; then
  fail "Could not clone $FORK_REPO; workspace preserved at $run_root"
fi
expected_upstream_url="https://github.com/$UPSTREAM_REPO.git"
configured_upstream_url="$(git -C "$checkout" remote get-url upstream 2>/dev/null || true)"
if [ -n "$configured_upstream_url" ]; then
  if [ "$configured_upstream_url" != "$expected_upstream_url" ]; then
    fail "Existing upstream remote points to $configured_upstream_url, expected $expected_upstream_url; workspace preserved at $run_root"
  fi
elif ! git -C "$checkout" remote add upstream "$expected_upstream_url"; then
  fail "Could not configure upstream remote; workspace preserved at $run_root"
fi

rm -f "$LAST_RESULT"

if ! "$CODEX_BIN" \
  --cd "$checkout" \
  --sandbox workspace-write \
  --ask-for-approval never \
  exec \
  --output-schema "$OUTPUT_SCHEMA" \
  --output-last-message "$LAST_RESULT" \
  - <<'PROMPT'
Run the repository owner's scheduled daily upstream synchronization and stable
release maintenance.

Recurring authorization: the owner explicitly authorized this scheduled job to
create and push branches, open ready pull requests, merge them only after their
required CI succeeds, bump the patch release version, dispatch the repository's
release workflow, and approve the expected `release-stable` pending deployment
for the exact release run and commit. This authorization does not extend to
unrelated repositories, environments, releases, or destructive history edits.

Required behavior:

1. Read and follow the repository's `AGENTS.md`, `README.md`, and relevant
   release documentation. Confirm the fresh checkout is clean.
2. Fetch `origin` and `upstream`. Determine the commits on `upstream/main` that
   are not yet contained in `origin/main`. If there are none, make no external
   changes and return `no_change`.
3. Synchronize all current upstream commits through a dedicated branch and a
   ready pull request. Preserve fork-specific behavior, resolve merge conflicts
   carefully, run proportionate local validation, wait for the PR CI with one
   session-managed watcher, and merge only when CI succeeds.
4. From the resulting `origin/main`, increment the lockstepped patch version in
   the four release manifests and their matching workspace lockfile entries.
   Validate it, open a second ready pull request, wait for CI, and merge it.
5. Wait for CI on the exact resulting `main` commit. Recheck `upstream/main`
   immediately before publication; if it advanced, synchronize the additional
   commits through the same guarded PR process before releasing.
6. Do not create or push a tag directly. Dispatch
   `Publish release (warm, tag, Release)` from the exact validated `main`
   commit. Keep the turn alive with one watcher through warmup and the
   dispatched `Release` run.
7. Inspect pending deployments while monitoring. When the exact run waits on
   the expected `release-stable` environment, approve it once under the
   recurring authorization above, then continue the same watcher.
8. Verify the final tag resolves to the intended `main` commit and that the
   stable GitHub Release is published, is not a draft or prerelease, and has
   the complete expected asset set. Use read-only GitHub API or `gh` queries
   for this final remote verification; do not require a post-publication
   `git fetch`, which may be blocked by the non-interactive sandbox.
9. If a conflict cannot be resolved confidently, CI fails, authorization is
   insufficient, or an unexpected environment/repository is involved, do not
   merge or publish. Leave useful remote state when appropriate and return
   `blocked` or `failed` with the exact reason and relevant URLs.

Return only the structured result required by the output schema.
PROMPT
then
  fail "Codex execution failed; workspace preserved at $run_root"
fi

if ! jq -e . "$LAST_RESULT" >/dev/null 2>&1; then
  fail "Codex returned an invalid result; workspace preserved at $run_root"
fi

status="$(jq -r '.status' "$LAST_RESULT")"
summary="$(jq -r '.summary' "$LAST_RESULT")"

case "$status" in
  no_change)
    log "$summary"
    ;;
  success)
    log "$summary"
    notify "grok-build daily release completed" "$summary"
    ;;
  blocked)
    log "BLOCKED: $summary; workspace preserved at $run_root"
    notify "grok-build daily release blocked" \
      "$summary; workspace preserved at $run_root"
    exit 2
    ;;
  failed)
    fail "$summary; workspace preserved at $run_root"
    ;;
  *)
    fail "Codex returned an unknown status: $status"
    ;;
esac

case "$(basename "$run_root")" in
  grok-build-daily.*)
    rm -rf -- "$run_root"
    ;;
  *)
    fail "Refusing to clean unexpected temporary path: $run_root"
    ;;
esac
