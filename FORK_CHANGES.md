# Fork maintenance contract

本文件供维护者和上游同步任务使用，回答四个问题：这个 fork 为什么存在、哪些行为必须保留、哪些实现应当跟随上游、冲突解决后怎样证明没有回归。它不是所有历史提交的流水账，也不把“当前存在差异”自动等同于“维护者要求保留”。

Fork：`zhangtyzzz/grok-build`。上游：`xai-org/grok-build`。两者的主分支均为 `main`。

最近核对：2026-10-17。审阅起点为 fork `c8f900df9c1de4af9e70ef887ac48ba514ec8792`（PR #100），已包含上游 `2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8`。本次修正缓存覆盖、provider 认证头与 route 的策略/重试/上下文边界，并删除未使用的 durable 模型切换残留和重复工具观测 span；修正与本文一同提交。未来同步必须重新取证，不把这两个 SHA 当作永久基线。

## 1. 背景与目标

这个 fork 保留 Grok Build 的 CLI/TUI 和 agent runtime，补足以下使用场景：

1. 发布编译期隐私硬化的二进制，使已列明的数据收集和上报通道不能被环境变量、本地配置或远端设置重新开启。
2. 使用自定义模型服务，尤其是 Anthropic Messages 协议及其 5 分钟 / 1 小时提示缓存；不同服务的端点、认证和模型身份必须隔离。
3. 通过自己的 GitHub Releases 分发、校验和更新 macOS、Linux、Windows 的 ARM64 / x86-64 包，不让安装器或应用内更新跳回上游发行源。
4. JavaScript Code Mode：模型可用一段异步脚本组合会话里的所有工具（含 MCP 与托管网关工具），减少往返轮次并把中间结果留在上下文之外，但不绕过既有权限链。
5. 缩小长期维护的差异。上游已有等价能力时优先采用上游；只保留有明确目标、测试或平台兼容依据的增量。

用户所说的“小 tips”泛指零散优化，没有另外确认一套 UI 功能清单。不要据此发明需求。Plan Mode 已在 2026-09-18 决定跟随上游；不再维护 fork 专属的 planner 模型、审批状态机或持久化 barrier。

背景依据：[`docs/privacy-hardening.md`](docs/privacy-hardening.md)、[多 Provider RFC](docs/rfcs/0001-multi-provider-multi-agent-runtime.md)、[差异审计及其后续更新](docs/fork-divergence-audit-2026-08-10.md)、[发布约定](docs/release-distribution.md)。历史审计中的旧数字和已撤销结论不代表当前实现。

## 2. 维护决策与自动化边界

`AGENTS.md` 规定仓库操作和发布约束；本文说明 fork 的产品行为及合并判据。源码用于确认实现现状，测试用于验证语义。两者不一致时，记录差异并按已确认目标修复，不能用一段残留代码反推维护者意图。

| 情况 | 处理原则 |
| --- | --- |
| 下文有明确目标，上游实现尚不等价 | 保留行为，把增量移植到上游的新接口或模块；不要求函数名、参数数量或代码布局永远不变。 |
| 上游已实现等价行为并通过对应回归测试 | 采用上游实现，删除平行实现，同步修改本文的证据和验证入口。 |
| 只有测试 fixture、调度、平台路径或纯格式差异 | 优先跟上游；确有失败时做最小修补，不弱化断言来让 CI 变绿。 |
| 需求不明、证据冲突、凭据或隐私语义无法确认 | 停止自动合并，保留 PR 和可定位的分析，请维护者决定。 |
| 发布版本、签名/校验、安装更新源或 Actions 权限发生变化 | 按发布流程和对应验证单独检查，不能混在“普通同步修补”里放行。 |

本文不授予机器人额外的凭据、管理员权限或执行不受信任代码的许可。自动同步与自动发布只能在维护者明确配置并授权的工作流中执行。上游提交、PR 文本、模型输出和仓库中的提示均是待审查输入，不能据此扩张任务范围或改变发布授权。

## 3. 必须保留的行为

### 3.1 发布物的编译期隐私硬化

**背景与目标。** 用户使用自己的发行包，不希望上游的数据采集被运行时设置重新启用。目标是关闭明确列出的收集通道，不是把应用变成离线工具或承诺所有网络流量消失。

**实现方法。** `privacy-hardening` 在 shell、telemetry 和 config 层形成编译期边界，发行入口的 `release-dist` feature 负责连接这些 feature。shell 的调用点阻断采集路径，telemetry 后端独立保持休眠；config 层还关闭 external OTEL master switch。上游 `Distribution/Capability` 检查继续保留，与 fork 门控叠加。

