//! Mixed plan.md edit + exit: approval snapshot matches the post-edit plan body.

use super::support::*;
use super::*;
use agent_client_protocol as acp;
use xai_grok_tools::implementations::grok_build::exit_plan_mode::ExitPlanModeExtRequest;

const SEED_PLAN: &str = "# OLD mixed-batch plan seed unique-c91e04";
const NEW_PLAN: &str = "# NEW mixed-batch plan body unique-a7f3c2";

fn ext_response(outcome: &str) -> Arc<serde_json::value::RawValue> {
    serde_json::value::to_raw_value(&serde_json::json!({ "outcome": outcome }))
        .unwrap()
        .into()
}

fn search_replace_plan(id: &str, plan_path: &str) -> ToolCallResponse {
    ToolCallResponse {
        id: id.to_string(),
        kind: "function".to_string(),
        function: crate::sampling::types::ToolCallFunction::new(
            "search_replace",
            serde_json::json!({
                "file_path": plan_path,
                "old_string": SEED_PLAN,
                "new_string": NEW_PLAN,
            })
            .to_string(),
        ),
    }
}

fn exit_plan_mode_call(id: &str) -> ToolCallResponse {
    ToolCallResponse {
        id: id.to_string(),
        kind: "function".to_string(),
        function: crate::sampling::types::ToolCallFunction::new("exit_plan_mode", "{}"),
    }
}

fn install_plan_transition_barrier_stub(actor: &mut SessionActor) -> tokio::task::JoinHandle<()> {
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    actor.tool_context.session_cmd_tx = Some(cmd_tx);
    tokio::task::spawn_local(async move {
        while let Some(command) = cmd_rx.recv().await {
            let SessionCommand::ApplyPlanToolTransition { responds_to, .. } = command else {
                panic!("unexpected command in plan transition worker");
            };
            if let Some(responds_to) = responds_to {
                let _ = responds_to.send(Ok(()));
            }
        }
    })
}

async fn seeded_active_plan_actor_with_edit_tools() -> (
    SessionActor,
    tokio::sync::mpsc::UnboundedReceiver<xai_acp_lib::AcpClientMessage>,
    tokio::task::JoinHandle<()>,
    tempfile::TempDir,
    std::path::PathBuf,
) {
    use xai_grok_tools::implementations::grok_build::enter_plan_mode::EnterPlanModeTool;
    use xai_grok_tools::implementations::grok_build::exit_plan_mode::ExitPlanModeTool;
    use xai_grok_tools::registry::types::ToolConfig;

    let (gateway_tx, gateway_rx) =
        tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
    let (persistence_tx, persistence_rx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
    spawn_test_persistence_acknowledger(persistence_rx);
    let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
    *actor.agent.borrow_mut() = test_agent_with_tools(vec![
        ToolConfig::from_id("GrokBuild:read_file"),
        ToolConfig {
            id: "GrokBuild:search_replace".into(),
            params: Some(
                serde_json::from_value(serde_json::json!({
                    "skip_read_before_edit": true
                }))
                .unwrap(),
            ),
            name_override: None,
            params_name_overrides: None,
            description_override: None,
            behavior_version: None,
            kind: None,
        },
        ToolConfig::for_tool::<EnterPlanModeTool>(),
        ToolConfig::for_tool::<ExitPlanModeTool>(),
    ])
    .await;

    let dir = tempfile::tempdir().unwrap();
    // macOS tempdirs live under the `/var` symlink; the protected plan-file
    // reader walks parents with O_NOFOLLOW, so use the canonical path.
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let plan_path = root.join("plan.md");
    std::fs::write(&plan_path, SEED_PLAN).unwrap();
    {
        let mut tracker = actor.plan_mode.lock();
        *tracker = crate::session::plan_mode::PlanModeTracker::new(root);
        tracker.activate_from_tool();
    }
    actor
        .agent
        .borrow()
        .tool_bridge()
        .update_resource(xai_grok_tools::types::resources::PlanFilePath(
            plan_path.clone(),
        ))
        .await;
    // Phase-2 file tools dispatch through workspace_ops; without a bound
    // session, search_replace hard-errors before writing plan.md.
    actor
        .workspace_ops
        .bind_local_session(
            &actor.session_id_string(),
            actor.tool_context.cwd.as_path().to_path_buf(),
            actor.tool_context.hunk_tracker_handle.clone(),
            actor.agent.borrow().tool_bridge().toolset(),
            None,
        )
        .expect("bind_local_session must succeed");
    let transition_worker = install_plan_transition_barrier_stub(&mut actor);

    (actor, gateway_rx, transition_worker, dir, plan_path)
}

fn spawn_exit_capture(
    mut gateway_rx: tokio::sync::mpsc::UnboundedReceiver<xai_acp_lib::AcpClientMessage>,
) -> (
    tokio::task::JoinHandle<()>,
    std::sync::Arc<std::sync::Mutex<Option<Option<String>>>>,
) {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<Option<String>>));
    let captured_for_task = captured.clone();
    let handle = tokio::task::spawn_local(async move {
        while let Some(msg) = gateway_rx.recv().await {
            match msg {
                xai_acp_lib::AcpClientMessage::ExtMethod(args) => {
                    if args.request.method.as_ref() == "x.ai/exit_plan_mode" {
                        let req: ExitPlanModeExtRequest =
                            serde_json::from_str(args.request.params.get()).unwrap();
                        *captured_for_task.lock().unwrap() = Some(req.plan_content);
                        let _ = args
                            .response_tx
                            .send(Ok(acp::ExtResponse::new(ext_response("approved"))));
                    }
                }
                xai_acp_lib::AcpClientMessage::SessionNotification(args) => {
                    let _ = args.response_tx.send(Ok(()));
                }
                _ => {}
            }
        }
    });
    (handle, captured)
}

