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

### 7.5 未逐项审计的区域（本轮覆盖不全，如实说明）

本轮原计划用 5 个并行子 agent 分区取证，但它们全部因为环境里没有对应模型配置而启动即
失败（`PROXY_005 No model config found`），一份结论都没产出。因此下列区域**只做了区域级
判断，没有逐项取证**，报告不为它们给出逐项结论：

- `xai-grok-shell` 剩余的多 provider 实现细节（75 个文件里，除 §1、§2、§3 已定论的部分）；
- `xai-grok-tools` 除 `protected_plan_file.rs` 外的 12 个文件（`embedded_search_tools.rs`、
  `shell_state.rs`、几个 tool implementation 的改动，对应 `fd52ce42` / `3753e56f`）；
- `xai-grok-update`（5 个文件 +282/−41）、`xai-grok-telemetry`（6 个文件 +42/−2）、
  `crates/build/xai-proto-build`（`09aab64b`）、`xai-grok-tools/build.rs`；
- 其余「同步后测试修补」提交：`c114eb79`、`3929b2d8`、`ea242bf7`、`5511079c`、`cd71bf73`、
  `b54cdbb0`、`5636471e`/`f87dcb43`、`4c87aa55`、`6146028a`、`9ddf85bf`、`25705ffd`。

**后面该怎么动：** 这些项适合用本轮验证过的方法逐个判定 —— **把补丁还原成上游版本，跑对应
测试**。这个方法在本轮已经产出两个明确结论（§7.2 证明补丁掩盖了真实分歧；§7.3 证明补丁与
生产代码耦合），比读代码猜测可靠。建议按 owning crate 分批，每批一个提交。

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

## 8. 本轮实际改了什么

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

## 9. 两个数

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

一句话：本轮把「每天都可能踩到的高频冲突点」搬走了，把「体量大但永不冲突」的部分留下了；
要继续压低每日成本，下一步的杠杆是 §7.5 那批未逐项审计的测试修补，以及把 §4/§1.2 这类通用
修复推给上游。