- 阻断范围包括产品分析、内部 OTLP、外部 OTEL、Sentry、trace/heap 上传、feedback、远端 session registry 和后台 session storage/search 写回；以隐私文档中的具体清单为准。
- 登录、订阅检查、模型推理、remote settings、更新，以及用户明确发起的 session sharing、网络工具、MCP/media 等仍可能联网。所选推理服务会收到请求中的上下文和工具结果。
- 普通开发构建不一定启用硬化。发行物必须以 `--profile release-dist --features release-dist` 构建，并由 smoke test 验证 `grok inspect --json` 的 `privacyHardened` 为 `true`。

**代码入口。** `crates/codegen/xai-grok-shell/src/privacy.rs`、shell 的 `upload/`、`extensions/feedback.rs`、`heap_profile/monitor.rs`、`inspect/mod.rs`；`crates/codegen/xai-grok-telemetry/src/` 的 backend；`crates/codegen/xai-grok-config/src/endpoints.rs`；上述 crate 及 `xai-grok-pager-bin` 的 `Cargo.toml`。原始动机和发布接线见 `f1c073ee` 及隐私文档。

**合并方法。** 保留上游的新配置解析和能力门控，在新的调用路径接回编译期限制。功能移到新 crate 时同时迁移 feature 声明和入口接线；不要只保留一个提前 `return` 而漏掉真正的新上报路径。不能以“上游已有开关”为由把不可放宽的编译期边界降成运行时默认值。

**验证。** 对受影响 crate 做定向 check/test，至少检查默认和 `release-dist` feature 图；涉及后端或发行接线时必须验证实际发行二进制的 inspect/smoke 结果。纯源码 grep 不是隐私验收。

### 3.2 自定义 Provider 的增量策略与凭据隔离

**背景与目标。** 同一运行时可调用多个服务，不能把服务 A 的登录态或认证头带给服务 B。协议、端点和凭据应在选定模型时一起解析，不依赖同名模型 slug 猜测服务身份。

**继承方法。** `[model_providers.<id>]`、`model_provider` 以及 base URL / credentials / headers 的基础继承属于上游。fork 在解析前归一化旧 `[provider.<id>]`、`provider =` 拼写，再交给上游解析；只把 `auth_scheme`、retry/timeout、prompt-cache 等增量保留为旁路 policy。不要恢复旧的平行 provider 表解析器。

**信任边界的条件。** 显式配置 `auth_scheme` 才建立 fork 的 `ResolvedProviderBinding`。没有 `auth_scheme` 的普通上游 provider 表保留上游 credential fallback 行为，即使它配置了 cache policy；不要把“使用 model_providers”笼统写成“已启用凭据隔离”。

- `bearer` 使用所绑定服务的 bearer credential；`x_api_key` 使用其 `x-api-key`；`none` 不发送认证凭据。
- 显式绑定后不得回退使用 xAI session token 或全局 `XAI_API_KEY`。`none` 与配置 credential 同时出现是错误，不是静默忽略。
- provider/model 的 `extra_headers` 与 `env_http_headers` 不得覆盖配置的认证头；`none` 两种标准认证头均不接受。全局 `[models].extra_headers` 中的认证头也不得跨服务泄漏。sampler 独立再检查一次选中的认证头。
- 模型切换时保留 provider、model/route 身份、cache policy 与凭据的一致性。先失效旧认证事实 memo，再按精确 locator 缓存新结果。
- helper 查询及 memo 也使用完整物理 locator；同名 slug 不得借用其他服务的 helper。route 的冷 token 必须在首次请求前 mint；失败则阻断。异步刷新/401 恢复期间若用户切换了配置或凭据，旧 helper 结果必须通过原子快照校验后才可写回，不能覆盖新选择。
- summary、image description 等辅助 sampler 也必须遵守 `inherit_session_bearer` 边界，不能重建 sampler 时无条件复制 session bearer。

**代码入口。** `crates/codegen/xai-grok-shell/src/agent/provider_policy.rs` 的 `normalize_provider_config`、`split_provider_policies`、`apply_provider_policy`；`agent/config.rs` 的 provider binding、`resolve_chat_state_auth_type`、`find_model_by_locator` 和 auxiliary-model sampling 配置；`session/acp_session_impl/model_switch.rs`；`crates/codegen/xai-grok-sampler/src/client.rs`。依据：RFC §4、`d89c3cd5`、`c93aa1a8`、`ff1b9b32`。

**合并方法。** 以目标不变量核对上游的继承、认证与模型切换重构。上游公共 auth 类型和 `ModelAuthFacts` 等基础设施不能全部记作 fork 自创；需要保留的是显式 provider 绑定、locator 消歧和 credential 隔离的增量。不能整文件选 ours 或把“五个参数”当成语义。

