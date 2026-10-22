# Release distribution

`scripts/dist.sh` builds, packages, and verifies one target at a time. A release
coordinator or CI matrix runs the script once for each target in
`scripts/dist/targets.json`.

## Build

Distribution builds use both the hardened Cargo profile and the runtime release
feature:

```sh
export GROK_TOOLS_BUNDLE_RG_PATH=path/to/target-compatible/rg
export GROK_TOOLS_BUNDLE_RG_VERSION=15.1.0
export GROK_TOOLS_BUNDLE_BFS_PATH=path/to/target-compatible/bfs
export GROK_TOOLS_BUNDLE_BFS_VERSION=4.1.4
export GROK_TOOLS_BUNDLE_UGREP_PATH=path/to/target-compatible/ugrep
export GROK_TOOLS_BUNDLE_UGREP_VERSION=7.8.2

scripts/dist.sh build \
  --target aarch64-apple-darwin \
  --version 0.2.101
```

The effective Cargo invocation is locked and non-incremental:

```sh
cargo build --locked \
  -p xai-grok-pager-bin \
  --profile release-dist \
  --features release-dist \
  --target TARGET
```

The `release-dist` feature also enables the fork's compile-time privacy
boundary. It cannot be relaxed by environment variables, local configuration,
managed policy, or remote settings. Every release smoke test requires
`grok inspect --json` to contain `"privacyHardened": true`; see
[Privacy hardening](privacy-hardening.md) for the exact blocked and retained
network paths.

The tool paths and explicit version labels are required for release builds so
that a successful build cannot silently omit or mislabel the embedded search
tools. This also works for cross-target binaries that cannot execute on the
build host. The build and package commands also inspect every supplied
Mach-O/ELF/PE executable structure and reject header stubs, libraries/object
files, malformed segment tables, or a tool binary for the wrong platform or
architecture. For local diagnostics only, `--allow-unbundled-tools` relaxes
the presence gate.

On success, `build` writes
`xai-grok-pager[.exe].build-attestation.json` next to the executable. This
receipt binds the artifact and bundled-tool SHA-256 hashes to the target,
version, source revisions, Cargo profile/features, and toolchain. It samples
the Git/SOURCE_REV identity and dirty-state fingerprint both before and after
compilation. Effective `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, and target
Rustflags are recorded only as `unset` or a SHA-256 fingerprint so host paths
and sensitive compiler arguments cannot leak into the package;
`MACOSX_DEPLOYMENT_TARGET` is recorded as a validated version. The receipt is
unsigned build metadata, not a substitute for CI artifact signing.

Linux ARM64 distribution builds override the repository's server-oriented CPU
tuning with `target-cpu=generic`. macOS builds default to deployment target
11.0 unless `MACOSX_DEPLOYMENT_TARGET` is already set. The same deployment
target is applied while compiling the embedded bfs and ugrep binaries so the
tools cannot silently require the release runner's newer macOS version.
Availability warnings are fatal during those source builds, and structural
Mach-O validation rejects the main binary or any bundled tool that declares a
minimum newer than 11.0.

## Package

Package the binary produced by the build:

```sh
scripts/dist.sh package \
  --target aarch64-apple-darwin \
  --version 0.2.101
```

Use `--binary PATH` when build and packaging are separate CI jobs, and pass
its matching receipt with `--attestation PATH`. `--output-dir DIR` chooses the
output directory. The package command verifies the receipt against the binary,
source checkout, version, target-native Mach-O/ELF/PE architecture, and exact
bundled-tool inputs. It refuses a dirty worktree by default. `--allow-dirty`
exists for local diagnostics and is recorded in the manifest. For fake-binary
tests only,
`--allow-unbundled-tools` and `--allow-unattested` relax those gates; such a
manifest is never `releaseReady`.

A packaging-only host does not need Rust when it receives both build outputs:

```sh
scripts/dist.sh package \
  --target TARGET \
  --version VERSION \
  --binary PATH \
  --attestation PATH.build-attestation.json
