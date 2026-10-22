# Code Mode

Code Mode lets the model submit one JavaScript program that calls tools, examines
results, and continues without a model request between each dependent step. It
composes every tool the session can call, including MCP and managed-gateway
tools that a shell command cannot reach. It is enabled by default:

```sh
GROK_CODE_MODE=0 grok   # disable (also false/off/no)
GROK_CODE_MODE_TIMEOUT_SECS=900 grok   # overall run limit, 10..=3600, default 600
# Both apply to the shipped grok TUI, headless and ACP entry points.
```

The model gains an additive `execute` tool; every direct tool, including the
`search_tool`/`use_tool` path to MCP tools, stays available, and the model picks
per task. If a tool already owns the name `execute`, Code Mode is not advertised
or intercepted. Enabling it authorizes computation only; all external effects
still pass the existing individual tool admission checks.

Direct calls already run in parallel, so a few independent calls gain nothing
from a script. The model cannot know a result's size before calling, so the
guidance keys on what the task needs instead: `execute` pays off when the task
wants a derived answer (filtered, counted, deduplicated or joined data) rather
than the raw results, or a fixed multi-step flow feeds one result into the
next. Steps that need the model to read full content and decide in between
stay direct; a direct result that turns out too long is a cue to script later
similar calls. While `execute`
is advertised, the injected MCP reminder (live and after compaction, both from
`mcp_usage_hint`) names both schema paths — `search_tool` then `use_tool`, or
`catalog.describe` then `tools[name]()` — instead of requiring `search_tool`.

## Script contract

The `code` input is an async JavaScript function body. Use explicit `return`,
otherwise the value is null. For example (names/schemas vary with the toolset):

```js
const matches = catalog.search('read');
console.log(matches);
return catalog.describe(matches.tools[0].name);
```

After discovering the exact name and schema:

```js
const settled = await Promise.allSettled([
  tools.read_file({ target_file: 'src/main.rs' }),
  tools.read_file({ target_file: 'Cargo.toml' }),
]);
return settled.map((result, index) => ({
  index,
  status: result.status,
  text: result.status === 'fulfilled' ? result.value.slice(0, 200) : result.reason.message,
}));
```

The host returns normal model-facing tool text as a **string**, including normal
output caps/spill references and hook replacements. Parse it as JSON only when
that tool's documented output is JSON. Runtime/permission/policy/tool errors
reject the Promise; tools' own successful structured/text contracts still apply
(for example a reported command exit status need not be a dispatch exception).

`catalog.search(query, offset)` matches names and descriptions case-insensitively
and returns at most 20 summaries per page; `catalog.describe(name)` returns the
full input schema. Names are not rewritten into aliases: bracket notation works
for non-identifiers, e.g. `tools["linear__save_issue"]({...})`.
Both catalog functions return JSON objects directly; only host tool calls return
model-facing text strings.

The inventory is every declared builtin tool plus, whenever `use_tool` is offered,
every MCP and managed-gateway tool from the snapshot `search_tool` searches (the
same `server__tool` names; disabled tools are absent). Excluded are only
`execute` itself, the `search_tool`/`use_tool` pair (their targets are listed
directly), and the turn-control tools `enter_plan`, `exit_plan` and `ask_user`,
which wait on the user or switch the turn's mode. A nested MCP call runs as an
ordinary inline `use_tool` call, so MCP auth retry, argument coercion,
permissions, hooks and telemetry are unchanged. Only the model-facing inline
limit is skipped: `use_tool` truncates MCP text for calls the model reads
directly (`MAX_MCP_OUTPUT_BYTES`, 20 KB by default), but a nested call registers
its id in `UntruncatedMcpCalls`, so the script receives complete data it can
parse. A single nested result over the 2 MiB frame limit fails that call with a
catchable error instead of aborting the run; the final `execute` result keeps
its own bound. System reminders appended to a nested result (background-task
completions, diagnostics, coercion notes) are split off before the script sees
the text and appended after the `execute` result, so they neither corrupt JSON
nor get lost. Every nested call keeps normal
validation, hooks, permissions, trust/sandbox checks, same-file locking and
cancellation. Media-generation caps count all calls admitted in one run. The
inventory is frozen per run: a tool whose definition changes mid-run is
rejected, and tools connected later need a new execute invocation.

Each program gets a fresh QuickJS process. No imports, filesystem, network,
process, timers or credentials are injected. All external access is through host
tools. This is not a complete operating-system security sandbox. Native QuickJS
is used through rquickjs; no Node/Bun/Python installation is needed.

Return JSON data; explicitly await nested Promises. Cycles, functions, BigInt,
nonfinite or imprecise integer values are rejected. Console logs and final data
are bounded. There is no persistent store, recursive execute, automatic final
expression return, automatic script retry, or durable program replay.

## Cancellation, partial effects and traces

