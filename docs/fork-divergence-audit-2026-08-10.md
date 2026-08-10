# Fork 差异评估（2026-08-10）

本文评估 `zhangtyzzz/grok-build` 相对 `xai-org/grok-build` 的自有差异，逐项给出
**保留 / 删除 / 改为跟随上游** 三选一的结论，并附可追溯依据。

判断口径只有一条：**上游已有对应能力的，跟随上游最新实现，不保留并行实现；只有上游
确实没有、而我们确实需要的才留下。** 「感觉还有用」不作为依据。

---

## 0. 差异边界怎么划的（为什么没把上游的更新算成我们的）

审计当天 `upstream/main` 的 HEAD 已经被当日同步合入我们的 `main`：

```
$ git merge-base main upstream/main
75e73f3d6ac0350d211f12ae7d57c2c0aad72576
$ git rev-parse upstream/main
75e73f3d6ac0350d211f12ae7d57c2c0aad72576
$ git rev-list --count main..upstream/main
0
```

merge-base 恰好等于 `upstream/main` 的 HEAD，且上游没有任何未合入的提交。因此
`git diff upstream/main..HEAD` **只包含我们自己的改动**，不含任何上游更新 —— 这是本文
所有分区统计和逐项结论的唯一数据来源。

我们自有提交 137 个（其中非 merge 提交 65 个）。

### 审计起点的差异规模

| 类别 | 文件数 | 增 | 删 |
|---|---|---|---|
| **修改上游已有文件**（同步冲突成本的唯一来源） | 164 | +8795 | −1773 |
| 　└ 其中测试类 | 38 | +2051 | −290 |
| 　└ 其中非测试 | 126 | +6744 | −1483 |
| **新增自有文件**（上游不存在该路径，同步零冲突） | 58 | +11195 | — |
| 合计 | 222 | +19990 | −1773 |

一个关键事实：上游树里**根本没有** `.github/`、`scripts/`、`packaging/`、`AGENTS.md`：

```
$ git ls-tree -r upstream/main --name-only | grep -E '^\.github|^scripts|^packaging|^AGENTS'
(无输出)
```

所以我们那套 CI、发布、分发流水线（约 5700 行）虽然体量最大，**每天同步的成本是 0**，
上游永远不会碰这些路径。真正在涨的成本全部来自那 164 个被我们改过的上游文件。这一点
决定了本轮的优先级：优先削减「对上游文件的修改」，而不是按行数排序。

---

## 1. `xai-grok-sampling-types` / `xai-grok-sampler` —— Anthropic Messages 缓存断点

这是本轮最大的一处「上游已有对应能力」。

### 1.1 上游已自建同样的能力（依据）

上游提交 **`dd04f397`（2026-07-30）** 引入了完整的会话前缀缓存断点实现，包括
`mark_message_cache_breakpoint`、`apply_cache_breakpoints`，以及 system prefix + 当前
tip + 上一个 tip 的放置策略：

```
$ git log --oneline upstream/main -- crates/codegen/xai-grok-sampling-types/src/conversation/messages.rs
dd04f397 Synced from monorepo
```

我们自己那套是 2026-07-20 / 07-22 做的（`d434aea4` Cache the conversation prefix、
`892c56af`、`7e75903c`、`6b37853f`、`795c52f8`），比上游早 8~10 天。当日同步已经把上游
版本合了进来，于是我们的 fork 变成「上游函数的一份改写副本 + 三项叠加」。

### 1.2 逐项结论

| 项 | 涉及位置 | 结论 | 依据 |
|---|---|---|---|
| 断点**放置**逻辑（system + 当前 tip + 上一个 tip） | `conversation/messages.rs` `apply_cache_breakpoints` | **改为跟随上游** | 上游 `dd04f397` 已实现，语义与我们一致 |
| tool-definition 断点（在最后一个工具定义上打断点） | `messages.rs` `ToolParam::cache_control`、builder 中的 `enumerate()` 分支 | **删除** | ① 上游 `apply_cache_breakpoints` 注释明确写 “The fourth slot stays free: a gateway that turns on automatic caching takes it, and five is rejected outright”，我们占用了上游预留的第四个 slot；② Anthropic 按 `tools → system → messages` 整体前缀缓存，system 断点已经覆盖其前面的全部工具定义，单独再打一个只多产生一次 cache write，没有 read 收益；③ 违背我们自己 `docs/rfcs/0001` §9.2 原始设计（「第一版只有一个 explicit breakpoint」，理由与②相同） |
| cache-write 按 TTL 拆成 5m/1h 双桶 | `TokenUsage`、`UsageTotals`、`ResponseUsage`、`PromptResponseMeta`、headless 投影，共 4 个 crate | **删除，跟随上游单一聚合字段** | 见 1.3 |
| 5m / 1h / off cache policy | `PromptCachePolicy` / `PromptCacheMode` / `PromptCacheTtl`、`CacheControl.ttl` | **保留**（实现方式重写） | 上游 `CacheControl` 只有 `type` 字段（`git show upstream/main:crates/codegen/xai-grok-sampling-types/src/messages.rs` 第 92-103 行），无 TTL 概念；我们这条已从 provider/model 配置端到端接通（`agent/config.rs:5796` 进 `SamplingConfig`，`agent/config.rs:6073-6138` 有 TOML 级测试） |
| 空块守卫（不在空 Text / 空 ToolResult 上打断点） | `mark_message_cache_breakpoint` | **保留** | 上游无此判断；且可达：builder 里 `ConversationItem::ToolResult` 分支无条件构造 `ToolResultContent::Text(t.content)`，工具无输出时即为空块，若它是本轮 tip，上游逻辑会把断点打在空块上，被 API 以 “text content blocks must be non-empty” 拒掉 |

### 1.3 为什么 TTL 双桶必须删（这是本轮证据最硬的一项）

上游本来就有单一聚合字段 `TokenUsage::cache_creation_prompt_tokens` /
`UsageTotals::cache_creation_tokens`。我们把它**替换**成了
`cache_write_5m_input_tokens` + `cache_write_1h_input_tokens`，横跨 4 个 crate。三条硬依据：

1. **丢数据。** 双桶只从 Anthropic 可选的 `usage.cache_creation` 明细对象取值
   （`sampler/src/stream/messages.rs`）。任何只上报文档化聚合字段
   `cache_creation_input_tokens` 的 provider，在我们这里 cache-write 会记成 **0**，而上游
   能报出真实值。
2. **fork 自己的测试把这个回归写进了断言。** 改动前
   `sampler/src/stream/messages_tests.rs::prompt_tokens_sums_all_three_anthropic_buckets`
   原本断言 `cache_creation_prompt_tokens == 200`，被改成断言两个 TTL 桶 `== 0`；
   `message_delta_cache_fields_override_message_start` 还加了注释
   “Without per-TTL detail the aggregate write only shows up in `prompt_tokens`”。
3. **破坏上游标注为冻结的外部兼容形状。** `extensions/notification.rs` 里上游把 headless
   result 的字段注为 “the frozen headless shape” / “reduced external-compat schema”，我们
   把 `cache_creation_input_tokens` / `cacheCreationInputTokens` 改名成了
   `cacheWrite5mInputTokens` 等。
4. 另有自承的同步隐患：`pager/src/headless/reducer/messages/usage.rs` 里我们自己写了
   “If an upstream sync ever makes the projection emit the aggregate *alongside* the TTL
   buckets, this sum would double-count it”。

需要 per-TTL 明细时，正确做法是作为**新增**字段提给上游，而不是替换既有字段。

### 1.4 TTL policy 的实现方式改了（这是本轮降低同步成本的关键手法）

原实现给上游的 `apply_cache_breakpoints` / `mark_message_cache_breakpoint` **加了参数**，
等于长期改写上游刚刚重写过、且仍在演进的两个函数签名 —— 这是整个 fork 里冲突概率最高的
一处编辑。