```

Unix targets produce `.tar.gz`; Windows targets produce `.zip`. Every archive
contains one top-level directory:

```text
grok-build-VERSION-TARGET/
  bin/grok
  LICENSE
  THIRD-PARTY-NOTICES
  BUNDLED-TOOLS-NOTICES.md
  SOURCE_REV
  build-attestation.json
  build-manifest.json
  MANIFEST.sha256
  profiles/starter/
```

`build-manifest.json` records source revisions, build/packaging cleanliness,
toolchain, target, Cargo profile/features, the binary hash, the verified
attestation, bundled-tool hashes, and the starter profile hash. It deliberately
excludes host paths, environment dumps, credentials, and user state.
`MANIFEST.sha256` covers every other file inside the archive.

The package command normalizes payload timestamps using `SOURCE_DATE_EPOCH`
(defaulting to the source commit timestamp), fixes ZIP timestamp encoding to
UTC, and verifies the archive before moving it to its final name. It then
writes an external `SHA256SUMS` covering all archives in the output directory.

## Verify

Verify either a completed archive or an extracted package:

```sh
scripts/dist.sh verify dist/VERSION/grok-build-VERSION-TARGET.tar.gz
scripts/dist.sh verify path/to/extracted/grok-build-VERSION-TARGET
scripts/dist.sh checksums --output-dir dist/VERSION
```

Verification rejects archive path traversal, symlinks, undeclared or missing
payload files, duplicate normalized paths, size, hash, or Unix executable-mode
differences, invalid attestation/profile hashes, missing adjacent external
checksums, inline secret-like values (including `.example` and extensionless
UTF-8 text), private-key markers, user
authentication/session/managed-policy/cache paths, and machine-specific paths
in textual payload files.

## Portable profile

`packaging/profile/starter` is copied into every archive. It contains no
credentials or machine state. Copy it to a writable directory and select it
with `GROK_HOME`:

```sh
cp -R profiles/starter my-grok-profile
export GROK_HOME="$PWD/my-grok-profile"
```

Authentication and sessions are intentionally created independently on each
machine. Shared profile packs must never contain `auth.json`, session data,
managed policy, or generated caches.

The starter profile deliberately contains no opinionated hooks, agents,
skills, or plugins. Maintain personal automation in a separate configuration
repository and install it as a trusted plugin. This keeps release artifacts
portable and prevents one user's reviewer policy, model choice, or host
dependencies from becoming part of the terminal runtime.

## GitHub release channels

`.github/workflows/release.yml` accepts only two tag families:

- `vX.Y.Z` is the stable channel and publishes through the `release-stable`
  GitHub environment.
- `vX.Y.Z-alpha.N`, `vX.Y.Z-beta.N`, and `vX.Y.Z-rc.N` are the testing channel
  and publish as prereleases through the `release-testing` environment.

The tag must exactly match the lockstepped versions in the pager binary,
pager, shell, and version crates. A manual run with `publish=false` is a
packaging dry run. A publishing run is accepted only when the workflow itself
is running on the matching existing tag and commit.

The release workflow calls the same CI workflow used by pull requests while
packaging and testing on six native hosted runners in parallel: macOS, Linux, and Windows
on both ARM64 and x86-64. The fan-in job depends on both the quality workflow
and every platform job, so parallel execution shortens the critical path
without allowing an unverified build to reach publication.
`scripts/dist/prepare-release-tools.sh` downloads a
checksum-pinned protoc and ripgrep for every target and builds the pinned bfs
and ugrep sources on Unix. Windows ARM64 runs protobuf's official win64 protoc
under Windows 11 emulation because protobuf does not publish a Windows ARM64
compiler archive; protoc is a build-host tool and is not shipped in the
product archive.

CI and release jobs restore Cargo registry and Git source caches keyed by the
runner OS, architecture, Rust toolchain, and `Cargo.lock`. Compiler results use
bounded local sccache directories (3 GiB for Rust CI, 4 GiB for terminal CI,
512 MiB for Windows protobuf CI, and 2 GiB per release target). Trusted branch
jobs upload completed compiler objects as Actions artifacts even if a later test
or packaging step fails. Cancelled jobs do not upload. These objects never replace
CI verdicts, and changed source inputs still require compilation.
Public standard-runner Actions usage is free; snapshot uploads are explicitly
disabled for private repositories. These snapshots do not compete with source
and final-binary caches for the 10 GiB dependency-cache pool or the service's
200-small-cache-entry uploads/minute limit.

The snapshot name separates workload, host OS/architecture, Rust and sccache
versions. Restore prefers the newest compatible artifact from the current
branch, then falls back to main; PRs prefer their base branch. Main never consumes
alpha or feature-branch snapshots. The helper verifies the same repository and head
repository, artifact/run source identity, a push/manual event and an allowed
CI/release workflow. PR artifacts, expired snapshots and other workloads are
rejected. A missing snapshot or service failure starts an empty local cache.
Compiler keys still cover source, flags, features and target inputs, so restoring
an older snapshot cannot substitute a different crate's output. Restoring a
compiler snapshot never skips source validation or release verification.

PRs, coordinator quality checks and tag fallback builds do not publish compiler
snapshots. Retention is 30 days; maintenance keeps the newest two snapshots
per workload and release branch. This preserves main-to-alpha reuse without
paid cache capacity, an external storage service or a paid runner.

The Intel macOS warm build cross-compiles on the free Apple Silicon runner.
Its pinned bfs/ugrep builds explicitly target x86_64 and retain the existing
architecture and execution checks. Release still uses a native Intel runner
for Intel packaging and smoke tests. All six distribution targets remain
required; a standalone fallback Release without warm artifacts remains native.

Checksum-pinned protoc and search tools are cached separately by target and the
complete distribution-helper digest. Restores rerun the existing executable
format and version checks before exporting their paths; the current build then
records their hashes in its receipt. This avoids rebuilding unchanged bfs/ugrep
for each application version. A tool-cache hit never substitutes for an
application build. Only branch builds save tool entries.

This remains crate/object reuse, not Cargo's incremental mode:
`CARGO_INCREMENTAL=0` is required by sccache. Final executable linking and some
build scripts still run. The fallback wrapper invokes rustc directly when the
local cache server fails. Source/final-binary caches remain subject to GitHub's
free dependency-cache allowance; compiler snapshots are kept separately as
public workflow artifacts.

Both stable and alpha load `.cargo/release-distribution.toml` through
`scripts/dist.sh`: standard release optimization (`opt-level=3`), LTO off, and
16 codegen units. This shared mode was owner-authorized after the two-mode cache
working set exceeded the free allowance. The `release-dist` feature, privacy
hardening, panic strategy, debug information, portable target flags and all
package checks remain enabled. Removing ThinLTO can affect CPU-bound runtime
performance; startup measurements are not proof of equivalence for every
workload. The generated root `Cargo.toml` is unchanged. `cargo --timings` reports
are uploaded separately from release payloads.

The fork numbers its own releases; the four lockstepped package versions do
not track upstream. `scripts/automation/upstream_sync.py` rewrites them during
every sync to the next patch above `main`'s version, resolving a conflict that
only differs in version lines and overriding any version in an AI resolution,
so tags never collide with upstream's line. `main` publishes stable `X.Y.Z`
versions. The `alpha` branch carries features under test at the next stable
patch with `-alpha.N`; `Release` marks those tags as prereleases in the
`release-testing` environment, and only clients on the alpha update channel
install them. Promotion is an owner-approved PR from `alpha` to `main` that
sets the next stable patch. After each stable release, merge `main` back into
`alpha` and reset it to the following patch's `-alpha.1`, because a stable
`X.Y.Z` sorts above every `X.Y.Z-alpha.N`.

To publish a version, manually dispatch `Publish release (warm, tag, Release)`
from `main` or `alpha` after its required CI checks pass. Ordinary pushes and pull-request
merges never create a tag. The publication workflow checks the four lockstepped
package versions and whether their version tag already exists. For a new
version, it calls `Warm release build cache` with the exact requested main
commit. The six target jobs build that commit and save each final
executable, its build attestation, and the exact checksum-pinned bundled-tool
inputs under a key containing the target, Rust version, `Cargo.lock` hash, and
full commit SHA. Quality validation runs alongside the warm builds. It first
looks for a successful CI run on the exact source SHA, restricted to push/manual runs of
`ci.yml` on main or alpha with a successful current-attempt `CI required` job.
If no such evidence exists, it runs all quality gates. Only after both quality
and every target build succeed does the coordinator recheck the branch head,
create the annotated tag and dispatch `Release`, passing its own run ID.

The coordinator uploads each executable, receipt and exact tool inputs as a
compressed artifact (with executable permissions preserved inside tar). The shared
archive helper normalizes native Windows paths before invoking tar; CI checks an
archive/extract round trip on Unix and Windows. Release
checks that the source run is the expected same-repository coordinator on main
or alpha at the exact tag SHA and that all six builds and the quality gate passed
in its current attempt. Delayed job/artifact visibility is retried for up to
15 minutes; failed gates and source mismatches fail immediately. It downloads
those artifacts by run ID, bypassing the
Actions cache restriction that makes alpha caches invisible to tag runs. Missing,
expired or mismatched handoff artifacts fail the run; they do not silently
substitute artifacts from another revision. Packaging still verifies the receipt,
binary/tool hashes, architecture and version before all native smoke tests,
provenance, asset verification and the channel's publication environment.

A manual `Release` without `build_run_id` remains a recovery/dry-run path: restore
an exact default-branch cache when available, otherwise build normally with the
compiler cache. Quality gates may reuse verified same-SHA CI or execute in full.
To verify performance without publishing, dispatch `Release` on the candidate
branch with its current version and `publish=false`. Compare a cold run and a
second run after the compiler cache has been saved; report both separately.
The target is under 30 minutes for the six-platform build/package path with a
reusable compiler cache, excluding earlier source CI, queue delays and human
environment approval. Cold and warm timings must be reported separately; this
is an acceptance target, not a guarantee until all six runners are measured.

If the tag was created but dispatch or publication subsequently failed, rerunning
the coordinator is a no-op. Inspect the failure, preserve that existing tag,
and manually dispatch `Release` on the same tag with its version and
`publish=true`; never move a tag or publish assets from a different commit.

Callers that already validated a specific commit, including the owner-authorized
upstream-sync workflow, pass the full SHA as the optional `expected_commit`
dispatch input. The coordinator rejects a dispatch whose resolved main SHA is
different, so a concurrent push cannot substitute an unvalidated commit between
the caller's check and workflow startup. Omitting the input preserves the manual
entry point; maintainers must still validate the selected main commit first.

`Prune superseded build caches` runs after successful release-branch workflows.
It removes retired `sccache-disk-v1` archives and keeps only the newest Cargo
source and exact-build snapshot for each platform, toolchain, and branch. It
also keeps the newest two compiler artifacts per workload and release branch.
Legacy `sccache/` objects are no longer written and expire through normal Actions
eviction. Cleanup does not increase the repository's storage limit. The cleanup
job executes only default-branch code and ignores pull-request runs, feature
branches, unknown cache keys, and cache formats it does not own.

`Warm release build cache` remains manually dispatchable from its supported
release branches for recovery and diagnostics. A standalone tag workflow may
restore an exact cache entry from the default branch. Coordinated releases use
the validated artifact handoff instead. Both paths revalidate the cached tools'
formats and versions, then packaging
verifies their hashes against the build attestation together with the binary
hash, source commit, and target before smoke tests and provenance generation. A
miss or cache-service failure falls back to the normal native build. Neither
workflow archives the workspace's potentially very large general-purpose
`target/` directory; only the staged release executable, attestation, and its
exact tool inputs are saved for release reuse.

Cache misses, quota limits, and cache service failures affect performance only,
never the release gates or artifact contents. Native job timeouts retain a
safety margin for cold builds and runner variability; they are not performance
targets. Coordinated alpha tags consume verified build artifacts and do not
repeat their warmup compilation or already successful quality gates.

Each matrix job packages, verifies, extracts, and smoke-tests its real binary,
then creates GitHub artifact provenance. A fan-in job requires the exact six
archive names, verifies every archive and its Sigstore bundle, and creates one
global `SHA256SUMS`. Only the environment-gated publisher can create a GitHub
Release; it uploads to a draft and publishes only after every asset succeeds.
The publisher resolves the tag before draft creation and again immediately
before publication, and aborts if it no longer points at the attested source
commit.

Configure both GitHub environments before the first publication. Stable
releases should require an approving reviewer. Enable immutable releases in
the repository settings so published tags and assets cannot later be changed.
When a release reaches this gate, GitHub reports the publisher job as
`waiting`, with no runner or steps, until a reviewer approves the pending
deployment. Release monitors must inspect pending deployments rather than wait
only for a terminal workflow result. After approval, the same workflow run
continues and the existing watcher can observe publication success or failure.

The pipeline follows GitHub's recommended draft → upload all assets → publish
sequence for
[immutable releases](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases),
uses [deployment environments](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments)
for channel approval, and generates
[artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations).
Actions are pinned to full commit SHAs following GitHub's
[secure-use guidance](https://docs.github.com/en/actions/reference/security/secure-use).
The native archive/checksum layout is also consistent with mature Rust CLI
releases such as [uv](https://github.com/astral-sh/uv/releases) and
[ripgrep](https://github.com/BurntSushi/ripgrep/releases).

Windows currently requires Git to be installed on the system PATH. This fork's
releases do not yet publish the optional grove helper executables or MinGit
sidecars referenced by the upstream installer; missing optional payloads are
best-effort skips. If a distribution provides these assets, installation and
update requests use the locked version's tag rather than mutable `latest`.

## Daily upstream release automation

The owner-authorized `Sync upstream and release validated versions` workflow
runs daily at 15:23 UTC (23:23 Singapore) or by manual dispatch from `main`.
It pins the fork and official upstream commits, attempts a normal merge, opens
a PR, invokes the full reusable CI on its exact candidate, and merges only
after every job succeeds. If the four lockstepped versions have no existing
tag, it dispatches the existing publication coordinator with `expected_commit`.
Ordinary PR merges still do not publish, and stable environment approval still
applies. An active publication blocks synchronization from advancing `main`.

### Subscription configuration

In repository **Settings → Secrets and variables → Actions**, add:

- Secret `LLM_API_KEY`: a key created for your own BigModel GLM Coding Plan.
  Enter it directly in GitHub; never commit it or copy desktop login tokens.
  Create it under **Personal Coding Plan → Overview**, not **Standard API Key**.
  BigModel separates newly created standard and subscription keys; having an
  active subscription does not make a new standard key eligible for its quota.
  A standard key can return code `1113` even while the subscription has quota.
  See the official [key-type notice](https://docs.bigmodel.cn/cn/coding-plan/notice/api-key-management).
- Optional variable `LLM_BASE_URL`: defaults to
  `https://open.bigmodel.cn/api/anthropic` (domestic BigModel subscription).
