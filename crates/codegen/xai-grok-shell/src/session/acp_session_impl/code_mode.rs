//! Code Mode host. Nested calls reuse the ordinary prepare/dispatch/finalize pipeline;
//! only delivery is scoped to a nested sink rather than the provider transcript.
use super::*;
use futures::{FutureExt, StreamExt, future::LocalBoxFuture, stream::FuturesUnordered};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::rc::Rc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use xai_grok_tools::code_mode as runtime;
use xai_grok_tools::types::tool::ToolKind;

type FileLocks =
    Arc<parking_lot::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>;
type NestedResult = (
    String,
    bool,
    Vec<ConversationItem>,
    Option<ToolLoop>,
    Option<serde_json::Value>,
);

tokio::task_local! {
    static NESTED: Rc<NestedSink>;
}
#[derive(Default)]
struct NestedSink {
    parent: String,
    results: RefCell<Vec<ConversationItem>>,
    failed: Cell<bool>,
    locks: FileLocks,
    prepared: RefCell<Option<serde_json::Value>>,
}

pub(super) fn mark_failed() {
    let _ = NESTED.try_with(|sink| sink.failed.set(true));
}
pub(super) fn parent_id() -> Option<String> {
    NESTED.try_with(|sink| sink.parent.clone()).ok()
}

/// Parent `execute` call ID of a nested call ID (`{parent}/code/{n}`).
pub(super) fn nested_parent_id(id: &str) -> Option<&str> {
    let (parent, sequence) = id.rsplit_once("/code/")?;
    (!parent.is_empty() && !sequence.is_empty() && sequence.bytes().all(|b| b.is_ascii_digit()))
        .then_some(parent)
}
pub(super) fn file_lock(path: &str) -> Option<Arc<tokio::sync::Mutex<()>>> {
    NESTED
        .try_with(|sink| {
            sink.locks
                .lock()
                .entry(path.to_owned())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        })
        .ok()
}
pub(super) fn record_prepared(prepared: &PreparedToolCall) {
    let _ = NESTED.try_with(|sink| {
        *sink.prepared.borrow_mut() = Some(json!({
            "tool_id":prepared.tool_id,"arguments":prepared.execution_arguments(),
            "invocation_id":prepared.invocation_id,"model":prepared.model_id,
        }))
    });
}
/// Every tool the session can call composes, except recursion, the MCP
/// meta-dispatch pair (MCP tools enter the catalog by name instead), and
/// turn-control tools that wait on the user or switch the turn's mode.
fn composable(name: &str, kind: Option<ToolKind>) -> bool {
    name != "execute"
        && !matches!(
            kind,
            Some(
                ToolKind::SearchTool
                    | ToolKind::UseTool
                    | ToolKind::EnterPlan
                    | ToolKind::ExitPlan
                    | ToolKind::AskUser
            )
        )
}
#[cfg(test)]
thread_local! {
    /// Per-thread override so tests never mutate the process environment.
    static ENABLED_FOR_TEST: Cell<bool> = const { Cell::new(false) };
}
/// On by default; `GROK_CODE_MODE=0` (or false/off/no) disables it.
#[cfg(not(test))]
fn enabled() -> bool {
    !std::env::var(runtime::ENV_ENABLED).is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}