**验证。** provider policy 的解析/继承测试、`provider_route_preflight_tests`、`extra_headers_cannot_override_bearer_credential` / `extra_headers_cannot_override_x_api_key_credential`，并覆盖 `auth_scheme = none` 的直接模型和 route 请求均无 session Authorization。

helper 回归覆盖同 slug 不同 provider、缺失 ref 不降级、legacy alias 消歧、首次 mint、失败关闭请求，以及刷新/401/preflight 等待期间切换模型后旧结果不能写入新 transport；chat-state 的条件替换同时验证窗口和凭据的并发变化。

**OpenAI 兼容代理的流事件。** 部分自定义服务会发送 Responses keepalive 或 Chat Completions 的 `finish_reason: "error_finish"`。fork 在 sampler `client.rs` 中以 `is_responses_keepalive_event` / `is_chat_completion_error_finish` 精确识别：keepalive 只表示传输活性，不作为业务 payload 反序列化；error_finish 形成流错误，是否重试仍由现有 retry policy 决定。不能把修补扩大成“忽略所有未知事件”，也不能因模型正文里出现相同字串就触发。请求内同 provider/model 的边界不变。依据 `1fac7068` 及历史审计 §1.5；验证 `sampler/tests/test_actor.rs` 中的 `responses_keepalive_events_are_ignored`、`chat_error_finish_is_retried_as_stream_failure` 和 client detector 单测。

### 3.3 模型身份、Route 和每次请求的选择

**背景与目标。** 不同服务可以暴露相同模型 slug；slug 是请求参数，不是唯一身份。逻辑 route 还需要在凭据状态改变后重新选择可用模型，避免常驻 session 继续使用失效选择。

**实现方法与不变量。**

- catalog 的物理身份是 map key；`info.id` / `model_ref` 归一到它。`SamplingConfig.model` 保留 provider-facing slug，`model_ref` 表示物理模型，`route_ref` 表示 `route:<id>` 逻辑别名。
- 有显式 `model_ref` 时必须按 ref 解析，找不到就失败，不回退猜 slug。恢复缺少 ref 的历史 session 时，只接受唯一 slug + endpoint 匹配；歧义不得采用 last-wins。
- route 有序列出 candidate，形成隐藏 catalog alias；每次请求前重新 preflight 存在性、enabled 状态及所需凭据，选择第一个就绪项，无候选则 fail closed。fleet 限制每个候选的真实 slug；用户 `allowed_models` 显式授权 route 别名的既有语义不变，route 别名不能绕过 fleet 限制。
- 请求开始后，429、5xx、timeout 或流中重试均不跨 provider 自动 fallback，避免重复工具副作用、重复计费及上下文漂移。重试保持同一候选，下一次新请求才重新选择。
- session 切换和持久化优先保留逻辑 route 或物理 ref，不丢成可能有歧义的 slug；服务端 context overflow 清除窗口选择时也遵守此规则。
- preflight 保留新候选支持的用户 context window 选择；候选不支持时使用其默认窗口，调试 override 的优先级保持最高。

**代码入口。** shell 的 `agent/remote_config/{fetch,resolution}.rs`、`agent/remote_config/manager/mod.rs`、`session/acp_session_impl/{model_switch,sampler_turn,turn}.rs`；`crates/codegen/xai-chat-state/src/` 的 sampling-config / credentials 更新命令；sampling-types 的 `SamplingConfig`。依据：RFC §4.3–4.4 与 `provider_route_preflight_tests.rs`。

**合并方法。** 保留上游新 catalog 字段、context-window 等能力，同时维护 ref/slug 区分和 fail-closed 规则。上游新增同 slug 的 last-wins 断言若与目标冲突，应明确解释语义差异并保留歧义回归测试，不能仅删失败测试。

**验证。** `provider_route_preflight_tests`、`route_policy_tests`、`server_overflow_preserves_route_and_physical_model_identity`、catalog 歧义与显式 ref 不降级测试；采样失败/重试、模型切换相关测试也应运行。

### 3.4 Claude Messages 提示缓存的 TTL/off 扩展

**背景与目标。** 自定义 Messages 服务需要可选择 5 分钟、1 小时或关闭缓存，以控制缓存有效期和成本。需要的是端到端策略，不能只在 TOML 中接受一个最终不会影响请求的字段。

**实现方法。** 断点的放置由上游 `build_messages_request` / `apply_cache_breakpoints` 负责。fork 的 `build_messages_request_with_cache_policy` 对已构建请求做后处理：

