# Community usage guide

This guide is for developers who want to install this Grok Build fork, use
their own model provider, or compose built-in and MCP tools for repository
work. The maintainer reports that friends and colleagues use the fork for
their own development workflows. Anyone can use the same public release
and configuration instructions.

## Install and verify

Use the [platform installer](../README.md#installation-and-public-releases)
for the latest stable build. The fork publishes ARM64 and x86-64 binaries for
macOS, Linux, and Windows. Run the following commands after installation:

```sh
grok --version
grok inspect --json
```

Fork release binaries report `"privacyHardened": true`. The
[privacy hardening guide](privacy-hardening.md) lists the analytics and
upload paths disabled at compile time and the network paths still in use.
Your chosen model service can receive prompts, selected files, and tool
output included in inference requests.

For a manual install, choose a `grok-build-<version>-<target>` archive from
[GitHub Releases](https://github.com/zhangtyzzz/grok-build/releases) and
compare its SHA-256 digest with the matching entry in `SHA256SUMS` from
the same release. Use `shasum -a 256 <archive>` on macOS,
`sha256sum <archive>` on Linux, or
`Get-FileHash -Algorithm SHA256 <archive>` in PowerShell. Extract the
archive and run `bin/grok` or `bin\grok.exe` from the extracted directory.
On Unix, keep the executable permission when moving the binary.

Installer-managed copies use this fork's release feed for in-app updates.
Run `grok update` to check on demand. For a manually extracted copy, repeat
the download and verification steps for the new release. Stable builds use
`vX.Y.Z` tags. Testing builds are marked as prereleases on GitHub.

## Configure a model provider

Place the following example in your user `config.toml` under `GROK_HOME`
(default `~/.grok`, or `%USERPROFILE%\.grok` on Windows). Merge the example
with any existing settings. Replace each URL, model ID, and context window
with the values for the service you use. The hosted example uses a bearer
key from the `CODING_GATEWAY_API_KEY` environment variable. Set it in the
environment that launches `grok` using your usual secret manager. The local
example assumes a chat-completions server listening on loopback with tool
calling enabled.

```toml
[model_providers.hosted]
base_url = "https://gateway.example.com/v1"
api_backend = "chat_completions"
auth_scheme = "bearer"
env_key = "CODING_GATEWAY_API_KEY"

[model_providers.local]
base_url = "http://127.0.0.1:11434/v1"
api_backend = "chat_completions"
auth_scheme = "none"

[model.hosted-coding]
model_provider = "hosted"
model = "your-hosted-model-id"
context_window = 32768

[model.local-coding]
model_provider = "local"
model = "your-local-model-id"
context_window = 32768

[model_route.coding]
candidates = ["hosted-coding", "local-coding"]

[models]
default = "route:coding"
```

With `auth_scheme` set, inference to that provider never falls back to the
xAI login token or `XAI_API_KEY`. With `auth_scheme = "none"`, the local
provider sends no inference credentials. Keep authentication headers out of
`extra_headers` when using a bound provider.

The default route tries candidates in order and can skip a model whose
required credential is missing before inference begins. A request that has
already started stays on its selected provider even if it encounters an API
error. See the [custom models guide](../crates/codegen/xai-grok-pager/docs/user-guide/11-custom-models.md#named-providers-and-model-routes)
for OpenAI Responses, Anthropic Messages, and Anthropic prompt-cache options.

### Compare two models on the same task

Open a repository that you can share with both configured providers. In
separate sessions, use the same model prompt and compare the results. An
example using the headless CLI:

```sh
grok -m hosted-coding -p "Read README.md and list the supported platforms. Do not edit files."
grok -m local-coding -p "Read README.md and list the supported platforms. Do not edit files."
```

In the TUI, use `/model hosted-coding` and `/model local-coding` to switch
models. Run `grok models` to inspect the available models if a name is missing.

## Try Code Mode and MCP

Code Mode is enabled by default and gives the model an `execute` tool for
composing multiple calls in one JavaScript script. From a checked-out
repository, ask the agent to find the test entry points, read their build
configuration, and return a summary without editing files. Ask it to use
Code Mode to combine the searches and reads. If you have a read-only MCP
server configured, the same script can look up related documentation
through that server and include it in the summary.

The model chooses when to use `execute`. Tool permissions and hooks apply to
each call. For a comparison on macOS or Linux, start a fresh session with
`GROK_CODE_MODE=0 grok`. For Windows PowerShell, set
`$env:GROK_CODE_MODE = '0'` before running `grok`, then remove the override
with `Remove-Item Env:GROK_CODE_MODE`.

See the [Code Mode guide](code-mode.md) for the JavaScript interface and
the [MCP setup guide](../crates/codegen/xai-grok-pager/docs/user-guide/07-mcp-servers.md)
for adding a server.

## Get help and report issues

For bugs, installation problems, or suggestions, open an issue on the
[fork's issue tracker](https://github.com/zhangtyzzz/grok-build/issues).
Include the release version, OS and architecture, and a minimal reproduction.
For security reports, use the [private disclosure process](../SECURITY.md).