改后：上游放置逻辑原样不动，policy 作为**后处理**施加在已构建好的请求上
（`for_each_cache_control` + `build_messages_request_with_cache_policy`）。效果：

- `5m`（默认）：新测试 `cache_policy_five_minutes_reproduces_the_upstream_wire_shape`
  断言产出与上游 `build_messages_request` **逐字节相同**；
- `1h`：把上游已放好的断点统一改写为 `ttl: "1h"`，不增不减断点数量；
- `off`：清空断点，并把单块 system 收敛回上游的裸 text 形状。

`xai-grok-sampling-types` 对上游文件的差异因此从 +750/−39 降到 +392/−5，其中
`conversation/messages.rs` 从 +121/−30 降到只剩空块守卫的 +37/−4。

### 1.5 上游确实没有、保留的其余 sampler 项

| 项 | 结论 | 依据 |
|---|---|---|
| `is_responses_keepalive_event` / `is_chat_completion_error_finish`（`1fac7068`） | **保留** | `git show upstream/main:.../client.rs \| grep -i "keepalive\|error_finish"` 无输出；这两类是 OpenAI 兼容代理的非标准事件，多 provider 场景才会遇到 |
| `extra_headers` 不得覆盖已配置认证头（`c93aa1a8`） | **保留** | 上游 client.rs:558-567 原样透传全部 extra header，注释称其为 proxy-auth 的注入点；单 provider 下没有风险，多 provider 下 model switch 会把 A 的头带进 B 的请求。我们的检查只在 `api_key.is_some()` 且头名等于所配置 scheme 的认证头时报错，上游那个「无 api_key、用 extra_headers 供 proxy 认证」的用法仍然可用 |
| `model_ref` / `route_ref` | **保留** | 上游 `SamplerConfig` 只有 `pub model: String`；`model` 是路由 slug 不是身份，两个 provider 可以暴露同名 slug |

---

## 2. 零价值差异（纯噪音，已全部还原）

这些改动不改变任何编译产物，只在每次同步时制造冲突面：

| 项 | 文件数 | 结论 | 依据 |
|---|---|---|---|
| 上游 changelog 与用户指南被删掉行尾空行 | 10 | **删除（还原上游）** | `git diff` 显示 10 个文件各只有 `+0 −1`，内容为末尾空行 |
| pager 三个测试文件里 `matches!` 宏体缩进被改成 rustfmt 不会产出的形状 | 3 | **删除（还原上游）** | `git diff -w` 后无差异，即纯空白改动；rustfmt 不进入宏体，所以 `cargo fmt --check` 查不出来 |
| `turn.rs` / `updates.rs` 宏体被写坏（`error = % e`、`target : ...`、`subagent_id = % subagent_id`、一个被重排成怪形状的 `json!` 块） | 2 | **删除（还原上游写法）** | 同上，宏内格式 rustfmt 不管；来源应是某次冲突解决或自动修复工具 |

共 15 个上游文件回到上游原文。

---

## 3. `xai-grok-shell` —— 多 provider 多 agent 运行时与 privacy

本轮**保留**，但需要说明为什么需要，以及它就是剩余同步成本的主体（75 个文件
+5934/−1236）。

| 项 | 结论 | 依据 |
|---|---|---|
| provider / model_route / route preflight / 凭据原子替换（`ReplaceSamplingConfigAndCredentials`、`UpdateCredentialsIfSamplingConfigMatches`、`preflight_active_route_for_request`） | **保留** | 上游只有单一 provider 模型：`SamplerConfig` 无 `model_ref`/`route_ref`，chat-state 无这两个命令。多 provider 是 fork 的立项理由，上游不会有 |
| `privacy-hardening`（`f1c073ee`，`src/privacy.rs` + 20 余处 `is_hardened_build()` 门禁） | **保留** | 上游 `xai-grok-shell/Cargo.toml` 的 `[features]` 只有 `dhat-heap` / `test-support` / `local-workspace` / `default-bazel`，无 privacy feature；`crates/codegen/xai-grok-shell/src/privacy.rs` 在 `upstream/main` 中不存在。我们要对外分发二进制，必须能在编译期关掉遥测与上传，且不可被环境变量或远端设置放宽 |

---

## 4. `xai-grok-tools` —— plan 文件防护

| 项 | 结论 | 依据 |
|---|---|---|
| `computer/protected_plan_file.rs`（`plan.md` 的 symlink / TOCTOU 防护，`openat(2)` + `O_NOFOLLOW` + `renameat(2)`） | **保留** | Plan Mode 会对唯一一个文件自动放行写入，该文件因此成为安全边界。上游 `exit_plan_mode/mod.rs` 中 grep `symlink\|nofollow\|canonical\|protected` 无任何命中，即上游无等价防护。已接入 6 处调用点（tools / shell / pager）。**建议提交上游** |
| `reminders/task_completion.rs` 新增 `is_reported()` 只读访问器 | 见 §7 暂缓项 | 这 2 行是为测试断言加的，本身无害，但它服务的那处断言掩盖了一个未定位的行为分歧 |

---

## 5. `.github/` / `scripts/` / `packaging/` / 安装脚本

| 项 | 结论 | 依据 |
|---|---|---|
| `ci.yml` / `release.yml` / `auto-release.yml` / `release-cache.yml`、两个 compiler-cache composite action、`scripts/ci/sccache-fallback.rs` | **保留** | 上游无 `.github/`（见 §0）。全部在用：`ci.yml:58,119,174,222` 引用两个 action，`setup-compiler-cache/action.yml:66` 编译 `sccache-fallback.rs` |
| `scripts/dist/` 全套 14 个文件 + `scripts/dist.sh` | **保留** | 逐文件扫过引用，**没有死文件**；每个文件的引用链都能上溯到某个 workflow：`release.yml:234/239/245/439/449` 与 `release-cache.yml:150` 调 `dist.sh`，`ci.yml` 调 `prepare-release-tools.sh` 与 `dist/test.sh` |
| `packaging/profile/starter/` | **保留** | 上游无 `packaging/`；随分发一起发的初始 profile |
| `crates/codegen/xai-grok-pager/scripts/install.sh` / `install.ps1`（`1003d4d0`、`3753e56f`） | **保留** | 把分发源从 `x.ai/cli` + GCS 换成我们自己的 GitHub Releases，并加了 sha256 校验。这是 fork 存在的目的之一，上游不会有。代价是两个上游文件上长期 +108/−54 的冲突面，无法避免 |
| `scripts/automation/daily-upstream-release.sh` + 其 schema | 见 §7 暂缓项 | 没有任何 workflow 引用它；且它硬编码 `/opt/homebrew/bin`、`~/Library/Application Support`、`osascript`、以及 `/Applications/ChatGPT.app/Contents/Resources/codex` —— 是个 macOS 本地 runner，与「同步跑在 GitHub Actions 里」的实际链路不符 |

---

## 6. `docs/` —— 本轮同步修订

| 项 | 结论 | 说明 |
|---|---|---|
| `docs/rfcs/0001-multi-provider-multi-agent-runtime.md` | **保留 + 修订** | §9.2 补记：断点放置已交回上游，tool-definition 断点已删（它与本节原始设计冲突）；§9.3 补记：cache-write 回到上游聚合字段，并记录了删除双桶的原因 |
| `pager/docs/user-guide/11-custom-models.md` | **保留 + 修订** | 原文声称「断点打在最后一个工具定义、system prompt 和最后一条消息上」并描述 TTL 分桶统计，与实现不再一致，已改为：只有生命周期可配，放置跟随上游；并说明 system 断点已覆盖其前的 tools |
| `pager/docs/user-guide/14-headless-mode.md` | **删除（还原上游）** | 它记录的是被删掉的 TTL 桶字段，并自承「只返回聚合计数的 provider 无法填充 TTL 桶」 |
| `docs/ARCHITECTURE.md`、`docs/privacy-hardening.md`、`docs/release-distribution.md` | **保留** | 上游无 `docs/` 下这些文件；对应的是保留下来的能力 |
| `AGENTS.md` | **保留** | 上游无此文件（上游只有 `CONTRIBUTING.md`） |