- Optional variable `LLM_MODEL`: defaults to `glm-5.3-flash`; select a model
  available to your subscription.
- Optional variable `CLAUDE_CODE_VERSION`: an exact compatible npm version
  for the official `@anthropic-ai/claude-code` harness (default `2.1.296`).
  Revalidate the restricted-mode CLI flags when changing this version.

The official Claude Code CLI is an agent harness here; model requests go to
the configured GLM subscription endpoint. No Claude subscription is required
for this route. A clean merge does not invoke a model. Missing credentials or
model/API failures stop conflicts without merging. Confirm the current Coding
Plan terms for your personal automation; do not use the secret as a shared
API service. See the official [Coding Plan integration guide](https://docs.bigmodel.cn/cn/coding-plan/best-practice/claude-code).
ZCode's CLI can run headlessly, but official documentation does not establish
that this use receives its desktop quota promotions. This workflow neither
promises a 1.5× benefit nor emulates a desktop client's identity.

GitHub must allow Actions to create pull requests. Keep default token
permissions read-only; the workflow grants narrowly scoped writes only to its
trusted PR and merge jobs. No personal GitHub token is needed.

Use `Check conflict resolver subscription` for a manual credential/model check.
It runs the same pinned, restricted CLI against a synthetic temporary file,
without checking out repository code or exposing GitHub write credentials.
It validates authentication and a real file edit, not only secret existence.

If dispatch reports `Actions has been disabled for this repository`, open the
repository's Actions page and inspect the fork-disabled notice. GitHub can
disable fork execution independently of the permissions API reporting
`enabled: true`. Re-enable understood workflows there, then trigger validation
again; absent checks must never be counted as passing CI.

### Resolution and validation boundaries

The resolver receives the subscription key but no GitHub token, shell tool,
MCP servers or project hooks/settings. It may edit only the original regular
UTF-8 text conflict files. Its output is data-only JSON, not executable code
or a Git bundle. A separate job independently fetches and reconstructs the
same merge from trusted control code, checks the exact conflict set and
parents, and only then publishes a candidate. CI uses read-only compiler and
Cargo source caches. The privileged merge step executes the helper from
trusted main, checks base/head/tree, then verifies the actual merged tree
before dispatching publication.

Changes to workflows, release tooling, automation helpers, policy documents,
toolchains or credential paths require manual synchronization even when Git
can merge them cleanly. Binary, executable, delete/modify and symlink conflicts
also stop. An existing synchronization branch is never force-pushed: review
and close or finish its PR, then remove that obsolete branch before retrying.
A changed base/upstream or active release requires a fresh run after review.

Reusable CI validates the candidate bytes, but its GitHub check runs belong
to the caller commit. The repository currently has no required branch checks;
if such protection is enabled, automatic merging can be blocked until genuine
candidate-owned checks or a merge queue are implemented. Never manufacture a
successful status to bypass protection. GitHub's merge API has no atomic
expected-base parameter: the workflow checks base immediately before merge
and the resulting tree afterward. Concurrent writes can still cause a merge
that requires manual review; a different tree never starts publication.

### Retiring the older local runner

`scripts/automation/daily-upstream-release.sh` is a historical macOS/Codex
runner with separate authentication, scheduling and old fork-version bump
assumptions. It is not used by the Actions workflow. Inspect any installed
`launchd` jobs or external scheduler before enabling both; retire the old
schedule to avoid duplicate upstream PRs and competing publishers. Preserve
its history for diagnostics rather than silently repurposing it in CI.

## Tests and release gates

The local packaging test uses structurally valid synthetic executable images,
exercises tar and zip, verifies reproducibility for identical inputs and
across host time zones, and proves that executable-header stubs, expanded
inline credential forms, attestation mismatch, payload tampering, duplicate
paths, and checksum downgrade attempts fail:

```sh
scripts/dist/test.sh
```

The GitHub release workflow runs the shared Rust and distribution tests, then
performs a native build/package/verify/smoke cycle for all six targets. A
future signed-distribution phase should add macOS notarization, Windows code
signing, platform dependency inspection, and explicit oldest-supported-glibc
and alternate ARM64 page-size runners. Those checks are not implied by the
current unsigned archive pipeline.
