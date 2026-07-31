# RFC 0001：Multi-provider、Plan Mode、异步 Reviewer 与可分发运行时

- 状态：Implemented（baseline）
- 目标：用少量原生运行时原语支持可配置的 planner / executor / reviewer
- 影响范围：config、model catalog、session、sampler、hooks、distribution

## 1. 结论

本项目不在第一版引入一套固定的 `plan -> execute -> review` 工作流引擎。
实现采用更小、也更接近 Claude Code 的组合：

1. **Provider / model / route 是原生能力。** 每个 provider 有独立 endpoint、
   protocol、认证、headers、重试、超时和 cache policy；model 引用 provider；
   logical route 在请求开始前选择一个物理模型。
2. **Planner 是同一 session 的 Mode Profile。** `/plan` 进入既有 Plan Mode，
   临时应用单独的 model/route、instructions 和 skills；退出时安全恢复原模型。
3. **Executor 就是 main session。** 计划获批后仍由当前会话执行；用户可随时
   用 `/model` 切换 main 的模型，不需要复制上下文到一个 executor child。
4. **Reviewer 是 main session 的只读 subagent。** `Stop` hook 在 turn 结束时
   发现 `HEAD` 被 commit 推进，就返回 block decision 要求 main spawn 一个
   只读 review subagent；结果沿 subagent 原生回传，由 main 判断修复、忽略、
   继续或请求用户输入。
5. **Skill / agent / hook / plugin 负责编排内容，核心负责不变量。**
   Prompt、工具集、模型选择和 hook 配置可以随 profile/plugin 分发；model
   scope、只读边界、live-session 注入、幂等和持久化必须留在核心。
6. **Anthropic 1h cache 和 portable distribution 是原生能力。**
7. **Reviewer 策略不随 terminal/runtime 分发。** 核心只提供 command hook、
   `Stop` block decision 和 subagent 原语；触发条件、prompt 和 adapter 由独立
   的 `grok-build-configs` plugin 维护。共享插件不注册泛化 reviewer agent，
   subagent 沿用当前 session 的 model/route/provider 解析。

这使“Hook 只能执行 command”不再是限制：command 是薄适配器，它只需判断该不该
review，并把请求交回 main；真正的执行由 session 自己的 subagent 承担。

## 2. 目标与非目标

### 2.1 目标

- 同一进程中配置并使用多个 provider 和多个 model。
- main、Plan Mode、subagent、reviewer 可分别选择物理模型或 logical route。
- provider-bound model 不会误用 xAI 登录 token。
- `/plan` 使用同一 session 上下文，同时具备真正的只读运行时边界。
- reviewer 不阻塞 commit tool，也不并发 resume 同一个 session。
- reviewer 结果能进入当前 live session，并支持进程内幂等重试。
- Messages adapter 支持 5m、1h 和 off cache policy，并保留 cache usage 明细。
- 构建物可以复制到另一台同平台机器运行，不携带用户凭证和 session。

### 2.2 非目标

- 不实现任意 DAG、repair loop 或持久化 workflow engine。
- 不在已经开始的请求中自动切换 provider。
- 不把外部 reviewer 输出提升为 system authority。
- 不加载 Rust dylib plugin ABI。
- 不在不同 provider 之间共享 prompt cache。
- 不跨平台复用同一个二进制；每个 target 仍需独立产物。

## 3. 整体架构

```mermaid
flowchart TD
    User["User / TUI"]
    Main["Main session\nexecutor + /model"]
    Plan["Plan Mode profile\nmodel + skills + instructions"]
    Gate["Plan runtime gate\nread-only except plan.md"]
    Catalog["Model catalog\nprovider + model + route"]
    Sampler["Protocol adapters\nchat / responses / messages"]
    Hook["Stop command hook\nHEAD advanced by a commit"]
    Reviewer["Read-only review subagent\nchild of this session"]

    User -->|"/plan"| Plan
    Plan --> Main
    Plan --> Gate
    Main --> Catalog
    Catalog --> Sampler
    Main -->|"turn end after a commit"| Hook
    Hook -->|"block decision: spawn reviewer"| Main
    Main --> Reviewer
    Reviewer -->|"findings as subagent result"| Main
```

关键边界：