---

## 7. 本轮决定暂缓的项

以下每项都写清「为什么这轮不动」和「后面该怎么动」。

### 7.1 `third_party/smartstring`（20 个文件 / 3249 行）—— 有证据可删，但被约束挡住

**证据表明这个 vendor 已经过期。** 它的存在理由写在 `.cargo/config.toml` 里：
「smartstring 1.0.1 mis-detects the unstable allocator API on **Linux/Rust 1.92**」。而
`rust-toolchain.toml` 现在 pin 的是 **1.94.0**。实测（临时关掉 `[patch.crates-io]`，改用
crates.io 版本）：

```
$ cargo check -p smartstring --target x86_64-unknown-linux-gnu     → Finished，无错误
$ cargo check -p smartstring --target aarch64-unknown-linux-gnu    → Finished，无错误
$ cargo check -p xai-workflow --target x86_64-unknown-linux-gnu    → Finished，无错误
$ cargo check -p xai-workflow --target aarch64-unknown-linux-gnu   → Finished，无错误
```

两个 Linux 发布目标（`rust-toolchain.toml` 里声明的那两个）都干净编过。整个 vendor 实际
只改了 build.rs 的 13 行（把 `has_allocator` / `needs_allocator_feature` 两个 cfg 钉死为
关闭），依赖链是 `rhai → xai-workflow → xai-grok-shell`。

**为什么这轮不动：** 任务约束明确写了「third_party 是 vendored，这条别踩」，`AGENTS.md`
也要求不得改动 vendored 组件。删除整棵 vendored 树属于踩线，需要你显式解除这条约束。
另外它的同步成本本来就是 0（上游没有这个路径），所以留着并不推高每日成本。

**后面该怎么动：** 解除约束后，一次提交内完成 ① 删掉 `.cargo/config.toml` 的
`[patch.crates-io]` 段；② `git rm -r third_party/smartstring`；③ 把
`third_party/NOTICE`、`third_party/README.md` 还原到上游原文（我们只往里加了 smartstring
条目）；④ 让 Cargo.lock 把 smartstring 换回 registry 源（会多两行 `source` / `checksum`）；
⑤ 在 CI 上对两个 Linux 目标各跑一次 release 构建确认。

### 7.2 `auto_wake_suppression_tests.rs` —— 被削弱的上游断言掩盖了一处未定位的行为分歧

`git show upstream/main` 的原始断言是 `.is_none()`，我们改成了
`.is_none_or(|reported| !reported.is_reported(...))`，即从「该资源必须完全不存在」放宽成
「存在也行，只要没标记这个 id」。

**这不是消除 flaky。** 把该文件还原成上游版本后，两个测试在我们的代码上稳定失败：

```
cancel_barrier_rejects_task_completion_wake_without_reporting_it
  → declined admission must not report before user re-engagement
task_completion_wake_is_admitted_without_cancel_barrier
  → queue acceptance alone must not mark the completion reported
```

即我们树里 `ReportedTaskCompletions` 这个资源在上游断言其「完全不存在」的时刻已经存在。

**为什么这轮不动：** 根因不在 `reminders/task_completion.rs`（我们只给它加了一个只读
`is_reported()`），而在别处更早地插入了该资源；定位需要跨 shell 的 tool 资源装配路径逐步
排查，本轮时间不够，而且贸然改回上游行为可能影响任务完成提醒的真实语义。

**后面该怎么动：** 在这两个测试上用上游断言复现，然后在 `xai-grok-shell` 的
`tools/tool_context.rs`、`tools/notification_bridge.rs` 两个候选文件里定位谁提前插入了
`State<ReportedTaskCompletions>`；结论要么是「我们的行为才对，把分歧写进测试注释并提给
上游」，要么是「这是回归，恢复上游行为并删掉这处放宽」。**在定论之前，不应把削弱上游断言
当成同步修补的常规手段** —— 这类改动会让后续同步失去回归信号。

### 7.3 `search_bootstrap.rs` 的 `#[cfg(test)] claim_hold` 调度钩子

我们在生产函数 `bootstrap_with_lease_inner` 里插了一个 `#[cfg(test)]` 的 sleep，并给
`BootstrapTiming` 加了 `#[cfg(test)] claim_hold` 字段（`5aaa72f3`），用来让 single-flight
测试确定性地观察到竞争。

**为什么这轮不动：** 要判定它是否还必要，得证明上游那版 single-flight 测试在当前代码上
**不再 flaky**，而 flaky 判定需要多次重复运行；`xai-grok-shell` 的测试编译+运行周期是分钟
级，本轮预算不够做一次可信的 flake 猎取。还原测试文件而不还原生产文件会直接编译失败
（上游测试不认识 `claim_hold` 字段），所以必须成对处理。

**后面该怎么动：** 单独起一个任务，把两个文件成对还原到上游，在 CI 上把该测试重复跑
50~100 次；不 flaky 就删掉这 10 行钩子，flaky 就保留并在注释里写明上游测试的竞态窗口，
同时把这个稳定化方案提给上游。

### 7.4 `scripts/automation/daily-upstream-release.sh`

没有任何 workflow 引用它，并且它硬编码了 macOS 路径与第三方应用内的 CLI 路径，与
「同步跑在 GitHub Actions 里」不符 —— 从仓库内看它是一条孤立的本地 runner。

**为什么这轮不动：** 我无法从仓库内确认你机器上是否有 launchd / cron 挂着它。删掉一个可能
正被仓库外调度器使用的脚本，风险大于收益；而它的同步成本是 0（上游没有 `scripts/`）。

**后面该怎么动：** 确认本地调度器是否还在用。若不用，连同 `daily-upstream-release-output.schema.json`
和 `docs/release-distribution.md:283` 那段一起删；若在用，把它挪到仓库外，或改成 workflow
并去掉 `/Applications/ChatGPT.app` 这类硬编码。

### 7.5 曾经未逐项审计的区域 —— 已在第二轮补完

第一轮原计划用 5 个并行子 agent 分区取证，它们全部因为环境里没有对应模型配置而启动即失败
（`PROXY_005 No model config found`），一份结论都没产出，所以第一轮把下列区域挂成了"未逐项
审计"。**第二轮已全部补完**，逐项结论见：

| 曾挂起的区域 | 现在在哪 |
|---|---|
| `xai-grok-telemetry`（6 个文件） | §10 |
| `crates/build/xai-proto-build`（`09aab64b`） | §11 |
| `xai-grok-tools` 其余 12 个文件（含 `build.rs`、`embedded_search_tools.rs`、`shell_state.rs`、`fd52ce42` / `3753e56f`） | §12 |
| `xai-grok-update`（5 个文件） | §15 |
| 「同步后测试修补」那批提交（`c114eb79`、`3929b2d8`、`ea242bf7`、`5511079c`、`cd71bf73`、`b54cdbb0`、`4c87aa55`、`6146028a`、`9ddf85bf`、`25705ffd`） | §16，逐条附实测结果 |
| `5636471e` / `f87dcb43`（allexport） | §12 |
| `xai-grok-shell` 剩余多 provider 实现细节 | §3 保留结论不变；其中被判死代码的一项已删（§16 `3929b2d8`） |

第二轮还额外发现并修掉了第一轮完全没覆盖到的一处过期修补：CI/release workflow 硬编码
Rust 1.92.0（§13）。

**仍然判不了的只剩两项**，单列在 §7.2 与 §7.3，理由都不是"没时间看"而是"证据不足以定论"：

- §7.2 `auto_wake_suppression_tests.rs`：能确定它掩盖了一处真实行为分歧（还原上游断言后两个
  测试稳定失败），但**定不了根因在哪个 fork 改动**，因此也定不了该恢复上游行为还是把分歧
  合法化；