/// Unit tests ignore the environment and opt in per thread.
#[cfg(test)]
fn enabled() -> bool {
    ENABLED_FOR_TEST.with(Cell::get)
}
const ENV_TIMEOUT_SECS: &str = "GROK_CODE_MODE_TIMEOUT_SECS";
const DEFAULT_WALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
/// Overall run limit including tool and permission waits; subagents and workflows
/// routinely outlive a couple of minutes. Clamped to 10 s ..= 1 h.
fn wall_timeout() -> std::time::Duration {
    std::env::var(ENV_TIMEOUT_SECS)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|secs| std::time::Duration::from_secs(secs.clamp(10, 3600)))
        .unwrap_or(DEFAULT_WALL_TIMEOUT)
}
fn definition() -> ToolDefinition {
    ToolDefinition::function(
        "execute",
        Some(format!(
            "Compose any of your tools, including MCP server tools, with JavaScript in an isolated runtime. Choose by what the task needs: prefer it when you need a derived answer (filtered, counted, deduplicated or joined data) rather than the raw results, or when a fixed multi-step flow feeds one result into the next; call tools directly (independent calls already run in parallel) when you need to read full results or decide each next step from them. If a direct result turns out too long, use it for later similar calls. Input code is an async function body: use await and explicit return (otherwise null). catalog.search(query, offset) matches names and descriptions (20 per page); catalog.describe(name) returns the input schema; then call tools[name](args), e.g. tools[\"server__tool\"]({{...}}). MCP tools found with search_tool are callable here by that same name. The catalog functions return JSON objects. Tool calls resolve to their text output; do not assume it is structured JSON. Use Promise.allSettled for partial failures; at most {in_flight} calls in flight and {calls} calls total. All calls keep normal permissions, hooks and Plan Mode. Await all required work: unfinished calls are interrupted at return, without rolling back completed effects. No filesystem, network, imports, timers, process, or persistent JS state; reach everything through tools. console.log appends bounded logs. Return compact JSON data (only the fields you need, not raw results) with all nested promises awaited. Limits: 64 KiB source, 64 MiB JS heap, 5 seconds active computation, {wall} seconds overall INCLUDING tools and permission waits, 64 KiB result/log text. Do not retry an entire failed script with writes without inspecting its trace.",
            in_flight = runtime::MAX_IN_FLIGHT,
            calls = runtime::MAX_CALLS,
            wall = wall_timeout().as_secs(),
        )),
        json!({"type":"object","properties":{"code":{"type":"string"}},"required":["code"],"additionalProperties":false}),
    )
}

// Bound aggregate Rust-owned results/images/followups, outside the JS heap.
const HOST_RESULT_BUDGET: usize = 16 * 1024 * 1024;
struct ResultBudget(usize);
impl Write for ResultBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.checked_sub(bytes.len()).ok_or_else(|| {
            std::io::Error::other("Code Mode aggregate host result budget exceeded; inspect trace")
        })?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Local trace opened once with exclusive creation. Dropped executions
/// retain admission records and an explicit unknown outcome; never infer rollback.
struct RunTrace {
    file: std::fs::File,
    rows: BTreeMap<String, serde_json::Value>,
    finished: bool,
}
impl RunTrace {
    fn event(&mut self, value: &serde_json::Value) -> std::io::Result<()> {
        let mut timed = value.clone();
        let object = timed
            .as_object_mut()
            .ok_or_else(|| std::io::Error::other("Invalid Code Mode trace event"))?;
        object.insert(
            "timestamp_ms".to_owned(),
            json!(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            ),
        );
        serde_json::to_writer(&mut self.file, &timed)?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        if value.get("event").and_then(serde_json::Value::as_str) == Some("finished") {
            self.finished = true;
        }
        Ok(())
    }
    fn start(
        &mut self,
        id: &str,
        name: &str,
        arguments: &serde_json::Value,
    ) -> std::io::Result<()> {
        let row =
            json!({"id":id,"name":name,"sequence":self.rows.len()+1,"status":"outcome_unknown"});
        self.event(&json!({"event":"admitted","call":row,"arguments":arguments}))?;
        self.rows.insert(id.to_owned(), row);
        Ok(())
    }
}
impl Drop for RunTrace {
    fn drop(&mut self) {
        let unfinished: Vec<_> = self
            .rows
            .values()
            .filter(|r| r["status"] == "outcome_unknown")
            .cloned()
            .collect();
        if !self.finished || !unfinished.is_empty() {
            let _ = self.event(&json!({"event":"closed","program_finished":self.finished,"unfinished":unfinished,"warning":"Cancellation does not roll back or confirm termination of external effects"}));
        }
    }
}

#[derive(Default)]
struct CodeRunDelivery {
    followups: Vec<ConversationItem>,
    images: Vec<xai_grok_sampling_types::ContentPart>,
    trace_path: Option<String>,
}

// A cancelled run keeps its slot until its worker has actually exited.
static WORKERS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));
struct WorkerGuard {
    child: Option<tokio::process::Child>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    group: Option<Arc<xai_tty_utils::ProcessGroup>>,
    reaped: bool,
}
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let group = self.group.take();
        if let Some(group) = &group {
            let _ = group.kill();
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let permit = self.permit.take();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _permit = permit;
                    let _ = child.wait().await;
                    drop(group);
                });
            }
        }
    }
}