Tool calls start eagerly. Await required work before returning. Program return,
failure, user cancellation, timeout or worker failure closes admission and drops
owned tool futures; external tools may already have committed effects. These
are **not rolled back**, and cancellation does not prove that an external
command or service stopped. Unconfirmed calls are marked `outcome_unknown`.
Do not retry an entire failed program with writes without inspecting its trace.

When the user sends a new message mid-run, running nested commands move to the
background as they do for direct calls. Their notice answers the parent
`execute` call (nested IDs never enter history), listing each background task ID
for later output retrieval.

The model conversation receives only the outer execute result, selected value,
bounded logs/warnings, call summaries and images. Local session files named
`code-mode-<uuid>.jsonl` retain the parent ID, program, admissions, actual prepared
arguments/identity, nested results, and unknown outcomes. They follow the
session's existing access restrictions and can contain sensitive tool data;
there is no new upload/telemetry backend. The returned `trace` field is a local
absolute path; reading it still requires ordinary read permissions. Export these trace files with the
session when nested provenance is required; transcript-only exports do not
contain the full nested trace. Resuming does not replay interrupted scripts.

Limits are initial conservative bounds, not performance claims:

| Resource | Limit |
| --- | --- |
| Source | 64 KiB |
| JS heap / stack | 64 MiB / 512 KiB |
| Host tool calls / in flight per program | 64 / 8 |
| Workers per host process | 2, slot held until actual worker exit |
| Active JS computation | 5 seconds cooperative budget (excludes blocked tool-reply reads); overall deadline kills stalled workers |
| Overall execution | 600 seconds by default (`GROK_CODE_MODE_TIMEOUT_SECS`, 10–3600), **including tool, subagent and permission waits** |
| Final serialized value plus raw log text | 64 KiB; at most 256 log entries |
| Aggregate host results/images/followups per program | 16 MiB |
| IPC frame | 2 MiB, checked before body allocation |

The outer result envelope and trace are additional to the final value/log budget.
Return a smaller summary if the output limit is exceeded. Long commands and
interactive operations can use the ordinary direct tools. Approval waits may
consume the overall deadline; timed-out programs do not resume when a late
approval arrives.

## Architecture and references

The shell reuses its existing batch prepare/dispatch/finalize path. A private
async task-local sink redirects nested results, including all rejection/error
paths, away from the provider transcript. It is scoped around each nested call;
dispatch tasks do not write transcripts. Per-run file locks span nested batches.
A model response that mixes `execute` with direct calls runs as one tool round:
groups share the interruption state, and deferred followups, interjections and
skill reminders are flushed once after every call has its result, so tool
results stay contiguous after the assistant message as providers require.
The composition root enters the hidden worker before normal application startup.
Workers enroll in global and session process scopes and use the existing Linux
parent-death helper, so session reclamation also reaches a busy runtime.

References are [OpenCode's tool bridge](https://github.com/anomalyco/opencode/blob/4d5965762ec5e0d1a0b14630553fc62b51a11fba/packages/core/src/codemode/tool.ts),
[Pi's QuickJS Code Mode](https://github.com/earendil-works/pi/tree/82432219af91c2342fc1b096e179e96df6ed8e7f/packages/codemode),
and [Cloudflare's Code Mode article](https://blog.cloudflare.com/code-mode/).
OpenCode uses its own JS subset interpreter; Pi uses QuickJS Wasm workers. This
Rust host uses native QuickJS child processes and its own IPC and tool integration.
No compatibility with either complete API is claimed.

## Reproduce validation

```sh
cargo check --locked -p xai-grok-tools -p xai-grok-shell -p xai-grok-pager-bin
RUST_MIN_STACK=8388608 cargo test --locked -p xai-grok-tools -p xai-grok-shell --lib code_mode
cargo build --locked -p xai-grok-tools --example code_mode_worker
GROK_TEST_CODE_MODE_WORKER="$PWD/target/debug/examples/code_mode_worker" \
  RUST_MIN_STACK=8388608 cargo test --locked -p xai-grok-shell --lib \
  code_mode::tests::real_worker -- --ignored --test-threads=1
python3 scripts/bench-code-mode.py target/debug/examples/code_mode_worker
```

The benchmark has fixed local fake tools and verifies identical outputs and
actual nested call count across ten fresh workers. Its direct baseline describes
three **dependent** tool submissions; an ordinary model can already batch
independent calls. It reports payload bytes and process overhead, not model
tokens, live model latency, or task success rates. It excludes program/catalog
and result-envelope costs and must not be presented as a total token saving.

A [24-session live-model check](code-mode-live-evaluation.md) of the first,
builtin-only inventory records read/filter gains and a terminal-enabled control
with no script adoption. That inventory offered nothing a shell could not do.
Default-on follows OpenCode and Pi, which route MCP tools through their code
modes by default; here direct MCP calls stay available, so the only fixed cost
is the `execute` definition.