- §7.3 `search_bootstrap.rs` 的 `#[cfg(test)] claim_hold` 钩子：判定它是否仍必要需要证明上游那版
  single-flight 测试**不再 flaky**，而 flaky 结论需要在 CI 上重复跑几十次，本轮预算不够；
  且它与生产文件成对耦合（只还原测试文件会直接编译失败）。

**已顺带核实一项，避免后人误删：** `plan_mode_edit_gate_tests.rs`（`+2/−2`，把期望从
`ToolLoop::Continue` 改成 `ToolLoop::PermissionReject`）以及与之配套的
`plan_exit_batch_barrier_tests.rs`，**不是**被改松的断言，而是有我们自己的生产改动支撑的：
上游 `tool_calls.rs:1021` 在 plan-mode 拒绝编辑时返回 `Ok(Err(ToolLoop::Continue))`，我们改成
了 `PermissionReject`（配套还把 `leave_plan_mode_to_default` 改成 async 并加了持久化失败回滚）。
这属于 §4 plan 文件防护那条线上的真实自有行为，**应当保留**，不要按"测试修补"处理。它和 §7.2
的区别正是判定这批补丁的关键：看还原后失败的是"断言被改松"还是"生产行为确实不同"。

### 7.6 顺带发现：一个上游自身的非 hermetic 测试

`claude_import::tests::gate_load_claude_env_returns_empty_when_marker_set` 会读取真实的
`~/.claude/settings.json`。在任何存在该文件且带 `env` 段的机器上它都会失败：

```
$ cargo test -p xai-grok-shell --lib -- claude_import::tests::gate_load_claude_env_...
FAILED: load_claude_env_with_project should be empty when marker set
$ HOME=$(mktemp -d) cargo test ... 同一个测试
ok
```

`crates/codegen/xai-grok-shell/src/claude_import.rs` 在我们的差异里是**空的**（纯上游文件），
所以这是上游自己的问题，违反 `AGENTS.md` 的「Keep tests hermetic」。CI 上没有 `~/.claude`，
不会红。**本轮刻意不打补丁** —— 再加一个 fork 侧测试修补正是本次要减少的东西。应报给上游。

---

## 8. 第一轮实际改了什么

> 两轮合计的改动清单与最终数字见 §19。本节保留第一轮结束时的记录。

| 提交 | 内容 |
|---|---|
| `b96300ed` | 还原 13 个上游文件的零价值改动（10 个行尾空行 + 3 个宏体缩进） |
| `8595f481` | 跟随上游的 Messages 缓存实现：删 tool-definition 断点、删 TTL 双桶 usage、TTL policy 改为上游放置之后的后处理、保留空块守卫；顺带还原 `turn.rs` / `updates.rs` 被写坏的宏体格式；同步修订 RFC 与两篇用户文档 |

### 验证（按 owning crate 收窄）

```
cargo fmt --all -- --check                                   # clean
cargo test -p xai-grok-sampling-types                        # 306 passed
cargo test -p xai-grok-sampler                               # 179 + 7 + 1 + 1 + 3 + 17 passed
cargo test -p xai-chat-state                                 # 352 passed
cargo check -p xai-grok-shell --all-targets                  # clean
cargo check -p xai-grok-pager --all-targets                  # clean
cargo test -p xai-grok-shell                                 # 6115 passed，1 failed
```

`xai-grok-shell` 那 1 个失败是 §7.6 的上游非 hermetic 测试，由本机 `~/.claude/settings.json`
触发，与本轮改动无关（清空 `HOME` 后同一测试通过），CI 上不会出现。

新增的回归保护：`cache_policy_five_minutes_reproduces_the_upstream_wire_shape` 断言默认路径
与上游 `build_messages_request` 逐字节一致 —— 以后谁再想在放置逻辑上加 fork 分支，这个测试
会先红。

---

## 9. 两个数（第一轮结束时）

> 第二轮之后的最终数字见 §19。

### 9.1 这一轮之后我们相对上游还剩多少差异

| 类别 | 审计前 | 本轮后 | 变化 |
|---|---|---|---|
| **修改上游已有文件** | 164 个，+8795/−1773 | **136 个，+7930/−1568** | **−28 个文件，−1070 行** |
| 　└ 测试类 | 38 个，+2051/−290 | 32 个，+1581/−259 | −6 个文件，−501 行 |
| 　└ 非测试 | 126 个，+6744/−1483 | 104 个，+6349/−1309 | −22 个文件，−569 行 |
| 新增自有文件 | 58 个，+11195 | 58 个，+11207 | +12 行（RFC 修订） |
| 合计 | 222 个，+19990/−1773 | **194 个，+19137/−1568** | −28 个文件 |

剩余差异的构成，按「是否产生同步成本」分：

- **零同步成本，约 11200 行 / 58 个文件（59%）**：`.github/`、`scripts/`、`packaging/`、
  `docs/`、`AGENTS.md`、`third_party/smartstring`。上游没有这些路径，永远不冲突。
- **有同步成本，约 9500 行 / 136 个文件（41%）**：主体是多 provider 运行时（shell 75 个
  文件）、privacy-hardening 门禁、两个安装脚本、以及 32 个测试文件的修补。

### 9.2 后续每天同步的成本预计怎么变

**小幅下降，且下降主要来自「概率」而不是「行数」。**

- 冲突面文件数 164 → 136（−17%），行数 10568 → 9498（−10%）。按每日同步冲突量与被改上游
  文件数大致成正比估算，预期每日冲突处理量降约 15%。
- 更重要的一处是概率而非体积：原来我们**改写了上游 `apply_cache_breakpoints` /
  `mark_message_cache_breakpoint` 的函数签名**，而上游刚在 `dd04f397` 把这两个函数整体重写
  过，属于活跃演进区。现在这两个函数除 12 行空块守卫外与上游一致，policy 全部移到附加层，
  这块的冲突概率从「上游每动一次缓存逻辑必冲突」降到接近零。同理，TTL 双桶原本让 4 个
  crate 的 usage 字段都跟上游不同名，任何上游 usage 改动都会波及；现在字段名回到上游。
- 反向的成本项：剩下 41% 的冲突面里，多 provider 运行时（shell 75 个文件）是不可压缩的
  —— 它是 fork 的立项理由。这部分的同步成本只能靠「把能提上游的提上游」来降，具体候选见
  §4（plan 文件防护）、§1.2（空块守卫）、§7.3（single-flight 稳定化）、§7.6（上游非 hermetic
  测试）。
- 如果 §7.1（smartstring）与 §7.4（孤立脚本）后续落地，文件数会再减约 22 个，但因为它们
  同步成本本来就是 0，**每日成本不会因此再降** —— 收益是仓库体积和审计面，不是同步成本。

一句话：第一轮把「每天都可能踩到的高频冲突点」搬走了，把「体量大但永不冲突」的部分留下了。
第二轮继续做完了逐项审计并又发现一处过期修补（§13），最终数字见 §19。

---

# 第二轮：逐项补审（2026-08-10）

第一轮 §7.5 把若干区域挂在「未逐项审计」。本节把它们做完，标准与前文一致：一手读上游实现、
依据可追溯、逐项给出结论。判定手法沿用第一轮验证过的那套 —— **把补丁还原成上游版本再跑对应
测试**，让代码而不是推测来回答「还需不需要」。

## 10. `xai-grok-telemetry`（6 个文件 +42/−2）

| 项 | 结论 | 依据 |
|---|---|---|
| `privacy-hardening` feature + `PRIVACY_HARDENED` 常量 + 4 处后端入口门禁（`client::init`、`client::init_if_needed`、`external::init`、`otel_layer::build_tracer_provider`、`sentry::init`）+ 1 个 feature 门测试 | **保留** | 上游 `xai-grok-telemetry/Cargo.toml` 的 `[features]` 只有 `default` / `default-bazel` / `memory-log`，无此 feature（diff 上下文即上游原文）。这 6 个文件构成一个完整且最小的熔断开关：每处门禁 3~4 行，落在"网络能力后端"的唯一入口上，编译期决定、运行时无法放宽 —— 正是对外分发二进制需要的性质 |