impl SessionActor {
    /// Scope is polled on the turn task, including all finalize/error paths.
    /// Dispatch workers never write the conversation and need no task-local state.
    pub(super) fn deliver_tool_result(&self, item: ConversationItem) {
        let mut item = Some(item);
        if NESTED
            .try_with(|sink| {
                sink.results
                    .borrow_mut()
                    .push(item.take().expect("one delivery"))
            })
            .is_err()
        {
            self.chat_state_handle
                .push_tool_result(item.expect("direct delivery"));
        }
    }
    pub(crate) fn code_mode_available(&self) -> bool {
        enabled()
            && self
                .agent
                .borrow()
                .tool_bridge()
                .tool_kind("execute")
                .is_none()
    }
    pub(super) fn add_code_mode_definition(&self, definitions: &mut Vec<ToolDefinition>) {
        if self.code_mode_available() && !definitions.iter().any(|d| d.function.name == "execute") {
            definitions.push(definition());
        }
    }

    pub(super) async fn execute_tool_calls(
        &self,
        calls: Vec<crate::sampling::types::ToolCallResponse>,
        requested_model: Option<String>,
    ) -> Result<ToolLoop, acp::Error> {
        if !self.code_mode_available() || !calls.iter().any(|c| c.function.name == "execute") {
            return self
                .execute_regular_tool_calls(calls, requested_model)
                .await;
        }
        // Preserve limits scoped to the entire model response before splitting
        // ordinary groups around execute (notably media generation caps).
        let calls = self.reject_excess_media_gen_calls(calls).await?;
        // One round for the whole response: groups share the interruption state,
        // and followups/interjections wait until every call has its result.
        let mut deferred_followups = Vec::new();
        let mut final_result: Option<ToolLoop> = None;
        let mut pending = Vec::new();
        for call in calls {
            if call.function.name != "execute" {
                pending.push(call);
                continue;
            }
            if !pending.is_empty() {
                self.execute_regular_tool_call_group(
                    std::mem::take(&mut pending),
                    requested_model.as_deref(),
                    &mut deferred_followups,
                    &mut final_result,
                )
                .await?;
            }
            if final_result.is_some() {
                self.deliver_tool_result(ConversationItem::tool_result(
                    call.id,
                    "Not executed after an earlier tool interrupted this batch",
                ));
                continue;
            }
            let outcome = self
                .execute_code_mode(
                    call,
                    requested_model.as_deref(),
                    None,
                    &mut deferred_followups,
                )
                .await?;
            if !matches!(outcome, ToolLoop::Continue) {
                final_result = Some(outcome);
            }
        }
        if !pending.is_empty() {
            self.execute_regular_tool_call_group(
                pending,
                requested_model.as_deref(),
                &mut deferred_followups,
                &mut final_result,
            )
            .await?;
        }
        Ok(self
            .finish_tool_round(deferred_followups, final_result)
            .await)
    }