- `xai-grok-shell` 拥有 session actor、mode scope、model resolution 和通知注入。
- `xai-grok-sampler` 只接收已解析的 transport/model/cache 配置。
- `xai-grok-agent` 与 skills 提供 prompt 和工具组合。
- `xai-grok-hooks` 只执行 hook command；脚本负责进程级编排。
- `xai-grok-pager` 负责 `/plan`、`/model`、session CLI 和呈现。
- `xai-grok-workspace` 继续拥有命令、权限和 sandbox。

## 4. Multi-provider 与 Multi-model

### 4.1 数据模型

四个概念保持分离：

| 概念 | 职责 |
| --- | --- |
| Protocol | `chat_completions`、`responses`、`messages` 的 wire shape |
| Provider | endpoint、认证、headers、retry、timeout、cache policy |
| Model | 上游 model id、context window、sampling 和能力元数据 |
| Route | 某个逻辑用途对应的有序物理 model 候选 |

`ApiBackend` 继续表示 protocol，不改名为 provider。

### 4.2 配置

```toml
[provider.anthropic]
base_url = "https://api.anthropic.com/v1"
api_backend = "messages"
auth = "x_api_key"
env_key = "ANTHROPIC_API_KEY"
extra_headers = { "anthropic-version" = "2023-06-01" }
max_retries = 5
inference_idle_timeout_secs = 300
prompt_cache = { mode = "stable_prefix", ttl = "1h" }

[provider.openai]
base_url = "https://api.openai.com/v1"
api_backend = "responses"
auth = "bearer"
env_key = "OPENAI_API_KEY"

[provider.local]
base_url = "http://127.0.0.1:11434/v1"
api_backend = "chat_completions"
auth = "none"

[model.claude-planner]
provider = "anthropic"
model = "claude-sonnet"
context_window = 200000

[model.openai-executor]
provider = "openai"
model = "gpt-codex"
context_window = 400000

[model.local-reviewer]
provider = "local"
model = "qwen-coder"
context_window = 65536

[model_route.planner]
candidates = ["claude-planner", "openai-executor"]

[model_route.reviewer]
candidates = ["local-reviewer", "claude-planner"]
```

Provider auth 取值：

- `bearer`：发送 `Authorization: Bearer ...`；
- `x_api_key`：发送 `x-api-key: ...`；
- `none`：不发送认证 header。

`api_key` 和 `env_key` 是 provider 的 credential source。推荐使用 `env_key`。
认证 header 不能同时放进 `extra_headers`。

### 4.3 兼容和安全规则

- 未设置 `provider` 的旧 `[model.*]` 完全保留现有 credential fallback。
- 设置 `provider` 后，model 不得重复 provider-owned 的 `base_url`、
  `api_base_url`、`api_backend`、`api_key` 或 `env_key`。
- Provider headers 是默认值；model 可按大小写无关的 key 覆盖非认证 header。
- Provider-bound model 只使用 provider 的 credential source，绝不 fallback
  到 xAI session token 或 `XAI_API_KEY`。
- `auth = "none"` 与 credential 同时出现是配置错误。
- 未知 provider、空 route、nested route 和认证 header 冲突在配置加载时失败。

### 4.4 Route 语义

Route 在 catalog 构建时按顺序选择第一个 preflight-ready candidate：

- model 必须存在且未被 disabled；
- 需要认证的 provider 必须能解析 credential；
- 未通过 preflight 时才尝试下一个 candidate。

选中后 route 形成隐藏的 `route:<name>` catalog alias。它不会出现在 model
picker 中，但可供 default、Plan Mode、agent 和 subagent 显式引用。

第一版只做 `preflight_only` fallback。请求一旦开始，retry 仍属于同一个
provider/model；不会因 timeout、429、5xx 或已经产生语义事件而跨 provider。
这样可以避免重复 tool call、重复计费和不同模型间的上下文漂移。

## 5. Planner：同 session 的 `/plan`

### 5.1 Mode Profile

```toml
[modes.plan]
model = "route:planner"
skills = ["architecture", "risk-review"]
instructions = """
Produce an implementation-ready plan with explicit acceptance criteria and
end-to-end verification.
"""
restore_model = true
```

进入 `/plan` 时：