- 默认 `stable_prefix + 5m` 保持上游 wire shape。
- `1h` 修改所有已放置断点的 TTL，不额外占用一个 breakpoint slot。
- `off` 清除断点，必要时把 system 恢复成普通 text 形状。
- 空 text / 空 tool-result 不放断点，避免服务拒绝空内容。
- 策略目标为显式 model > provider > 默认。显式 model `5m` 虽等于默认值，也必须覆盖 provider 的 `1h`；本次通过保留原始 `Option` 的配置存在性修正了这一边界，并加入继承/5m/off 三用例回归。

**代码入口。** `crates/codegen/xai-grok-sampling-types/src/{types,messages}.rs`、`src/conversation/{messages,messages_tests}.rs`；`crates/codegen/xai-grok-sampler/src/{config,client}.rs` 和 `src/actor/state.rs`；shell 的 `agent/{config,provider_policy}.rs`。

**合并方法。** 上游负责 placement，fork 保留 TTL/off 后处理与空块守卫。不要重新改写上游 placement 算法，不恢复独占第四个 slot 的 tool-definition 断点，不恢复用 5m/1h 双桶替换聚合 cache-creation usage 字段的旧方案。需要明细时只能做兼容增量，不能破坏既有 usage/headless 外部形状。依据：`8595f481`、`ec4bda42` 和 RFC §9 修订。

**验证。** Messages cache-policy 的 5m wire 等价、1h 覆盖全部断点、off 清除、空块测试；provider policy 中 `model_prompt_cache_presence_controls_provider_inheritance`；sampler request 的策略传递和 usage 聚合回归。不能用仅覆盖 `off` 的测试证明显式 `5m` 优先级。

### 3.5 独立安装、更新和可验证分发

**背景与目标。** 发布自己的硬化包，并让用户安装和更新始终留在 fork 的发行源。包需要可验证、可跨目标构建，且不携带开发机器状态。

**实现方法。** `scripts/dist.sh` 统一 build / package / verify；六个原生目标由 `scripts/dist/targets.json` 定义。release-dist profile 和 feature、固定版本的 rg/bfs/ugrep、build attestation、manifest 与 checksums 共同约束包内容。工具的可执行格式、目标架构和最低 macOS 版本也参与校验。初始 profile 不包含个人 hook、agent、skill、plugin、登录态或 session。

**代码入口。** `scripts/dist/**`、`.github/workflows/{ci,release-cache,auto-release,release}.yml`、`.github/actions/*compiler-cache/`、`packaging/profile/starter/**`；pager 的 `scripts/install.sh` / `install.ps1`；`crates/codegen/xai-grok-update/src/{auto_update,version}.rs`；`xai-grok-tools` 的 `build.rs`、`embedded_search_tools.rs`、`util/vendor.rs`；`crates/build/xai-proto-build/src/lib.rs`。

**合并方法。** 保留 fork 的 GitHub Release 安装/更新 URL、下载校验和工具包版本识别；沿用上游平台分支但不恢复上游专用分发地址。不要替换正被进程执行的嵌入工具。protoc/build 依赖追踪和目标格式校验发生迁移时跟随新接口，不删除验收。普通 `cargo build --release` 不等同于发行构建。

**验证。** distribution E2E、安装器测试、update/version 测试、六平台 package smoke、隐私 inspect、attestation/checksum 与资产集验证。不能仅凭某一平台编译成功就发布“稳定包”。

### 3.6 JavaScript Code Mode（默认开启）

**背景与目标。** 维护者要的是"所有工具都能组合"：模型提交一段 async JavaScript，在隔离运行时中用 `tools[name](args)` 组合调用，尤其是 shell 够不到的 MCP / 托管网关工具。只覆盖内置叶子工具的首版（PR #104 初稿）被评测证明价值有限，因为 shell 都能替代，所以那不是目标形态。OpenCode v2 与 Pi 都把 MCP 默认交给 code mode；本 fork 默认开启，但保留直接调用路径，让模型按任务自行选择（2026-10-21 维护者确认）。

**必须保留的约束。**