async fn assert_mixed_batch_snapshot(write_first: bool) {
    let (actor, gateway_rx, transition_worker, _dir, plan_path) =
        seeded_active_plan_actor_with_edit_tools().await;
    let plan_path_str = plan_path.to_string_lossy().into_owned();
    let (responder, captured) = spawn_exit_capture(gateway_rx);

    let write = search_replace_plan("call_write_plan", &plan_path_str);
    let exit = exit_plan_mode_call("call_exit_plan");
    let batch = if write_first {
        vec![write, exit]
    } else {
        vec![exit, write]
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        actor.execute_tool_calls(batch),
    )
    .await
    .expect("execute_tool_calls must not hang")
    .expect("execute_tool_calls must not error");

    assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), NEW_PLAN);

    let snapshot = captured
        .lock()
        .unwrap()
        .clone()
        .expect("gateway must receive x.ai/exit_plan_mode")
        .expect("x.ai/exit_plan_mode must carry plan content");
    assert_eq!(snapshot, NEW_PLAN);

    responder.abort();
    transition_worker.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn mixed_write_then_exit_snapshot_sees_new_plan() {
    let local = tokio::task::LocalSet::new();
    local.run_until(assert_mixed_batch_snapshot(true)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn mixed_exit_then_write_snapshot_sees_new_plan() {
    let local = tokio::task::LocalSet::new();
    local.run_until(assert_mixed_batch_snapshot(false)).await;
}

fn bash_call(id: &str) -> ToolCallResponse {
    ToolCallResponse {
        id: id.to_string(),
        kind: "function".to_string(),
        function: crate::sampling::types::ToolCallFunction::new(
            "run_terminal_cmd",
            r#"{"command":"echo mixed-batch-reject","description":"probe mixed-batch plan gate rejection"}"#,
        ),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn mixed_plan_gate_rejection_skips_exit_reverse_request() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            use xai_grok_paths::AbsPathBuf;
            use xai_grok_tools::implementations::grok_build::enter_plan_mode::EnterPlanModeTool;
            use xai_grok_tools::implementations::grok_build::exit_plan_mode::ExitPlanModeTool;
            use xai_grok_tools::registry::types::ToolConfig;
            use xai_grok_workspace::permission::{ClientType, spawn_permission_manager};

            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor =
                create_test_actor(0, 256_000, 85, gateway_tx.clone(), persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_tools(vec![
                ToolConfig {
                    id: "GrokBuild:run_terminal_cmd".into(),
                    params: Some(
                        serde_json::from_value(serde_json::json!({
                            "enabled_background": false
                        }))
                        .unwrap(),
                    ),
                    name_override: None,
                    params_name_overrides: None,
                    description_override: None,
                    behavior_version: None,
                    kind: None,
                },
                ToolConfig::for_tool::<EnterPlanModeTool>(),
                ToolConfig::for_tool::<ExitPlanModeTool>(),
            ])
            .await;

            let dir = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(dir.path()).unwrap();
            let plan_path = root.join("plan.md");
            std::fs::write(&plan_path, SEED_PLAN).unwrap();
            {
                let mut tracker = actor.plan_mode.lock();
                *tracker = crate::session::plan_mode::PlanModeTracker::new(root);
                tracker.activate_from_tool();
            }
            actor
                .agent
                .borrow()
                .tool_bridge()
                .update_resource(xai_grok_tools::types::resources::PlanFilePath(plan_path))
                .await;

            let cwd = AbsPathBuf::new(std::path::PathBuf::from(actor.session_info.cwd.clone()))
                .unwrap_or_else(|_| AbsPathBuf::new(std::path::PathBuf::from("/tmp")).unwrap());
            let (perms, _ev) = spawn_permission_manager(
                actor.session_info.id.clone(),
                xai_acp_lib::AcpAgentGatewaySender::new(gateway_tx),
                cwd,
                ClientType::Generic,
                None,
                vec![],
                vec![],
                false,
                None,
            );
            actor.permissions = perms;
            let exit_fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let exit_fired_task = exit_fired.clone();
            let responder = tokio::task::spawn_local(async move {
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        xai_acp_lib::AcpClientMessage::RequestPermission(args) => {
                            let _ = args
                                .response_tx
                                .send(Ok(acp::RequestPermissionResponse::new(
                                    acp::RequestPermissionOutcome::Cancelled,
                                )));
                        }
                        xai_acp_lib::AcpClientMessage::ExtMethod(args) => {
                            if args.request.method.as_ref() == "x.ai/exit_plan_mode" {
                                exit_fired_task.store(true, std::sync::atomic::Ordering::SeqCst);
                                let _ = args
                                    .response_tx
                                    .send(Ok(acp::ExtResponse::new(ext_response("approved"))));
                            }
                        }
                        xai_acp_lib::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                actor.execute_tool_calls(vec![
                    bash_call("call_bash_reject"),
                    exit_plan_mode_call("call_exit"),
                ]),
            )
            .await
            .expect("execute_tool_calls must not hang")
            .expect("execute_tool_calls must not error");

            assert!(
                !exit_fired.load(std::sync::atomic::Ordering::SeqCst),
                "exit must not reverse-request after an earlier plan gate rejection"
            );
            responder.abort();
        })
        .await;
}