无删除项。这个区域的实现方式没有可挑剔处：它没有改写任何上游逻辑，只在入口处提前返回。

## 11. `crates/build/xai-proto-build`（+81/−12）

| 项 | 结论 | 依据 |
|---|---|---|
| 用临时目录替代 `--dependency_out=/dev/stdout --descriptor_set_out=/dev/null`，并按实际 target 前缀解析依赖输出；附带 Windows 路径分隔符处理与 3 个单元测试（`09aab64b`） | **保留** | 上游 `crates/build/xai-proto-build/src/lib.rs:132-133,164-166` 至今仍写死 `/dev/stdout` 与 `/dev/null`，在 Windows 上不存在这两个设备文件。我们的发布目标含 `x86_64-pc-windows-msvc` / `aarch64-pc-windows-msvc`（`scripts/dist/targets.json`），且 `.github/workflows/ci.yml:230` 有专门的 `Windows protobuf build` job 常态验证这条路径 |

这是纯可移植性修复、与 fork 的业务无关，**属于最适合提给上游的一类**，已列入 §14 清单。

## 12. `xai-grok-tools`（13 个文件）

| 项 | 涉及文件 | 结论 | 依据 |
|---|---|---|---|
| plan 文件 symlink / TOCTOU 防护 | `computer/protected_plan_file.rs`（新增 519 行）、`types/resources.rs` 的 `ProtectedPlanFilePath` + `guard_protected_plan_file_system`、`computer/mod.rs`，接入 `enter_plan_mode` / `exit_plan_mode` / `search_replace` / `opencode/write` / `grok_build_hashline/edit` | **保留** | 上游 `exit_plan_mode/mod.rs` 内 grep `symlink\|nofollow\|canonical\|protected` 无任何命中，上游无等价防护。Plan Mode 对唯一一个文件自动放行写入，该文件即安全边界。已有针对性测试 `protected_plan_hashline_write_rejects_final_symlink`（植入指向 secret 的 symlink 后断言写入被拒） |
| 内嵌搜索工具的发布方式改为 hard link（`3753e56f`） | `computer/local/embedded_search_tools.rs` | **保留** | 上游 `extract_bundled` 用 `fs::rename` 发布，会无条件替换目标 inode；我们改用 `hard_link`，遇到已存在的赢家就保留它。测试 `publishing_candidate_does_not_replace_existing_winner` 直接断言赢家 inode 不变 —— 即"正在被执行的二进制永远不会被换掉"这个不变量。顺带把手搓的临时文件名换成 `tempfile`，是净简化 |
| bash 状态转储保留 allexport 但不导出内部变量（`f87dcb43` / `5636471e`） | `computer/local/shell_state.rs` | **保留** | 上游 `dump_bash_state` 的过滤器是 `grep -vE '^set [-+]o (nounset\|errexit\|pipefail)$'`，不含 `allexport`，也不在转储期间关掉它。用户若开着 `set -a`，转储自身的大块 `grok_snap_*` 变量会被导出，后续 execve 因环境过大失败（bash 报 126）。我们的版本在转储前关闭、转储末尾按用户原值恢复 |
| 内嵌工具版本常量上调（rg 15.1.0 / bfs 4.1.4 / ugrep 7.8.2）+ `GROK_TOOLS_BUNDLE_*_VERSION` 覆盖与校验（`fd52ce42`） | `build.rs`、`Cargo.toml`（`dunce` build-dep、`libc` 从 linux-only 放宽到 unix） | **保留** | 三个常量与我们发布流水线的 pin 完全一致：`scripts/dist/tool-bundles.json` 为 ripgrep 15.1.0、bfs 4.1.4、ugrep 7.8.2。`_VERSION` 覆盖是 `scripts/dist/prepare-release-tools.sh` 注入 pinned 工具时使用的入口，上游没有这套分发流水线所以不需要它 |
| `reminders/task_completion.rs` 的 `is_reported()` 只读访问器 | 同上 | **保留**（但见 §7.2） | 2 行只读访问器本身无害；它服务的那处断言所掩盖的行为分歧仍未定位，那条单独留在 §7.2 |

**顺带确认的一件事（影响本地验证口径）：** `xai-grok-tools` 的 grep / 终端相关测试**在本机裸跑必然失败**
（本轮实测 74 个失败，退出码 `-1`），原因是被测代码要启动外部搜索工具子进程，而本机没有
`ugrep` / `bfs`，构建时也没有 `GROK_TOOLS_BUNDLE_*`。已用 `git stash` 对照确认：把本轮全部改动
移除后同样失败，且 `grok_build/grep/mod.rs` 是纯上游文件（不在我们的差异里）。CI 的
`Terminal, tools, and CLI` job 先跑 `scripts/dist/prepare-release-tools.sh` 准备 pinned 工具，所以
CI 上是绿的。**这不是 fork 引入的问题，也不需要打补丁 —— 只是本地跑 tools 测试前必须先准备工具。**

## 13. `.github/workflows` 的工具链 pin —— 本轮发现并已修复

这是本轮新发现的一处**我们自己的过期修补**，第一轮没有覆盖到。

| 项 | 结论 | 依据 |
|---|---|---|
| 三个 workflow 的 5 处 `rustup toolchain install 1.92.0` + `rustup default 1.92.0`，以及 8 处以 `1.92.0` 为组成部分的缓存键 | **改为跟随上游**（已改） | `rust-toolchain.toml` 是**纯上游文件**（不在我们的差异里），上游已在 `ed6d5436`（2026-08-04）把 channel 从 1.93.0 提到 1.94.0；而 rustup 的 toolchain 文件优先级高于 `rustup default`。上一次 CI 运行的日志直接印证：<br>`info: note that the toolchain '1.94.0-x86_64-unknown-linux-gnu' is currently in use (overridden by .../rust-toolchain.toml)`<br>即每个 job 都下载了一个装完就不用的工具链，那个名为 "Install pinned Rust toolchain" 的步骤装的是错的版本 |

**已改成**：每个 job 在 checkout 之后从 `rust-toolchain.toml` 解析一次 channel 写入 `RUST_CHANNEL`，
安装与全部缓存键都用它，并删掉被 toolchain 文件覆盖、本就无效的 `rustup default`。这样上游下次
提工具链版本时，我们的 workflow **不需要任何改动** —— 这正是每日同步成本的一个来源被消除。

副作用：缓存键取值改变，本次之后第一次运行会重新填充 Cargo 与 release-build 缓存。

**这一条也修正了 §7.1 的证据口径**：我为 smartstring 做的编译验证用的是 1.94.0，而 CI 实际使用的
正是 1.94.0（不是 workflow 里写的 1.92.0），所以 §7.1 的结论成立。

## 14. 宏体格式噪音的第二批清理

第一轮清了 3 个文件；本轮做了一次全仓扫描，在我们改过的上游文件里又发现 138 处
`"key" : value` / `= % e` / `= ? x` 这类 rustfmt 不会产出、且 `cargo fmt --check` 查不出来
（rustfmt 不进入宏体）的写法。

判据严格化：只有当某条**新增行**做格式归一化之后**恰好等于同一文件里被删除的那条上游行**时，
才判定为"我们把上游文本写坏了"并还原。据此还原 **15 个文件 94 行**。其余（`acp_session.rs` 12 处、
`types/resources.rs` 4 处、`session_compact.rs` 2 处等）匹配不到上游对应行，说明位于我们自己新增的
代码里，本轮**不动** —— 它们是风格问题不是回归，改了反而增加 diff。

## 15. `xai-grok-update`（5 个文件 +282/−41）