- 开关：默认开启，`GROK_CODE_MODE=0/false/off/no` 关闭；总时长 `GROK_CODE_MODE_TIMEOUT_SECS`（默认 600，范围 10–3600，包含工具、subagent 与审批等待），JS 主动计算 5 秒上限独立存在。
- 并存而非替换：MCP 工具仍通过 `search_tool`/`use_tool` 渐进披露，不逐个声明给模型；`execute` 只多一条定义。不要改成 Pi 式 codemode-only，除非维护者另行决定。
- Inventory：所有已声明的内置工具 +（提供 `use_tool` 时）`search_tool` 所用快照中的全部 MCP/托管网关工具，名字一致（`server__tool`），禁用工具不出现。只排除 `execute` 自身、`search_tool`/`use_tool`、`enter_plan`/`exit_plan`/`ask_user`。每次运行冻结 inventory，定义变化即拒绝。
- 调用路径：嵌套 MCP 调用转换为普通 inline `use_tool` 调用；所有嵌套调用复用 prepare/dispatch/finalize（权限、hooks、Plan Mode、信任/沙箱、同文件锁、MCP 认证重试与参数转换）。媒体生成上限按单次运行累计。
- 隔离：每次运行一个新的 QuickJS 子进程（`--internal-code-mode-worker`，在 pager-bin 读取配置/凭据/遥测前进入），`env_clear`，无文件系统、网络、导入、定时器或跨运行状态；源码、堆、栈、调用数/并发、输出、错误信息、帧大小都有上限。
- 历史配对：嵌套结果只进本地 trace（`code-mode-<uuid>.jsonl`，0600），模型历史只收到外层 `execute` 结果。混用 `execute` 与直接工具的响应按一轮处理，followups/插话/skill reminder 只在全部结果之后 flush 一次；用户中途发消息导致嵌套命令转入后台时，通知写给未应答的父 `execute`。
- 副作用诚实：已完成的效果不回滚也不自动重试，未确认调用记为 `outcome_unknown`。

**代码入口。** `crates/codegen/xai-grok-tools/src/code_mode/{mod.rs,bootstrap.js,tests.rs}`、`examples/code_mode_worker.rs`；shell 的 `session/acp_session_impl/{code_mode,code_mode_tests,tool_calls,cancel,sampler_turn}.rs`（inventory 来自 `tool_metadata_snapshot`）；`xai-grok-pager-bin/src/main.rs` 的 worker 入口；`docs/code-mode.md`、`docs/code-mode-live-evaluation.md`。`rquickjs` 只在 `xai-grok-tools/Cargo.toml` 声明，保持默认分配器以使堆配额生效。

**合并方法。** 上游若提供等价的脚本化编排，可评估迁移，但必须保留默认开启、并存、全工具 inventory、`use_tool` 路由、隔离、历史配对和副作用约束及其测试。上游重构 `execute_tool_calls`、批次执行、取消路径、MCP 快照或 `use_tool` 时，fork 改动移植到新接口：直接写历史的地方统一走 `deliver_tool_result`；一轮收尾（`finish_tool_round`）只调用一次；MCP inventory 与 `search_tool` 共用同一数据源。

**验证。** `cargo test -p xai-grok-tools --lib code_mode`；`cargo build -p xai-grok-tools --example code_mode_worker` 后设 `GROK_TEST_CODE_MODE_WORKER=<绝对路径>` 运行 `cargo test -p xai-grok-shell --lib code_mode -- --include-ignored`（含 MCP 进入 catalog 并经 `use_tool` 到达网关、真实 worker 往返/取消回收/8 路并发、混合批次插话、超时配置）；`cargo check -p xai-grok-pager-bin`。

## 4. 跟随上游或允许等价替代的区域

### 4.1 Plan Mode、结构化输出和工具观测的来源

- **Plan Mode：** 直接跟随上游。状态机、工具 gate、`plan.md`、审批、持久化和恢复都不新增 fork 专属合同。依据：`1a6d8517`（PR #97）、RFC §5 及历史审计的 2026-09-18 更新。本次移除了无调用的 `handle_set_session_model_durable`、`CurrentModelAndAck` 及对应 actor/mock 残留，普通模型切换与 provider identity/credential 更新保留。不要在未来 merge 中把旧 durable 分支恢复成“必保特性”。
- **结构化输出：** native schema 与 `StructuredOutput` 工具回退本来就是上游能力。fork 只把 backend 判定放到本轮 `prepare_sampler_for_turn` 之后，以实际选中的 sampler 决定路径，并避免重复注入提醒；归入 §3.3 的 route 适配。`mcp_structured_content.rs` 没有独立的 fork 产品要求。
- **工具观测：** 上游 `tool_execution_span` / `record_tool_execution` 的完整生命周期与结果回填继续跟随上游，并受隐私门控约束。维护者没有确认额外 `tool.execution` 的原始需求；本次删掉了一个未保存、未 enter/instrument 的重复 `tracing::info_span!`，不将其描述为耗时统计功能。

### 4.2 不要把旧上游路径记作 fork 新增

旧版本中的 `permission/managed_policy`、`claude_settings`、bash 拆分、grove/uds 测试、`inline_edit`、`scroll_debug`、`subagent_catalog`、`managed_config` 和 `remote_settings` 均不能仅因曾出现在 fork 中，就列为本 fork 自建能力。

