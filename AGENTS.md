# Repository instructions

These instructions apply to the entire repository. Add a more specific
`AGENTS.md` in a subdirectory only when that subtree genuinely needs different
rules.

## Start here

- Read `README.md` for build and product orientation.
- Read `docs/ARCHITECTURE.md` before changing crate boundaries or runtime flow.
- Check `git status --short --branch` before editing and preserve unrelated
  user changes.
- Use `rg` and `rg --files` for repository searches.

## Repository constraints

- This public tree is periodically synced from the SpaceXAI monorepo. Keep
  changes self-contained and avoid assumptions about unpublished monorepo code.
- The root `Cargo.toml` is generated. Do not edit workspace members,
  dependencies, lints, or profiles there. Change a crate's own `Cargo.toml`
  instead.
- `SOURCE_REV` records the source monorepo revision. Change it only as part of
  the repository sync process.
- Treat `third_party/` as vendored source. Do not make feature changes there
  unless the task explicitly targets a vendored component; preserve its
  notices and upstream licenses.
- Do not commit, push, publish, or update generated notices unless explicitly
  requested.

## Releases

- Do not create or push a release tag directly. When publication is explicitly
  requested and `main` is ready, manually dispatch
  `Publish release (warm, tag, Release)` from `main`; it warms the exact release
  inputs before creating the version tag and starting `Release`.
- Ordinary pushes and pull-request merges must not publish a release. See
  `docs/release-distribution.md` for recovery and verification details.
- Monitor long-running Actions and release workflows with one session-managed
  watcher, then keep the current agent turn alive with the platform wait
  mechanism until success, failure, or watcher error. Do not repeatedly
  foreground-poll, end the turn while the watcher is running, rely on a
  post-turn callback, or launch a detached watcher whose terminal result cannot
  be delivered back to the thread.
- Release monitoring must follow the warmup through the dispatched `Release`
  run, then verify the tag commit and published assets. Do not advance `main`
  while a release warmup is still bound to its current commit.

## Architecture boundaries

- `xai-grok-pager-bin` is the composition root and executable entry point.
- `xai-grok-pager` owns interactive presentation and TUI state; keep model,
  tool, and workspace behavior out of view code.
- `xai-grok-shell` owns application/session orchestration and the agentic turn
  loop. Prefer extracting reusable leaf behavior instead of adding more
  presentation concerns to it.
- `xai-grok-agent` owns agent definitions, prompt assembly, and tool selection.
- `xai-grok-sampler` owns model transport, streaming, and retry mechanics.
- `xai-grok-tools` owns tool definitions and implementations.
- `xai-grok-workspace` owns host-local filesystem, VCS, process execution,
  trust, and checkpoint operations.
- Wire/data-only crates should not acquire dependencies on the shell or pager.

See `docs/ARCHITECTURE.md` for the runtime flows and the broader crate map.

## Editing and validation

- Make the smallest coherent change in the owning crate.
- Follow the pinned toolchain in `rust-toolchain.toml` and the formatting and
  lint configuration at the repository root.
- Prefer targeted validation because a full workspace build is expensive:

  ```sh
  cargo fmt --all -- --check
  cargo check -p <crate>
  cargo test -p <crate>
  cargo clippy -p <crate>
  ```

- Run the narrowest relevant test first. Expand to dependent crates when a
  public API, shared type, feature, or runtime boundary changes.
- Keep tests hermetic. Do not require real credentials, user configuration, or
  external services.
- Do not rewrite snapshots, lockfiles, generated code, or broad formatting
  output unless the change requires it and the resulting diff is reviewed.

## Documentation

- Update `docs/ARCHITECTURE.md` when a composition root, runtime boundary,
  cross-crate flow, or persistence contract changes.
- Update the pager user guide for user-visible commands, configuration, keys,
  permissions, skills, plugins, hooks, or session behavior.
- Keep `AGENTS.md` concise and actionable; put explanatory material in normal
  documentation and link to it.