| 项 | 结论 | 依据 |
|---|---|---|
| 把更新源从 x.ai / GCS 改成我们自己的 GitHub Releases：`GH_RELEASE_REPO`、channel pointer、版本化 asset base（`release_asset_base_url`）、alpha 走 Releases API 选最大 SemVer | **保留** | 上游 `version.rs` 至今是 `GH_RELEASE_REPO = "xai-org-shared/grok-build"`、`CLI_BASE_URL_PRIMARY = "https://x.ai/cli"` 加 GCS 兜底。**这条不是可选项**：不改的话我们的自动更新会把用户的 privacy-hardened 二进制替换成 xAI 的官方构建，等于把整个 fork 的分发与 privacy 收紧全部绕过 |
| 下载产物的 SHA256SUMS 校验（`checksum_from_manifest` / `sha256_file` / `verify_github_release_checksum`，`sha2` 依赖） | **保留** | 上游 gh-release 下载路径没有任何校验步骤。这条是纯安全增强，与 fork 的 URL 改动可分离，**建议单独提给上游** |
| `tests/test_subprocess.rs`、`tests/test_install_sh.rs` 的仓库名/URL 断言随之更新 | **保留** | 跟随上面两条，属于必要配套 |

## 16. 同步后测试修补：逐项判定

方法：把补丁还原成上游版本，跑对应测试，让结果而不是推测来定性。下表每条都写了实测结果。

| 提交 | 涉及 | 实测 | 结论 |
|---|---|---|---|
| `ea242bf7` auth-retry 大栈包装 | `acp_session_tests/turn/auth_retry_budget_tests.rs`（+140/−104） | 还原上游后**当场复现**：`thread '...fail_closed_401_is_uncharged_and_turn_survives' has overflowed its stack` / `fatal runtime error: stack overflow, aborting`，SIGABRT **整个 shell 测试二进制**，其余 6000+ 测试全部无法报告 | **保留** |
| `b54cdbb0` chat-history 大栈包装 | `acp_session_tests/turn/chat_history_integrity_tests.rs`（+25/−2） | 同上批次一起还原，同样触发进程级 abort；上游该文件确认**没有**大栈模式（`git show upstream/main:... \| grep stack_size` 无命中） | **保留** |
| `b54cdbb0` allexport | `xai-grok-tools/computer/local/shell_state.rs` | 见 §12，上游过滤器不含 `allexport` | **保留** |
| `4c87aa55` doctor 不依赖宿主音频 | `pager/src/doctor_cmd/tests.rs`（+15/−2） | 还原上游后本机（macOS）**通过**，但这不构成依据：`apply_voice_probe` 在 `input_device_info()` 失败时追加一条 Issue，而 Linux 上录音器是 PATH 上的 `pw-record`/`parec`/`arecord` 子进程（`xai-grok-voice/src/audio/capture_linux.rs`），CI 镜像不保证安装；上游断言的是精确 `issue_count() == 1`，任何缺这三个二进制的宿主都会多出一条。macOS 走 CoreAudio 所以本机不复现 | **保留**（并建议提上游，见 §17） |
| `6146028a` prompt history 竞态 | `pager/src/app/app_view.rs`（+2/−1） | 还原上游后用编译好的测试二进制直接跑 **60 次全过**，本机复现不出 flake。但判定依据是语义不是稳定性：该测试的 "needs_animation 门控" 意图在同一函数里用三条独立的 `needs_animation()` 断言覆盖了，循环体自己的断言消息是 "tick() must poll the history daemon and deliver results" —— 只关心投递。上游把 `tick()` 的返回值（含义是"需要动画"）和"结果到达"用 `&&` 耦合在一起属于附带耦合 | **保留**（2 行，语义更准，代价可忽略） |
| `5511079c` prompt wiring 栈溢出 / `25705ffd` 收窄内存持久化测试 | `acp_session_tests/cancel_running_task_tests.rs`（+147/−56，其中 21 行是机械字段初始化） | 语义改动是把 `prompt_task.await.expect(...)` 换成 `abort()` + 断言已取消，理由写在代码注释里：被测契约止于 pre-inference 持久化屏障，不应耦合 sampler 去连 `http://localhost` 的错误路径。这消除了一个环境依赖（宿主上若有进程监听 localhost，上游写法的行为会变），且没有削弱该测试声明的契约 | **保留** |
| `cd71bf73` 终端清理稳定化 | `terminal/local_terminal.rs`（测试内超时 300ms→1s）、`terminal/pty_session.rs`、`xai-tty-utils/src/lib.rs`（`HANGUP_GRACE` 200ms→1s） | 测试内超时放宽属于合理去 flake（被测行为是"超时后的清理"，不是"shell 300ms 内启动"）。但 `HANGUP_GRACE` 是**生产常量**，为测试稳定性把每次终端拆除都拖慢 800ms | **保留，但记入 §17 待办**：正确形态是让该常量可注入，生产保持 200ms、测试用更长值。当前值本身也站得住（在 leader 转发 SIGHUP 之前杀掉它会漏下独立进程组里的子进程），所以本轮不动 |
| `9ddf85bf` chat state 测试初始化 | `xai-chat-state/src/commands.rs`（+79/−0，主体是多 provider 的两个命令） | 该提交的部分已被后续多 provider 工作吸收，`commands.rs` 现存差异就是 §3 判定保留的 `ReplaceSamplingConfigAndCredentials` / `UpdateCredentialsIfSamplingConfigMatches` | **保留**（并入 §3） |
| `3929b2d8` session registry gateway 标记 | `agent/mvp_agent/session_registry.rs`（+8/−0） | `rg mark_require_gateway` 全仓**只有定义没有调用者**；上游 `mod.rs:711` 有 `require_gateway` 字段并在 `counts()` 里统计，但上游也**没有**任何写入点。我们加了一个谁都不调的 setter，字段该 inert 还是 inert | **删除**（本轮已删，纯死代码） |
| `c114eb79` 移除 sessions notify | 该提交删了 `extensions/session_notify.rs` 等 | **不是对上游的分歧**：`git cat-file -e upstream/main:.../extensions/session_notify.rs` → 不存在，上游 `extensions/mod.rs` 也不声明它。那是我们自己先前加的 fork 功能，后来自己撤掉，净差异归零，当前 diff 里根本没有它 | **无需动作** |

**这一节最值得记住的一条方法论：** 判定这类补丁必须问两个问题 —— ①还原上游后**是什么**在失败：
是断言被改松（§7.2 那种，掩盖行为分歧），还是生产行为确实不同 / 进程直接崩（`ea242bf7` 那种）；
②上游到底**有没有**这段代码（`c114eb79` 看着像删上游代码，实际上游从来没有过）。只看提交长相
会两头都判错。

## 17. 待上报上游的清单

以下都是与 fork 业务无关、上游同样受益的通用修复或上游自身的问题。它们目前作为 fork 差异
长期存在，**推给上游是唯一能真正消除这部分同步成本的办法**。