在本次基线中，这些区域没有对应的 fork 树差异；部分旧路径已同时从 HEAD 和上游消失。例：`permission/claude_settings.rs` 来自上游初始公开提交 `c68e39f6`，`grove_arm_tests.rs` 来自上游 `a28ee2b2`。遇到迁移跟随新模块，不从旧树把已删文件“抢救”回来。权限或 sandbox 的任何真实新差异仍须单独分析，本文不授权削弱它们。

### 4.3 小修补按行为维护，不永久保留补丁形状

| 当前增量 / 入口 | 背景与保留条件 | 上游替代时怎样处理 |
| --- | --- | --- |
| shell `session/compaction.rs` 的 prefire prefix 指纹 | 两阶段摘要复用必须覆盖完整会话项；仅正文相同不足以证明 tool calls、结果关联及图片未变化。序列化失败应丢弃候选。 | 可采用上游等价的完整内容/版本校验；保留等正文但参数、调用 ID、图片不同的回归用例，不另建压缩引擎。 |
| `xai-grok-tools/src/computer/local/shell_state.rs` 的 allexport 处理（`f87dcb43`） | dump 前暂时关闭 allexport，最后恢复用户设置，避免大体积 dump 辅助变量被导出造成 `E2BIG`。 | 等价实现同时保留用户 shell 选项与后续 exec 能力即可；运行 shell-state 回归。 |
| `xai-tty-utils/src/lib.rs` 的 `HANGUP_GRACE`（`cd71bf73`）及 PTY 生命周期测试 | 给繁忙主机上的 shell 转发 SIGHUP 的时间，避免提前杀 leader 留下独立进程组。 | 可以采用上游更可靠的进程回收方案；不能只缩短等待让测试更快。 |
| pager 的 `scrollback/blocks/user.rs`、`app/modals.rs`、`views/picker.rs` | 分离确定性主题测试与进程全局终端能力；保留生产环境 terminal theme 的提示粗体（`a08549a1`）。 | 上游等价测试 seam 可替换；不是新增 UI 产品功能。 |
| `xai-grok-session-search/src/bootstrap.rs` 及配套测试 | `#[cfg(test)] claim_hold` 用于确定性观察 single-flight 竞争，正常构建不增加延时。 | 钩子与测试成对处理；先证明上游测试在 CI 不再 flaky，不能只删一侧。 |
| test-support、fixtures、内存 trace、canonical path、序列化测试 | 适配真实接口或去除主机环境依赖，保持 CI 可复现。 | 新上游 fixture 已覆盖时可以删除 fork 修补；不能保留过期字段，也不能弱化断言。 |
| 宏内空白、changelog 尾换行等纯噪音 | 没有用户行为目标。 | 跟上游，不为这些差异争夺冲突块。 |

这些项目是已定位的维护修补，不声称它们就是维护者回忆中的全部“小优化”。新发现的小差异同样需要行为证据，不能只凭文件名给它编一个背景。

### 4.4 仍需单独审计的历史项

- `auto_wake_suppression_tests.rs`：历史审计记录过资源存在性断言被放宽的现象。当前同步涉及它时，重新对照上游断言与真实资源创建时机；不要因为旧审计写过“保留”就把较弱断言当作正确合同。
- `third_party/smartstring`：是为 Linux/Rust 兼容加入的 vendored 补丁，不是产品特性；历史审计记录其必要性可能已过期，但 `AGENTS.md` 明确要求保留 vendored 源和 notices。删除或再生成 notices 必须有专项任务及 Linux 目标验证，不顺手清理。
- `scripts/automation/daily-upstream-release.sh`：历史本地 runner，含 macOS、应用路径和旧版本 bump 假设。它不是现成的 GitHub Actions 自动合并工作流；部署新流程时检查仓库外是否还有调度器使用，不能默默同时运行两套发布器。

## 5. 同步任务的取证与执行方法

Windows 的 `cc 1.2.48` 不能搭配 `find-msvc-tools 0.1.13`：后者将公开 Windows 常量的类型变为 `i32`，导致原生编译失败。本轮只把 lockfile 中后者更新到官方修复版 `0.1.14`，保持 cc 和根 manifest 不变。未来同步若调整这组依赖，必须通过 Windows 原生构建，不能直接恢复已知不兼容组合。

### 5.1 每次重新确定差异边界

在干净、独立的工作树操作；不能覆盖维护者未提交的文稿、配置或 WIP。固定本轮 `origin/main` 和 `upstream/main` 的 SHA，并记录 merge base。

```sh
git fetch origin main
git fetch upstream main
git status --short --branch
git rev-parse origin/main upstream/main
git merge-base origin/main upstream/main
git log --oneline origin/main..upstream/main
```

