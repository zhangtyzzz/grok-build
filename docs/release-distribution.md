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
building on six native hosted runners in parallel: macOS, Linux, and Windows
on both ARM64 and x86-64. The fan-in job depends on both the quality workflow
and every native build, so parallel execution shortens the critical path
without allowing an unverified build to reach publication.
`scripts/dist/prepare-release-tools.sh` downloads a
checksum-pinned protoc and ripgrep for every target and builds the pinned bfs
and ugrep sources on Unix. Windows ARM64 runs protobuf's official win64 protoc
under Windows 11 emulation because protobuf does not publish a Windows ARM64
compiler archive; protoc is a build-host tool and is not shipped in the
product archive.

CI and release jobs restore Cargo registry and Git source caches keyed by the
runner OS and architecture, Rust toolchain, and `Cargo.lock`. A lockfile miss
may restore the most recent compatible source cache prefix. CI compilation uses
bounded runner-local sccache directories restored and saved as one cache archive
per workload. Rust, terminal, and Windows protobuf validation are isolated so
one workload cannot evict another's most useful objects. The cross-platform
wrapper retries an individual rustc invocation directly if the background
sccache server cannot start, disconnects, or returns a failure. Incremental
compilation stays disabled so rustc invocations are cacheable.

The cache limits follow the measured working sets: 3 GiB for the Rust gates,
4 GiB for terminal/tools/CLI, and 512 MiB for the small Windows protobuf job.
The two larger jobs run several distinct check, test, and clippy profiles; a
1 GiB shared LRU archive lets each early phase evict the previous run's later
phase and produces effectively no hits. Release warmup does not save a second
compiler cache per target because the exact final executable cache below is
both smaller and able to skip compilation entirely.

After a push to `main` passes the `CI` workflow, `Automatic release` checks the
four lockstepped package versions and whether their version tag already exists.
For a new version, it calls `Warm release build cache` with the exact validated
main commit. The six native jobs build that commit and save only each final
executable and its build attestation under a key containing the target, Rust
version, `Cargo.lock` hash, and full commit SHA. Once every warmup succeeds, the
workflow rechecks that `main` still points to the warmed commit, creates an
annotated version tag, and dispatches the normal `Release` workflow on that
tag. The explicit dispatch is required because GitHub suppresses recursive
workflow triggers for refs created with `GITHUB_TOKEN`. If main advances while
an older run is warming, the older run does not tag; the newer main CI run
performs release detection instead. Commits whose version tag already exists
are successful no-ops.

`Warm release build cache` remains manually dispatchable from `main` for
recovery and diagnostics. The tag workflow restores the exact cache entry from
the default branch and skips Rust compilation on a hit. Packaging still
recreates the checksum-pinned bundled tools and verifies their identities, the
binary hash, source commit, target, and build attestation before smoke tests and
provenance generation. A miss or cache-service failure falls back to the normal
native build. Neither workflow archives the workspace's potentially very large
`target/` directory; only the small staged executable and attestation are saved
for release reuse.

Pull requests may restore default-branch Cargo and compiler caches but never
save compiler entries that trusted runs consume. Trusted main and manual CI
runs save a new immutable bounded compiler cache after successful validation.
The release warmup runs only from `main`; tag builds consume only an exact
commit-keyed release build. Cache misses, quota limits, and cache service
failures affect performance only, never the release gates or artifact contents.

The ARM64 macOS release build and cache warmup have a four-hour timeout because
a cold native build can exceed three hours; the other targets keep their
two-hour limit. All six targets still run in parallel, so this larger safety
margin affects elapsed time only when that cold build is actually the critical
path.

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