| # | 内容 | 位置 | 性质 |
|---|---|---|---|
| 1 | protoc 依赖追踪的可移植性：`--dependency_out=/dev/stdout` / `--descriptor_set_out=/dev/null` 在 Windows 上不存在 | `crates/build/xai-proto-build/src/lib.rs` | 纯可移植性缺陷 |
| 2 | 缓存断点不应落在空 Text / 空 ToolResult 上（API 以 “text content blocks must be non-empty” 拒绝） | `xai-grok-sampling-types` `mark_message_cache_breakpoint` | 上游逻辑的真实缺陷 |
| 3 | `plan.md` 的 symlink / TOCTOU 防护（Plan Mode 自动放行写入的那个文件即安全边界） | `xai-grok-tools/computer/protected_plan_file.rs` | 安全加固 |
| 4 | bash 状态转储在用户开启 `set -a` 时导出内部大变量，导致后续 execve 因环境过大失败（bash 报 126） | `xai-grok-tools/computer/local/shell_state.rs` | 真实 bug |
| 5 | 内嵌工具发布不应替换正在被执行的二进制（`rename` → `hard_link`） | `xai-grok-tools/computer/local/embedded_search_tools.rs` | 健壮性 |
| 6 | 下载产物缺少 SHA256SUMS 校验 | `xai-grok-update/src/auto_update.rs` | 安全增强 |
| 7 | 插件提供的 hooks 在会话启动时未合入 hook registry，只有显式 reload 后才生效 | `xai-grok-shell/.../spawn.rs` + `hooks_plugins.rs` | 行为缺陷 |
| 8 | 全量 turn-loop 测试在 debug 构建下超出 libtest 默认线程栈，直接 SIGABRT 掉整个测试二进制 | `auth_retry_budget_tests.rs` / `chat_history_integrity_tests.rs` | 上游测试不可运行（本轮已复现） |
| 9 | `claude_import` 的 marker 测试读取真实 `~/.claude/settings.json`，在任何存在该文件且带 `env` 段的机器上失败（违反 `AGENTS.md` 的 hermetic 要求） | `xai-grok-shell/src/claude_import.rs`（**纯上游文件**，我们没打补丁） | 上游测试非 hermetic |
| 10 | doctor 报告断言精确 `issue_count()`，在缺 `pw-record`/`parec`/`arecord` 的宿主上会因多一条音频 Issue 而失败 | `pager/src/doctor_cmd/tests.rs` | 上游测试非 hermetic |

第 9 条按你的判断**不打补丁**：再加一个 fork 侧测试修补正是本轮要减少的东西，CI 上没有
`~/.claude` 所以不会红。

---

## 18. 本地打版本与产物验证（2026-08-10，aarch64-apple-darwin）

CI 的 `Distribution E2E` 只跑 `scripts/dist/test.sh`（用结构合法的合成可执行映像做确定性打包
校验），**不构建也不运行真实二进制**。本节是在本地把完整链路走通并把产物跑起来的记录。
未打 tag、未改版本号、未触发任何 release 工作流。

### 18.1 build

```
$ bash scripts/dist.sh build --target aarch64-apple-darwin --version 0.2.125 --allow-unbundled-tools
dist: warning: building without release tool bundle(s): GROK_TOOLS_BUNDLE_RG_PATH ... GROK_TOOLS_BUNDLE_UGREP_VERSION
dist: building grok-build 0.2.125 for aarch64-apple-darwin
    Finished `release-dist` profile [optimized + debuginfo] target(s) in 17m 15s
dist: wrote .../release-dist/xai-grok-pager.build-attestation.json
.../target/aarch64-apple-darwin/release-dist/xai-grok-pager
```

`--allow-unbundled-tools` 是脚本自身文档指定的本地诊断开关（未提供 pinned 的 rg/bfs/ugrep）。

**过程中撞到并确认了 attestation 的真实约束：** 第一次构建后我提交了一次文档改动，随后打包
直接被拒：

```
Error: build attestation: source revision does not match the packaging checkout
```

这说明 attestation 确实把产物绑定到构建时的源码版本，不是装饰。**修法不是绕过**（存在
`--allow-unattested` 诊断开关，但用了就等于没验证这条链），而是在稳定的 HEAD 上重新构建：

```
$ git rev-parse --short HEAD    # 8eecda30，工作树 0 处改动
$ bash scripts/dist.sh build --target aarch64-apple-darwin --version 0.2.125 --allow-unbundled-tools
    Finished `release-dist` profile [optimized + debuginfo] target(s) in 6m 26s
dist: wrote .../xai-grok-pager.build-attestation.json
```

### 18.2 package / verify / checksums

```
$ bash scripts/dist.sh package --target aarch64-apple-darwin --version 0.2.125 \
    --allow-unbundled-tools --output-dir /tmp/grok-dist-out
verified grok-build 0.2.125 for aarch64-apple-darwin: 13 payload files
dist: wrote /tmp/grok-dist-out/SHA256SUMS
dist: created /tmp/grok-dist-out/grok-build-0.2.125-aarch64-apple-darwin.tar.gz

$ bash scripts/dist.sh verify --archive /tmp/grok-dist-out/grok-build-0.2.125-aarch64-apple-darwin.tar.gz
verified grok-build 0.2.125 for aarch64-apple-darwin: 13 payload files

$ bash scripts/dist.sh checksums --output-dir /tmp/grok-dist-out
dist: wrote /tmp/grok-dist-out/SHA256SUMS

$ cat /tmp/grok-dist-out/SHA256SUMS
a0c907ede80c453cc5e6a215326534156d957f6c03acb329bec5f8970b0b8609  grok-build-0.2.125-aarch64-apple-darwin.tar.gz
```

归档 54,426,276 字节；解包后 14 个文件，独立校验全部通过：

```
$ shasum -a 256 -c MANIFEST.sha256
bin/grok: OK
build-attestation.json: OK
build-manifest.json: OK
BUNDLED-TOOLS-NOTICES.md: OK
LICENSE: OK
profiles/starter/{agents,hooks,plugins,skills}/.gitkeep: OK
profiles/starter/config.toml: OK
profiles/starter/{hooks/README.md,README.md}: OK
SOURCE_REV: OK
THIRD-PARTY-NOTICES: OK
```

`build-manifest.json` 关键字段：

```
.source.gitCommit      8eecda305dd2b7f492af4dbde759528d252a8e25   # == 构建时 HEAD
.source.sourceRev      a61c32b12a2b400f212221cd8762e05f9b36828d   # == 仓库 SOURCE_REV
.source.dirty          False
.build.profile         release-dist
.build.features        ["default","jemalloc","release-dist","sandbox-enforce"]
.build.rustc           rustc 1.94.0 (4a4ef493e 2026-03-02)
.releaseReady          False        # 正确标记为诊断构建（未捆绑 pinned 工具）
.artifact.sha256       4c1c1e1b6d56d74e9579a4e9544d8be8d9635a5fcea5c1959335c6a80a824b49
```

`.build.rustc` 是 **1.94.0**，再次印证 §13：workflow 里硬编码的 1.92.0 从来没被真正使用。

### 18.3 把产物跑起来

```
$ bin/grok --version
grok 0.2.125 (8eecda30)

$ file bin/grok
Mach-O 64-bit executable arm64
```

版本串里的 commit 与构建时 HEAD 一致。真实使用（`GROK_HOME` 指向临时目录以免污染本机）：

```
$ grok doctor
Grok Doctor
Environment
  · terminal                     Ghostty
  · multiplexer                  cmux
Clipboard
  · native                       local (pbcopy)
  · status                       confirmed
Voice
  · microphone                   none detected (mic device lookup did not start within 5s)
Findings
  ! voice.no-input-device        Voice dictation is unavailable: ...
1 issue, 0 recommendations
```

顺带实证了 §16 对 `4c87aa55` 的判断：doctor 确实会因为找不到麦克风而**多出一条 Issue**，
所以上游那条精确 `issue_count() == 1` 的断言在缺音频工具的宿主上必然失败。

```
$ grok inspect
  Environment
  └ Version: 0.2.125 [unknown]
  └ Privacy hardened: yes
  └ CWD: /Users/tianyi/opensourceProjects/grok-build
  └ Git root: ...
  └ Project trusted: no
  Project Instructions (1) / Permissions (2 loaded) / Skills (22) / Agents (3) / Plugins (2)

$ grok models
You are not authenticated.
Default model: grok-4.5
Available models:
  * grok-4.5 (default)

$ grok sessions list
No sessions found.

$ grok du
Disk usage for $GROK_HOME
    476.0 KB  docs
     36.0 KB  sessions
    684.0 KB  total
```