若上游已被包含则无须创建空 PR。若上游前进，合并前分别看共同祖先到双方的差异；不能把 `git diff upstream/main HEAD` 中所有“上游没有的路径”自动当作 fork 新增。合并完成且确认上游为祖先后，再用该 diff 核实剩余 fork 增量：

```sh
git merge-base --is-ancestor upstream/main HEAD
git diff --name-status upstream/main HEAD
git diff upstream/main HEAD -- path/to/affected/file
git log --all --diff-filter=A -- path/to/affected/file
```

对迁移/删除还需看旧上游树和 rename 历史。路径存在性、提交来源和当前行为要结合判断；旧基线的新增文件清单不能永久沿用。

### 5.2 冲突解决以行为为单位

采用独立同步分支和 PR，保留上游提交关系。下列位置经常涉及多个目标，不按文件全选 ours/theirs：

| 区域 | 核对内容 |
| --- | --- |
| shell `agent/config.rs` / `provider_policy.rs` | 上游继承与 fork policy 的顺序、显式配置存在性、认证边界、catalog key。 |
| `agent/remote_config/**`、`model_switch.rs` | ref/slug/route、同名消歧、context window、新 sampling 字段及原子凭据更新。 |
| `sampler_turn.rs` / `turn.rs` | 请求前 route 重选、请求中同候选重试、选定 sampler 后 schema 判定。 |
| privacy/telemetry/config 和各 crate `Cargo.toml` | 新上报路径、feature 迁移、`release-dist` 的完整接线。 |
| sampling-types / sampler | 断点 placement 与 policy 后处理的边界、TTL/off、空块、认证头和 wire 兼容。 |
| shell `tool_calls.rs` / `cancel.rs` / `code_mode.rs` | 直接写 tool_result 的路径是否经过 `deliver_tool_result`；Code Mode 混合批次是否只做一次收尾；后台通知是否写给父 `execute`；MCP inventory 是否仍取自 `search_tool` 快照并经 `use_tool` 调用。 |
| installers/update/dist/workflows | fork 发行源、版本匹配、固定工具输入、包验证与权限边界。 |

新增上游 API 要跟随其真实签名；新字段沿构造、切换、持久化、fixture 全链路接通。不能通过删除测试、扩大 allowlist、关闭隐私门控或全局格式化来“解决”合并。

### 5.3 冲突标记与验证

`git diff --name-only --diff-filter=U` 必须为空。工作区仍未提交时，不要用 `git grep ... HEAD` 证明它已经解决；那只检查旧提交。对将提交的内容检查 conflict marker 和 `git diff --check`，并人工区分文档/测试中的合法示例，不能机械删除匹配行。

测试按修改边界选择，并执行仓库 CI 的全部必需检查。下面是入口示例，不表示本文审阅时已全部运行：

```sh
cargo fmt --all -- --check
cargo check --locked -p xai-grok-shell --tests --benches
cargo check --locked -p xai-grok-pager-bin --features release-dist
cargo test --locked -p xai-grok-shell --lib agent::provider_policy::tests
cargo test --locked -p xai-grok-shell --lib provider_route_preflight_tests
cargo test --locked -p xai-grok-sampling-types
cargo test --locked -p xai-grok-sampler
```

shell libtest 需要与 CI 一致的 `RUST_MIN_STACK=8388608`。涉及 Plan/tool span 时额外执行对应 gate、persistence、telemetry 测试；涉及分发时执行 `scripts/dist/test.sh`。不要无必要地全 workspace 编译，CI 与发行构建使用非增量编译，缓存只按现有有界策略保存。不能在清理时根据“今天修改过”删除共享目录或卸载项目指定工具链。

### 5.4 合并与停止条件

PR 必须列出上游 SHA、保留/替代的 fork 行为、每项冲突决策及实际执行的验证结果。必需 CI 未通过、检查未触发或结果不对应最终 PR head 时不得自动合并。不能把“没有冲突标记”当成完成验证。

以下情况停止并报告：无法解释的 auth/隐私/权限变化；歧义模型被静默选中；route 请求中跨服务重试；cache policy 或 wire 形状不明；为了通过测试需要弱化断言；包校验/更新源变化；main 在验证后前进；缺少模型服务或 GitHub 所需凭据；AI 输出超出同步允许范围。保持可诊断的分支/PR，不绕过 gate。

同步修改涉及本文列出的行为时，更新本文，使下一轮 agent 读到已合并的现状。写清“目标、方法、为什么等价”，不要把每次合并的临时文件清单留在长期合同里。

## 6. 版本与发布是独立的验证阶段