1. session actor 读取 `[modes.plan]`；
2. 解析物理 model 或 `route:*`，但不修改 process-wide current model；
3. 记录 `{base, applied}` model locator；
4. 用已有 model-switch path 更新这个 session 的 sampler、context window、
   credentials、cache policy 和 UI notification；
5. 把 instructions 与配置 skill 的完整内容作为 plan-only overlay 注入；
6. 继续使用已有 Plan Mode state machine 和 `plan.md` approval flow。

退出时使用 compare-and-restore：

- 当前仍等于 `applied` 且 `restore_model = true`：恢复 `base`；
- 用户在 Plan Mode 中手动执行过 `/model`：认为 ownership 已转移，不覆盖；
- 原模型暂时不可解析或恢复失败：保留 scope，后续恢复时重试；
- `restore_model = false`：释放 scope，保留当前模型。

scope 使用 write-ahead snapshot 持久化到 `plan_mode.json`。进入与退出的
状态、session current-model 和 scope commit 都必须收到实际文件写入与
`fsync` ACK；任一步失败都会保留可重试记录并阻止下一次 sampling。进程重启
时，已经 collapse 为 Inactive 的 transient state 会在接收新 prompt 前先
尝试恢复，避免 session 卡在 planner 模型上。

Planner 仍使用同一 session transcript。因此配置的 planner provider 会收到
本轮请求携带的既有对话与 read/search 结果；mode profile 不是数据隔离或隐私
边界，部署方必须只选择允许接收这些项目数据的 provider。

### 5.2 真正的只读边界

Prompt 不是安全边界。Active Plan Mode 在 tool dispatch 前执行原生 gate：

- 只允许 `plan.md` 的 edit/write；
- 拒绝其他 edit、write、delete 和 apply-patch；
- 拒绝 Bash/monitor，防止 shell redirection 写文件；
- 拒绝 Task/subagent，防止 child 获得更宽工具集；
- 拒绝 MCP、dynamic/use-tool、scheduler mutation 和 media generators；
- 保留 read/list/grep/search/memory/LSP/web fetch、ask-user、exit-plan 等
  purpose-built read/control tools。

该 gate 优先于 permission manager，因此 always-approve 不能绕过。
在 Unix 上，`plan.md` 本身通过 descriptor-relative no-follow 文件边界
访问：逐级拒绝 symlink parent，拒绝 final symlink、非普通文件与 hard
link，并用同目录临时文件、`fsync`、原子 rename 写入，避免
auto-approve 被路径替换竞态利用。非 Unix fallback 会拒绝校验时发现的
link，但因为缺少 handle-relative parent walk，不承诺抵御并发
reparse-point 替换的同等级保证。

## 6. Executor：main session

计划获批后不创建新的 executor session。这样做有三个直接收益：

- 无需复制或压缩 planner conversation；
- plan approval、用户反馈和执行过程保留在同一 transcript；
- 现有 permission、sandbox、compaction、usage 和 cancellation 路径全部复用。

Main 的模型继续由现有 `/model` 控制，也可以把 `[models].default` 指向
`route:executor`。未来如果需要隔离执行，仍可使用现有 subagent/worktree，
但它不是这套 baseline 的必选步骤。

## 7. Reviewer：Stop hook + 只读 subagent

### 7.1 为什么 command hook 足够

Hook 不需要直接理解 agent，也不需要自己跑 reviewer。它只需在 turn 结束时判断
“这一轮是否产生了新 commit”，然后把请求交回 main：

```text
SessionStart event
  -> record HEAD as this session's baseline

Stop event (reason == end_turn)
  -> compare HEAD against the baseline
  -> unchanged, or moved by a non-commit operation: advance baseline, do nothing
  -> moved by a commit: advance baseline, emit a block decision asking main to
     spawn a read-only review subagent for that exact SHA
```

Subagent 是当前 session 的子节点，结果在它结束时原生回传，不需要 leader、
socket 或外部进程；headless 会话同样成立。baseline 在发出 block **之前**推进，
所以同一个 commit 不会被反复要求 review，stop-continuation 上限是二次兜底。

