# Grok Build — independently maintained fork

Grok Build is a terminal AI coding agent for developers who want to work with
their own model providers and tools. This fork publishes ready-to-install
builds for macOS, Linux, and Windows under Apache-2.0. It is maintained by one
developer as a non-commercial open-source project.

[Install the stable build](#installation-and-public-releases) ·
[Configure a model and try a workflow](docs/COMMUNITY_USAGE.md) ·
[Report an issue](https://github.com/zhangtyzzz/grok-build/issues)

## About this fork

The [upstream Grok Build project](https://github.com/xai-org/grok-build) provides
the Rust agent runtime, terminal UI, headless CLI, Agent Client Protocol (ACP)
integration, custom endpoint configuration, and built-in and MCP tools. This
fork extends that foundation so developers can bring their own model keys,
control which credentials accompany inference requests, and install builds
with a documented privacy boundary.

### What this fork adds

| Extension | Implementation and usage |
| --- | --- |
| **Provider policies and model routes.** Set authentication, timeout, retry, and prompt-cache policies on named providers. Select a model directly or choose from an ordered route before a request starts. | [Custom model configuration](crates/codegen/xai-grok-pager/docs/user-guide/11-custom-models.md#named-providers-and-model-routes) · [Provider policy](crates/codegen/xai-grok-shell/src/agent/provider_policy.rs) · [Route resolution](crates/codegen/xai-grok-shell/src/agent/config.rs) |
| **Inference credential isolation.** When a provider explicitly sets `auth_scheme`, its requests use the bound credentials or no authentication, according to the scheme. | [Provider binding and credential rules](FORK_CHANGES.md#32-自定义-provider-的增量策略与凭据隔离) |
| **Privacy-hardened release builds.** Compile-time gates disable the documented analytics, telemetry, and background upload paths. Model inference and other documented network paths remain available. | [Privacy boundary and verification](docs/privacy-hardening.md) · [Release feature wiring](crates/codegen/xai-grok-pager-bin/Cargo.toml) |
| **JavaScript Code Mode.** The model can compose built-in and configured MCP tools in one async script. Each tool call goes through the usual permissions and hooks. | [Code Mode guide](docs/code-mode.md) · [Stable implementation PR](https://github.com/zhangtyzzz/grok-build/pull/104) |
| **Independent releases and updates.** The fork builds, packages, and smoke-tests six targets. Its installers configure updates to use this repository's GitHub Releases. | [Release workflow](.github/workflows/release.yml) · [CI workflow](.github/workflows/ci.yml) · [Distribution guide](docs/release-distribution.md) |

## Installation and public releases

The [stable release](https://github.com/zhangtyzzz/grok-build/releases/latest)
provides installers and archives for macOS, Linux, and Windows on ARM64 and
x86-64. Run the command for your platform.

**macOS or Linux (Bash)**

```sh
curl -fsSL https://github.com/zhangtyzzz/grok-build/releases/latest/download/install.sh | bash
```

**Windows (PowerShell)**

```powershell
irm https://github.com/zhangtyzzz/grok-build/releases/latest/download/install.ps1 | iex
```

The installers verify the downloaded binary against the release's
`SHA256SUMS`. Open a new terminal if needed and run `grok --version`. To
review the installer before running it, download it from the release page.
The [community guide](docs/COMMUNITY_USAGE.md#install-and-verify) also walks
through manual installation, privacy verification, and updates.

## Community usage

Use the fork to try the same coding task with different model providers,
connect a supported local or hosted endpoint, or compose read-only repository
and MCP calls in a single Code Mode script. The
[community guide](docs/COMMUNITY_USAGE.md) walks through these workflows.

The maintainer reports that friends and colleagues also use the fork for
their own development workflows. The project welcomes bug reports and
feature requests from anyone using the public builds. See the
[contribution guide](CONTRIBUTING.md) for the relevant contact channels.

## Upstream attribution

Grok Build was created by SpaceXAI and is published upstream at
[xai-org/grok-build](https://github.com/xai-org/grok-build). This repository is
an independently maintained GitHub fork. [`SOURCE_REV`](SOURCE_REV) records
the imported monorepo revision, and [`FORK_CHANGES.md`](FORK_CHANGES.md)
records the changes the fork maintains across upstream syncs.

## Building from source

Requirements:

- **Rust** — the toolchain is pinned by [`rust-toolchain.toml`](rust-toolchain.toml);
  `rustup` installs it automatically on first build.
- **[DotSlash](https://dotslash-cli.com)** — required so hermetic tools under
  [`bin/`](bin/) (notably [`bin/protoc`](bin/protoc)) can download and run.
  Install it and ensure `dotslash` is on your `PATH` **before** building:

  ```sh
  cargo install dotslash
  # or: prebuilt packages — https://dotslash-cli.com/docs/installation/
  /usr/bin/env dotslash --help   # sanity check
  ```

- **protoc** — proto codegen resolves [`bin/protoc`](bin/protoc) via DotSlash,
  or falls back to a `protoc` on `PATH` / `$PROTOC`.
- The release workflow builds and smoke-tests native macOS, Linux, and Windows
  artifacts on both ARM64 and x86-64. Local source builds remain simplest on
  macOS or Linux.

```sh
cargo run -p xai-grok-pager-bin              # build + launch the TUI
cargo build -p xai-grok-pager-bin --release  # release binary: target/release/xai-grok-pager
cargo check -p xai-grok-pager-bin            # fast validation
```

The source build produces `xai-grok-pager`; the released command is named
`grok`. See the
[authentication guide](crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md)
for xAI sign-in and the [community guide](docs/COMMUNITY_USAGE.md#configure-a-model-provider)
for custom inference providers.

## Documentation

The [upstream documentation](https://docs.x.ai/build/overview) covers the base
product. Follow the [community guide](docs/COMMUNITY_USAGE.md) for this fork's
installation and configuration.

Repository contributor documentation:

- [Architecture](docs/ARCHITECTURE.md) — runtime flow, crate boundaries, state,
  and where to make changes.
- [Privacy hardening](docs/privacy-hardening.md) — the compile-time data
  collection boundary applied to custom release artifacts.
- [Release distribution](docs/release-distribution.md) — reproducible package,
  verification, and publication workflow.
- [Multi-provider and multi-agent RFC](docs/rfcs/0001-multi-provider-multi-agent-runtime.md)
  — the implemented provider/model registry, asynchronous external-agent
  notification contract, Anthropic prompt caching, and standalone distribution.
- [Repository instructions](AGENTS.md) — editing constraints and targeted
  validation for humans and coding agents.

The user guide ships with the pager crate:
[`crates/codegen/xai-grok-pager/docs/user-guide/`](crates/codegen/xai-grok-pager/docs/user-guide/)
— getting started, keyboard shortcuts, slash commands, configuration, theming,
MCP servers, skills, plugins, hooks, headless mode, sandboxing, and more.

## Repository layout

| Path | Contents |
|------|----------|
| `crates/codegen/xai-grok-pager-bin` | Composition-root package; builds the `xai-grok-pager` binary |
| `crates/codegen/xai-grok-pager` | The TUI: scrollback, prompt, modals, rendering |
| `crates/codegen/xai-grok-shell` | Agent runtime + leader/stdio/headless entry points |
| `crates/codegen/xai-grok-tools` | Tool implementations (terminal, file edit, search, ...) |
| `crates/codegen/xai-grok-workspace` | Host filesystem, VCS, execution, checkpoints |
| `crates/codegen/...` | The rest of the CLI crate closure (config, MCP, markdown, sandbox, ...) |
| `crates/common/`, `crates/build/`, `prod/mc/` | Small shared leaf crates pulled in by the closure |
| `third_party/` | Vendored upstream source (Mermaid diagram stack) — see below |

> [!IMPORTANT]
> The root `Cargo.toml` (workspace members, dependency versions, lints,
> profiles) is **generated** — treat it as read-only. Prefer editing per-crate
> `Cargo.toml` files.

## Development

```sh
cargo check -p <crate>        # always target specific crates; full-workspace builds are slow
cargo test -p xai-grok-config # per-crate tests
cargo clippy -p <crate>       # lint config: clippy.toml at the repo root
cargo fmt --all               # rustfmt.toml at the repo root
```

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for public feedback, patches, and
private security reporting.

## License

First-party code in this repository is licensed under the **Apache License,
Version 2.0** — see [`LICENSE`](LICENSE).

Third-party and vendored code remains under its original licenses. See:

- [`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES) — crates.io / git dependencies,
  bundled UI themes, and **in-tree source ports** (including openai/codex and
  sst/opencode tool implementations)
- [`crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md`](crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md)
  — crate-local notice for the codex and opencode ports (license texts +
  Apache §4(b) change notice)
- [`third_party/NOTICE`](third_party/NOTICE) — vendored Mermaid-stack index