**`Privacy hardened: yes` 是本节最有价值的一条：** 它证明 `release-dist` 构建确实启用了
`privacy-hardening`，即 §3 / §10 判定"保留"的那套熔断开关在真实分发产物里是生效的 ——
这一点此前只由 `Cargo.toml` 的 feature 声明支撑，现在有了产物级证据。

### 18.4 在真实产物上验证 5m / 1h / off 配置

写一份 provider 级 `ttl = "1h"` 加 model 级 `mode = "off"` 覆盖的 `config.toml`：

```
$ grok models
Available models:
  * grok-4.5 (default)
  - claude-1h
  - claude-nocache
```

两个模型都被接受。为确认这不是"未知键被静默忽略"，再故意写一个非法 TTL：

```
$ # prompt_cache = { mode = "stable_prefix", ttl = "42h" }
$ grok models
Error: Failed to create agent config: unknown variant `42h`, expected `5m` or `1h`
in `provider.anthropic.prompt_cache.ttl`
```

配置层是严格的、错误信息带完整路径。所以上面 `1h` 被接受确实意味着这颗**打包出来的二进制**
端到端理解一小时缓存策略。

### 18.5 这一节暴露的一件事

`scripts/dist.sh build` 与 `package` 之间**不能有任何提交**，否则 attestation 校验必然失败。
这是设计意图（产物绑定源码版本），但脚本用法里没写明，`docs/release-distribution.md` 也只
提到 `--allow-unattested` 这个诊断出口。已在 §17 之外记一条文档待办：在 `dist.sh` 的 usage
里明确"build 与 package 必须在同一个 commit 上"，避免下次有人误用 `--allow-unattested` 把
校验绕过去。

---

## 19. 两轮合计：改了什么，以及两个数

### 19.1 全部改动

| 提交 | 内容 |
|---|---|
| `b96300ed` | 还原 13 个上游文件的零价值改动（10 处行尾空行 + 3 个宏体缩进） |
| `8595f481` | 跟随上游的 Messages 缓存实现：删 tool-definition 断点、删 TTL 双桶 usage、TTL policy 改为上游放置之后的后处理、保留空块守卫；还原 `turn.rs`/`updates.rs` 被写坏的宏体；修订 RFC 与两篇用户文档 |
| `ae7e0cb6` | 加入本评估报告 |
| `ba5afb1b` | 记录 plan-mode gate 测试属于真实自有行为，避免后人误删 |
| `ec4bda42` | 1h prompt cache 通路补断言：请求体里**每一个**断点都必须带 `ttl:"1h"`；新增 `off` 的集成断言 |
| `65ec6a04` | CI/release workflow 改为从 `rust-toolchain.toml` 解析工具链，删掉无效的 `rustup default 1.92.0` 与 8 处硬编码缓存键 |
| `23f05ad7` | 删掉死代码 `SessionRegistry::mark_require_gateway`；完成宏体格式清理的第二批（44 行） |
| `8eecda30` | 报告补完逐项结论（§10–§17） |
| 本次 | 报告补入本地打版本验证（§18）与最终数字（§19） |

### 19.2 验证

按 owning crate 收窄：

```
cargo fmt --all -- --check                          # clean
cargo test -p xai-grok-sampling-types               # 306 passed
cargo test -p xai-grok-sampler                      # 全绿（含 1h/off 走完整 HTTP/SSE 的集成测试）
cargo test -p xai-chat-state                        # 352 passed
cargo test -p xai-grok-shell                        # 6113 / 6116 passed
cargo check -p xai-grok-shell -p xai-grok-pager -p xai-grok-tools --all-targets   # clean
```

`xai-grok-shell` 的 3 个失败**全部是宿主相关的上游测试**，单独跑都通过，且都不在本分支的改动
范围内：`claude_import` 的 marker 测试读真实 `~/.claude/settings.json`（§7.6）；
`provider_zero_timeout_clamps_to_one_second` 与
`capture_changes_diff_lazy_baseline_with_existing_history` 在全量并行下对时序敏感。

`xai-grok-tools` 的 74 个失败同样与本分支无关：本机没有 `ugrep`/`bfs`，被测代码要起外部搜索
工具子进程；已用 `git stash` 对照确认（详见 §12 末段）。CI 的对应 job 会先准备 pinned 工具。

发布链路本地实测见 §18：build → package（attestation 校验通过）→ verify → checksums → 解包 →
`grok --version` / `doctor` / `inspect` / `models` / `sessions` / `du` 全部真实运行，
`Privacy hardened: yes`。

### 19.3 数字一：还剩多少差异

代码差异（不含本评估报告自身）：

| 类别 | 审计前 | 两轮之后 | 变化 |
|---|---|---|---|
| **修改上游已有文件**（同步冲突面） | 164 个，+8795/−1773 | **135 个，+7933/−1481** | **−29 个文件；新增行 −862，删除行 −292** |
| 　└ 测试类 | 38 个，+2051/−290 | 32 个，+1658/−238 | −6 个文件（新增的 1h/off 断言使行数略回升） |
| 　└ 非测试 | 126 个，+6744/−1483 | 103 个，+6275/−1243 | −23 个文件 |
| 新增自有文件（上游无该路径） | 58 个，+11195 | 58 个，+11242 | +47 行 |
| 代码合计 | 222 个，+19990/−1773 | **193 个，+19175/−1481** | −29 个文件 |

按「是否产生同步成本」拆：

- **零同步成本：58 个文件、约 11240 行（59%）** —— `.github/`、`scripts/`、`packaging/`、
  `docs/`、`AGENTS.md`、`third_party/smartstring`。上游不存在这些路径，永远不冲突。
- **有同步成本：135 个文件、约 9410 行（41%）** —— 主体是多 provider 运行时（shell 74 个
  文件）、privacy-hardening 门禁、两个安装脚本与 updater 的分发改向、32 个测试文件。

### 19.4 数字二：后续每天同步的成本预计怎么变

**冲突面文件数 164 → 135（−18%），冲突面行数 10568 → 9414（−11%）。若按每日冲突量与被改
上游文件数近似成正比估算，预期每日冲突处理量下降 15%~20%。**

但真正的收益在**概率**而不是行数，具体有四处：

1. **缓存放置逻辑不再是我们的。** 原先我们改写了上游 `apply_cache_breakpoints` /
   `mark_message_cache_breakpoint` 的**函数签名**，而上游刚在 `dd04f397` 把这两个函数整体重写
   过 —— 属于活跃演进区，上游每动一次必冲突。现在除 12 行空块守卫外与上游一致，policy 全在
   附加层，这块冲突概率降到接近零，并有一条逐字节比对的测试守住。
2. **usage 字段名回到上游。** TTL 双桶原本让 4 个 crate 的字段都与上游不同名，任何上游 usage
   改动都会波及三处以上；现在同名。
3. **工具链不再硬编码。** 上游每次提 `rust-toolchain.toml`（近期已提过两次：1.92→1.93→1.94），
   过去都要手改 5 处安装步骤与 8 处缓存键，且不改也不报错、只是静默下载一个用不上的工具链。
   现在为零。
4. **宏体噪音清零。** 138 处 rustfmt 查不出、diff 里却真实存在的格式差异已全部处理，其中 94 处
   本就是被写坏的上游原文 —— 这类差异每次同步都要人工确认一遍"是不是我故意改的"。

反向的、压不掉的部分：剩余 41% 冲突面的主体是多 provider 运行时，那是 fork 的立项理由。
它的同步成本只能靠**把能提上游的提上游**来降 —— §17 列了 10 条候选，其中第 1、4、5、7 条
（protoc 可移植性、allexport、工具发布、插件 hooks）与 fork 业务完全无关，最容易被上游接受；
第 8、9、10 条是上游测试自身的问题，提上去还能顺带消掉我们的测试侧补丁。

§7.1（smartstring）与 §7.4（孤立脚本）若后续落地，会再减约 22 个文件，但它们同步成本本来
就是 0，**每日成本不会因此下降** —— 收益是仓库体积与审计面。