### 7.2 Hook 配置

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "bash \"${GROK_PLUGIN_ROOT}/scripts/reviewer-hook.sh\"",
            "timeout": 5
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "bash \"${GROK_PLUGIN_ROOT}/scripts/reviewer-hook.sh\"",
            "timeout": 10
          }
        ]
      }
    ]
  }
}
```

共享配置不包含 reviewer agent definition，避免与内置 `/review` 的命名和
persona 解析混淆。

### 7.3 Hook adapter 不变量

外部 `async-commit-reviewer` plugin 的 adapter：

- 用真实 JSON parser（Node 或 jq）解析 hook envelope，不用 regex；
- 只在 `reason == "end_turn"` 的真实 turn 结束上动作；
- 用 reflog 区分 commit 类操作与 branch switch / reset / fast-forward pull；
- per-session、per-repo 的 baseline 用密码学摘要做 key，避免碰撞；
- 任何解析或 IO 错误都 fail-open，退出 0，不阻塞 turn；
- 请求的 subagent 必须是 `capability_mode="read-only"` 且针对显式 SHA。

### 7.4 信任边界

Reviewer 输出以普通 subagent 结果的形式回到 main，属于可评估的证据，不是
system 指令。Reviewer 无权替 main 作最终决定，也不能通过输出提升权限。

## 8. Skill、Agent、Plugin 的扩展策略

### 8.1 Baseline 中什么是原生的

必须原生实现：

- provider/model/route resolution；
- session-scoped mode model ownership；
- Plan Mode tool gate；
- session actor queue/wake 语义；
- subagent 生命周期与结果回传；
- prompt-cache wire contract；
- distribution verification。

这些能力涉及安全、并发、恢复或 transport，不适合由 prompt/script 模拟。

### 8.2 什么放在 Skill / Agent / Hook

- Planner 的领域方法：skill；
- Planner 的额外约束：`[modes.plan].instructions`；
- Reviewer 默认沿用普通 headless 配置；可选的专用 persona、模型、tools、
  skills 才放在用户自己的 agent definition；
- 何时触发 review：hook matcher/adapter；
- 一组可复用配置：profile 或 plugin。

一个 plugin 可以打包：

```text
plugin.json
skills/architecture/SKILL.md
hooks/reviewer-after-commit.json
scripts/reviewer-hook.sh
```

它不需要新的 workflow ABI，也不需要进入 runtime release archive。个人
配置仓库可以独立 version、测试和安装该 plugin。

### 8.3 后续扩展

未来可按需要增加：

- `modes.design`、`modes.debug` 等通用 mode profile；
- 面向真正外部事件（CI、webhook）的 live-session 注入通道；
- hook event filters（commit、push、PR、test completion）；
- reviewer fan-out 与结果聚合；
- reviewer 对 diff artifact 的内容寻址输入；
- route health/capability preflight；
- provider account/workspace 隔离与 cost policy。

只有当产品确实需要跨重启的多 stage DAG、human gate 和有界 repair loop 时，
才应新增核心 workflow scheduler。届时 plugin 可以提供声明式 definition，
但 scheduler、安全、状态和 UI 仍属于核心。

## 9. Anthropic 1 小时 Prompt Cache

### 9.1 Policy

```rust
struct PromptCachePolicy {
    mode: PromptCacheMode, // off | stable_prefix
    ttl: PromptCacheTtl,   // 5m | 1h
}
```

优先级：

1. model override；
2. provider policy；
3. protocol-compatible default。

Messages 保留历史默认：`stable_prefix + 5m`。其他 adapter 忽略该字段。

### 9.2 Wire 行为

- `off`：不产生显式 breakpoint；
- `stable_prefix + 5m`：最后一个稳定 system block 加
  `{"type":"ephemeral"}`，省略 TTL；
- `stable_prefix + 1h`：加
  `{"type":"ephemeral","ttl":"1h"}`。

标记 system prefix 也覆盖 Anthropic prompt 顺序中更早的 tools。第一版只有
一个 explicit breakpoint，不涉及混合 TTL 的排序问题。

Anthropic 官方协议说明 5m 默认、`ttl: "1h"`、最多四个 breakpoint，以及
`cache_creation.ephemeral_5m_input_tokens` /
`ephemeral_1h_input_tokens` usage：
[Prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)。

### 9.3 Usage

Normalized usage 保留：

- prompt tokens；
- completion tokens；
- cache-read input tokens；
- cache-write 5m input tokens；
- cache-write 1h input tokens。

两个 write bucket 是 prompt token 的明细，不重复计入 total。老 provider
只返回 aggregate `cache_creation_input_tokens` 时仍兼容，bucket 为 0。

## 10. 独立构建与跨机器复用

### 10.1 Canonical commands

```bash
scripts/dist.sh build \
  --target aarch64-apple-darwin \
  --version 0.2.101