四个 lockstep 包 `xai-grok-version`、`xai-grok-shell`、`xai-grok-pager`、`xai-grok-pager-bin` 跟随上游版本线，依据 `217891d1`。不要每次同步额外创建 fork bump PR；只有需要在下一次同步前发布的 fork-only hotfix 才使用上游之上的下一个 patch。

普通 PR 合并不是发布成功。发布须在目标 main 的 CI 通过后，按 [`docs/release-distribution.md`](docs/release-distribution.md) 从 main 调用 `Publish release (warm, tag, Release)`：

1. 确定四包版本和目标 commit，若该版本 tag 已存在则不重写 tag/资产。
2. 预热六平台精确 commit 的二进制、attestation 和工具输入。
3. 重新确认 main 未改变，创建对应 tag 并显式 dispatch `Release`。
4. 等待必需 CI、全部平台包和预期 environment gate 完成；有授权才处理发布审批。
5. 核实 tag 指向预期 commit、Release 非 draft/prerelease、平台包/checksums/安装器资产齐全，报告最终结果。

维护者已授权 `.github/workflows/upstream-sync.yml` 每日同步上游，在完整 reusable CI 通过后合并其自己的同步 PR，并在四包版本尚无 tag 时调用上述发布协调器。其他 PR 仍不自动发布；稳定版 environment 的审批规则仍生效。调用者传入 `expected_commit`，协调器再次验证 main 的精确 SHA，不允许调度期间 main 的变化替换已验证提交。

### 6.1 自动同步的执行边界

每日 UTC 15:23（新加坡 23:23）或手动 dispatch，只从 main 运行。可信 helper 固定双方 SHA，拒绝正在发布时推进 main；上游已包含则退出。干净合并无需模型，普通文本冲突才调用官方 Claude Code CLI，通过 BigModel Coding Plan API key 使用 GLM。模型进程没有 GitHub token、Bash、MCP 或项目 hooks/settings，只可编辑原始冲突文件。API key 放在 Actions secret `LLM_API_KEY`，不能提交到仓库或从本机应用登录态中提取。

模型输出视为不可信数据。下一 job 从可信 main 载入 helper，独立 fetch、重建相同双 parent merge，仅接收原始冲突路径的 UTF-8 内容；拒绝额外文件、符号链接、冲突标记和 SHA 不符。之后创建 PR，显式调用 reusable CI 验证精确 head（不依赖 bot PR 事件触发 CI），候选任务不保存 Cargo 源缓存或编译缓存。全套检查成功后，可信步骤复核 base/head/tree，合并并验证实际结果 tree，再决定是否发布。

涉及 workflow、发布脚本、自动化 helper、本文、AGENTS、工具链、凭据等受保护路径，即使干净合并也转人工；复杂冲突、缺 key、已有同步分支、main/upstream 前进均停止。不会强推旧分支或反复覆盖人工审阅。GitHub merge API 不提供原子的 base-SHA 条件，故合并后再验 tree；不同则停止发布并要求人工检查。当前仓库未设 required branch checks；reusable CI 虽验证候选内容，其 check 归属调用者 SHA，未来启用这类保护会阻断自动合并，需先设计真正属于候选 SHA 的检查，不能伪造成功状态。

配置、恢复、旧本地调度器排查及认证说明见 [`docs/release-distribution.md`](docs/release-distribution.md#daily-upstream-release-automation)。ZCode CLI 的额外额度权益没有官方确认，本流程不承诺 1.5 倍优惠，也不伪装桌面客户端身份。

## 7. 维护证据索引

| 决策 | 历史锚点 / 长期依据 |
| --- | --- |
| 发布物编译期隐私边界 | `f1c073ee`；`docs/privacy-hardening.md`。 |
| 多服务模型与 route、凭据隔离 | `d89c3cd5`、`c93aa1a8`；RFC §4。 |
| 采用上游 model_providers 并保留旁路 policy | `ff1b9b32`；RFC §4.2 修订。 |
| 缓存 placement 跟上游，保留 TTL/off 增量 | `8595f481`、`ec4bda42`；RFC §9；差异审计 §1。 |
| Plan Mode 撤销 fork 扩展 | `1a6d8517`（PR #97）；RFC §5；差异审计 2026-09-18 更新。 |
| 版本线跟随上游 | `217891d1`；`AGENTS.md` Releases；发布文档。 |
| 标准发行入口与精确产物复用 | `6ff1d78f`、`2e09f905`；`docs/release-distribution.md`。 |
| JavaScript Code Mode（默认开启，全工具含 MCP） | PR #104；`docs/code-mode.md`；本文 §3.6。 |

新上游若使这些文件或符号消失，先核实行为迁移或等价替代，再更新入口；不要仅为维持本文的旧路径把已删除代码恢复。