    async fn execute_code_mode(
        &self,
        call: crate::sampling::types::ToolCallResponse,
        model: Option<&str>,
        worker_path: Option<&Path>,
        deferred_followups: &mut Vec<ConversationItem>,
    ) -> Result<ToolLoop, acp::Error> {
        let id = acp::ToolCallId::new(Arc::from(call.id.clone()));
        self.send_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(id.clone(), "execute")
                    .kind(acp::ToolKind::Other)
                    .status(acp::ToolCallStatus::InProgress)
                    .raw_input(
                        serde_json::from_str::<serde_json::Value>(&call.function.arguments).ok(),
                    ),
            ),
            None,
        )
        .await;
        let mut final_loop = ToolLoop::Continue;
        let mut delivery = CodeRunDelivery::default();
        let run = self.run_code_mode(&call, model, &mut final_loop, &mut delivery, worker_path);
        let mut result = match tokio::time::timeout(wall_timeout(), run).await {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                json!({"ok":false,"error":error.to_string(),"warning":"Inspect nested trace before retrying; completed effects are not rolled back"})
            }
            Err(_) => {
                json!({"ok":false,"error":"Code Mode overall timeout (includes tool and permission waits)","warning":"Unfinished effects have unknown outcome; inspect the nested trace before retrying"})
            }
        };
        if let Some(object) = result.as_object_mut() {
            object.insert("parent_id".to_owned(), json!(call.id));
            if let Some(trace_name) = delivery.trace_path {
                object.insert("trace".to_owned(), json!(trace_name));
            }
        }
        let success = result.get("ok").and_then(serde_json::Value::as_bool) == Some(true);
        let text = serde_json::to_string(&result)
            .unwrap_or_else(|_| "Code Mode result serialization failed".to_owned());
        self.send_update(
            acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                id,
                acp::ToolCallUpdateFields::new()
                    .status(Some(if success {
                        acp::ToolCallStatus::Completed
                    } else {
                        acp::ToolCallStatus::Failed
                    }))
                    .content(Some(vec![acp::ToolCallContent::from(
                        acp::ContentBlock::Text(acp::TextContent::new(text.clone())),
                    )])),
            )),
            None,
        )
        .await;
        self.deliver_tool_result(ConversationItem::tool_result_with_images(
            call.id,
            text,
            delivery.images,
        ));
        deferred_followups.extend(delivery.followups);
        Ok(final_loop)
    }

    async fn run_code_mode(
        &self,
        call: &crate::sampling::types::ToolCallResponse,
        model: Option<&str>,
        final_loop: &mut ToolLoop,
        delivery: &mut CodeRunDelivery,
        worker_path: Option<&Path>,
    ) -> anyhow::Result<serde_json::Value> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            code: String,
        }
        let input: Input = serde_json::from_str(&call.function.arguments)?;
        anyhow::ensure!(
            input.code.len() <= runtime::MAX_CODE_BYTES,
            "Code Mode source exceeds limit"
        );
        let inventory = self.code_mode_inventory().await;
        let catalog = inventory.values().map(|d| json!({"name":d.function.name,"description":d.function.description,"parameters":d.function.parameters})).collect();
        let dir = crate::session::persistence::ensure_owner_only_session_dir(&self.session_info)?;
        let trace_name = format!("code-mode-{}.jsonl", uuid::Uuid::new_v4());
        let permit = WORKERS.clone().acquire_owned().await?;
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut trace = RunTrace {
            file: options.open(dir.join(&trace_name))?,
            rows: BTreeMap::new(),
            finished: false,
        };
        delivery.trace_path = Some(dir.join(&trace_name).to_string_lossy().into_owned());
        trace.event(&json!({"event":"program","parent_id":call.id,"code":input.code}))?;
        // Only the composition root supplies this path; model input cannot select a binary.
        let executable = worker_path
            .map(Path::to_path_buf)
            .map(Ok)
            .unwrap_or_else(std::env::current_exe)?;
        let mut command = tokio::process::Command::new(executable);
        command
            .arg(runtime::WORKER_ARG)
            .env_clear()
            .current_dir(&dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        let global_scope = xai_grok_tools::util::global_process_scope();
        global_scope.prepare(&mut command);
        xai_tty_utils::kill_on_parent_death_std(command.as_std_mut());
        #[allow(clippy::disallowed_methods)] // Enrolled in both host and session scopes below.
        let spawned = command.spawn()?;
        let mut worker = WorkerGuard {
            child: Some(spawned),
            permit: Some(permit),
            group: None,
            reaped: false,
        };
        let group = global_scope.enroll(
            worker
                .child
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Missing worker"))?,
        )?;
        if let Some(scope) = &self.tool_context.process_scope {
            anyhow::ensure!(scope.register(&group), "Session process scope is closed");
        }
        worker.group = Some(group);
        let child = worker
            .child
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing worker"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Missing worker input"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Missing worker output"))?;
        let mut frame = Vec::new();
        runtime::write_frame(
            &mut frame,
            &runtime::Init {
                code: input.code,
                catalog,
            },
        )?;
        stdin.write_all(&frame).await?;
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let reader = tokio::spawn(async move {
            loop {
                let message = read_worker_frame(&mut stdout).await;
                let done = matches!(
                    &message,
                    Ok(runtime::WorkerMessage::Finished { .. }) | Err(_)
                );
                if tx.send(message).await.is_err() || done {
                    break;
                }
            }
        });
        let _reader_guard = crate::util::AbortOnDrop(reader);
        type Completion = (u64, String, anyhow::Result<NestedResult>);
        let mut running: FuturesUnordered<LocalBoxFuture<'_, Completion>> = FuturesUnordered::new();
        let locks = Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new()));
        let mut result_budget = ResultBudget(HOST_RESULT_BUDGET);
        let mut seen = HashSet::new();
        let mut admitted_kinds: Vec<(String, Option<ToolKind>)> = Vec::new();
        let finished = loop {
            tokio::select! {
                incoming = rx.recv() => {
                    let message = incoming.ok_or_else(|| anyhow::anyhow!("Code Mode worker disconnected"))??;
                    match message {
                        runtime::WorkerMessage::Call { id, name, arguments } => {
                            anyhow::ensure!(seen.len() < runtime::MAX_CALLS && seen.insert(id) && running.len() < runtime::MAX_IN_FLIGHT, "Code Mode call/concurrency limit or duplicate ID");
                            let child_id = format!("{}/code/{}", call.id, seen.len());
                            trace.start(&child_id, &name, &arguments)?;
                            let frozen = inventory.get(&name).cloned();
                            // Media caps apply per run: nested calls never pass the outer
                            // response's batch check.
                            let kind = self.agent.borrow().tool_bridge().tool_kind(&name);
                            admitted_kinds.push((name.clone(), kind));
                            let capped = !xai_grok_tools::media_gen_limits::over_cap_by_name(
                                admitted_kinds.iter().map(|(n, k)| (n.as_str(), *k)),
                                &self.rebuild_spec.media_gen_batch_limits,
                            )
                            .is_empty();
                            if capped {
                                admitted_kinds.pop();
                            }
                            let parent = call.id.clone();
                            let locks = locks.clone();
                            running.push(async move {
                                let result = if capped {
                                    Err(anyhow::anyhow!("Media generation limit for `{name}` reached in this execute run"))
                                } else {
                                    self.execute_nested_code_call(&parent, &child_id, &name, arguments, frozen, model, locks).await
                                };
                                (id, child_id, result)
                            }.boxed_local());
                        }
                        done @ runtime::WorkerMessage::Finished { .. } => break done,
                    }
                }
                Some((id, child_id, result)) = running.next(), if !running.is_empty() => {
                    let (payload, failed, messages, tool_loop, prepared) = match result {
                        Ok(result) => result,
                        Err(error) => (error.to_string(), true, Vec::new(), None, None),
                    };
                    let row = trace.rows.get_mut(&child_id).ok_or_else(|| anyhow::anyhow!("Unknown nested call"))?;
                    row["status"] = json!(if failed { "failed" } else { "completed" });
                    if let Err(error) = serde_json::to_writer(&mut result_budget, &messages) {
                        trace.event(&json!({"event":"result","id":child_id,"failed":failed,"prepared":prepared,"output_omitted":"aggregate host result budget exceeded"}))?;
                        return Err(error.into());
                    }
                    trace.event(&json!({"event":"result","id":child_id,"failed":failed,"output":payload,"prepared":prepared,"messages":messages}))?;
                    for message in messages {
                        if let ConversationItem::ToolResult(result) = &message { delivery.images.extend(result.images.clone()); }
                        else { delivery.followups.push(message); }
                    }
                    if let Some(tool_loop @ (ToolLoop::Cancelled | ToolLoop::PermissionReject { .. } | ToolLoop::FollowupMessage(_))) = tool_loop {
                        *final_loop = tool_loop;
                        anyhow::bail!("Code Mode interrupted by tool permission decision");
                    }
                    let reply = runtime::Reply { id, ok: !failed, payload: if failed { payload } else { serde_json::to_string(&payload)? } };
                    let mut frame = Vec::new();
                    runtime::write_frame(&mut frame, &reply)?;
                    stdin.write_all(&frame).await?;
                }
            }
        };
        // Close admission by leaving the receive loop, then abort owned host futures.
        // Tool dispatchers own their AbortOnDrop guards. External effects may remain unknown.
        drop(running);
        let _ = child.start_kill();
        let cleanup_incomplete = !matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await,
            Ok(Ok(_))
        );
        if !cleanup_incomplete {
            worker.reaped = true;
            worker.group.take();
            worker.permit.take();
        }
        trace.event(&json!({"event":"finished","result":finished}))?;
        let mut result = serde_json::to_value(finished)?;
        if let Some(object) = result.as_object_mut() {
            object.remove("type");
            if cleanup_incomplete
                && let Some(warnings) = object
                    .entry("warnings")
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
            {
                warnings.push(json!(
                    "Worker cleanup is still pending; its slot remains held until process exit"
                ));
            }
            let mut calls = trace.rows.values().collect::<Vec<_>>();
            calls.sort_by_key(|row| row.get("sequence").and_then(serde_json::Value::as_u64));
            object.insert("calls".to_owned(), json!(calls));
            if trace.rows.values().any(|row| {
                row.get("status")
                    .is_some_and(|status| status == "outcome_unknown")
            }) && let Some(warnings) = object
                .entry("warnings")
                .or_insert_with(|| json!([]))
                .as_array_mut()
            {
                warnings.push(json!("Some unawaited calls have unknown outcome; completed effects are not rolled back"));
            }
        }
        Ok(result)
    }

    /// Everything the session can call now, keyed by client name: the declared
    /// builtins plus, while `use_tool` is offered, every MCP and managed-gateway tool
    /// from the same snapshot `search_tool` reads (same `server__tool` names).
    async fn code_mode_inventory(&self) -> BTreeMap<String, ToolDefinition> {
        let bridge = self.agent.borrow().tool_bridge().clone();
        let kind_of = |name: &str| bridge.tool_kind(name);
        let declared = self.prepare_tool_definitions_inner().await;
        let mcp_reachable = declared
            .iter()
            .any(|d| kind_of(&d.function.name) == Some(ToolKind::UseTool));
        let mut inventory: BTreeMap<_, _> = declared
            .into_iter()
            .filter(|d| composable(&d.function.name, kind_of(&d.function.name)))
            .map(|d| (d.function.name.clone(), d))
            .collect();
        if mcp_reachable {
            for tool in self.mcp_tools() {
                inventory
                    .entry(tool.qualified_name.clone())
                    .or_insert_with(|| {
                        ToolDefinition::function(
                            tool.qualified_name,
                            Some(tool.description),
                            tool.input_schema,
                        )
                    });
            }
        }
        inventory
    }
    fn mcp_tools(&self) -> Vec<crate::session::tool_index::ToolMetadata> {
        self.tool_metadata_snapshot
            .lock()
            .map(|snapshot| snapshot.tools.clone())
            .unwrap_or_default()
    }
    /// MCP tools run as an inline `use_tool` call so MCP auth retry, argument
    /// coercion, permissions, hooks and telemetry stay on their one existing path.
    fn nested_call_function(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<crate::sampling::types::ToolCallFunction> {
        if !self
            .mcp_tools()
            .iter()
            .any(|tool| tool.qualified_name == name)
        {
            return Ok(crate::sampling::types::ToolCallFunction::new(
                name,
                serde_json::to_string(&arguments)?,
            ));
        }
        let toolset = self.agent.borrow().tool_bridge().toolset();
        let wrapper = toolset
            .tool_name_for_kind(ToolKind::UseTool)
            .ok_or_else(|| anyhow::anyhow!("MCP tools are unavailable in this session"))?;
        let input = xai_grok_tools::implementations::UseToolInput::Inline(
            xai_grok_tools::implementations::use_tool::InlineMcpInvocation {
                tool_name: name.to_owned(),
                tool_input: arguments,
            },
        );
        let wire = toolset
            .model_mcp_arguments(&wrapper, &input)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(crate::sampling::types::ToolCallFunction::new(
            wrapper,
            serde_json::to_string(&wire)?,
        ))
    }
    #[allow(clippy::too_many_arguments)]
    async fn execute_nested_code_call(
        &self,
        parent: &str,
        id: &str,
        name: &str,
        arguments: serde_json::Value,
        frozen: Option<ToolDefinition>,
        model: Option<&str>,
        locks: FileLocks,
    ) -> anyhow::Result<NestedResult> {
        let sink = Rc::new(NestedSink {
            parent: parent.to_owned(),
            locks,
            ..Default::default()
        });
        NESTED
            .scope(sink.clone(), async {
                let frozen = frozen.ok_or_else(|| {
                    anyhow::anyhow!("Unknown or ineligible tool; use catalog.search()")
                })?;
                let current = self.code_mode_inventory().await;
                anyhow::ensure!(
                    current.get(name).is_some_and(|d| {
                        serde_json::to_value(d).ok() == serde_json::to_value(&frozen).ok()
                    }),
                    "Tool inventory changed; start a new execute"
                );
                let call = crate::sampling::types::ToolCallResponse {
                    id: id.to_owned(),
                    kind: "function".to_owned(),
                    function: self.nested_call_function(name, arguments)?,
                };
                let mut followups = Vec::new();
                let mut outcome = None;
                self.execute_tool_calls_batch(vec![call], &mut followups, &mut outcome, model)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                let mut results = sink.results.take();
                let text = results
                    .iter()
                    .map(ConversationItem::text_content)
                    .collect::<Vec<_>>()
                    .join("\n");
                let failed = sink.failed.get()
                    || outcome
                        .as_ref()
                        .is_some_and(|v| !matches!(v, ToolLoop::Continue));
                results.extend(followups);
                Ok((text, failed, results, outcome, sink.prepared.take()))
            })
            .await
    }
}

async fn read_worker_frame(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
) -> anyhow::Result<runtime::WorkerMessage> {
    let size = reader.read_u32_le().await? as usize;
    anyhow::ensure!(
        size <= runtime::MAX_FRAME_BYTES,
        "Code Mode frame exceeds limit"
    );
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
#[path = "code_mode_tests.rs"]
mod tests;