scripts/dist.sh package \
  --target aarch64-apple-darwin \
  --version 0.2.101

scripts/dist.sh verify dist/0.2.101/grok-build-0.2.101-aarch64-apple-darwin.tar.gz
```

Build 使用：

```bash
cargo build --locked \
  -p xai-grok-pager-bin \
  --profile release-dist \
  --features release-dist \
  --target TARGET
```

### 10.2 Archive contract

```text
grok-build-VERSION-TARGET/
  bin/grok
  LICENSE
  THIRD-PARTY-NOTICES
  SOURCE_REV
  build-manifest.json
  MANIFEST.sha256
  profiles/starter/
```

Manifest 记录 source revision、target、toolchain、features、binary hash、
bundled search-tool hashes 和 profile hash。Archive 还生成外部
`SHA256SUMS`。

Starter profile：

- 不含 credential、`auth.json`、session、managed policy 或 cache；
- provider credential 只引用环境变量名；
- 不包含任何 opinionated reviewer hook、agent 或个人 prompt；
- 可复制到任意可写目录并通过 `GROK_HOME` 选择。

Verification 拒绝：

- path traversal 和 symlink；
- 缺失、额外或 hash 不匹配的文件；
- secret-like inline value；
- 文本 payload 中的机器绝对路径；
- 被篡改的 profile/manifest/checksum。

## 11. 测试与验收

### 11.1 Rust

- provider credential isolation、header merge、配置冲突；
- route 顺序、缺 credential fallback、hidden alias、explicit default；
- Plan Mode model scope capture/restore/manual switch/persistence；
- Plan Mode 对 Bash、subagent、MCP 和非 plan edit 的 fail-closed gate；
- Messages 5m/1h/off request JSON；
- cache usage wire parsing、ledger fold 和 response metadata；
- subagent 生命周期：spawn、只读 capability、结果回传；
- CLI argument/file-size/error behavior。

### 11.2 Shell / distribution

- 外部 reviewer plugin 自己测试 success、ignored events、failure report、
  baseline 推进、非 commit 的 HEAD 移动、解析失败 fail-open 和 fast return；
- tar/zip creation、reproducibility、checksums、tamper rejection；
- real `release-dist` build；
- archive extraction with isolated `GROK_HOME`；
- extracted `grok --version`、`--help` 和 completion smoke tests。

### 11.3 最终检查

```bash
cargo fmt --all -- --check
cargo check -p xai-grok-sampling-types
cargo test -p xai-grok-sampling-types
cargo check -p xai-grok-sampler
cargo test -p xai-grok-sampler
cargo check -p xai-grok-shell
cargo test -p xai-grok-shell
cargo check -p xai-grok-pager
cargo test -p xai-grok-pager
scripts/dist/test.sh
git diff --check
```

## 12. 取舍记录

### 固定 workflow engine

暂不采用。它会立即引入 stage schema、artifact protocol、repair budget、
recovery 和 UI 等大量状态，而当前需求用 Mode Profile + hook + subagent 就能
完整表达。

### 只用 prompt/skill

不采用。Skill 适合规划方法，不适合 credential isolation、tool gate、actor
queue、幂等和 cache wire semantics。

### Hook 内直接 resume parent session

不采用。两个进程同时拥有同一个 session 会破坏 actor serialization、
conversation persistence、usage 和 cancellation。

### Reviewer 作为独立进程 + live-session notify

最初的实现。它需要一个 live leader（`~/.grok/leader.sock`）才能投递，而 leader
模式默认关闭，headless 会话根本没有 leader：报告能生成却永远送不进去。改为
turn 结束后派生的**后台只读 subagent**，用原生结果回传取代 socket 注入；
`grok sessions notify` 原语随之移除。

真正的外部事件源（CI 完成、webhook、定时任务）仍然没有注入通道。等到出现确定
的使用场景时再重新引入，并且必须同时给出 leader 默认策略和端到端测试。

### 运行中跨 provider fallback

不采用。只做 preflight routing，避免重复副作用和语义漂移。
