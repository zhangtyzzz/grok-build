use super::*;
use crate::session::acp_session::support::*;

type Locks =
    Arc<parking_lot::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>;

async fn fixture() -> (SessionActor, tempfile::TempDir) {
    let (gateway, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let (persistence, _persist_rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = create_test_actor(0, 256_000, 85, gateway, persistence).await;
    *actor.agent.borrow_mut() = test_agent_with_tools(read_and_edit_toolset()).await;
    actor
        .workspace_ops
        .bind_local_session(
            &actor.session_id_string(),
            actor.tool_context.cwd.as_path().to_path_buf(),
            actor.tool_context.hunk_tracker_handle.clone(),
            actor.tool_bridge_handle().toolset(),
            None,
        )
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("note.txt"), "nested-needle\n").unwrap();
    (actor, directory)
}
async fn nested(
    actor: &SessionActor,
    name: &str,
    input: serde_json::Value,
) -> (
    String,
    bool,
    Vec<ConversationItem>,
    Option<ToolLoop>,
    Option<serde_json::Value>,
) {
    let definition = actor
        .prepare_tool_definitions_inner()
        .await
        .into_iter()
        .find(|d| d.function.name == name);
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        actor.execute_nested_code_call(
            "outer",
            "outer/code/1",
            name,
            input,
            definition,
            None,
            Locks::default(),
        ),
    )
    .await
    .expect("must not deadlock")
    .unwrap()
}
#[tokio::test(flavor = "current_thread")]
async fn nested_success_reuses_real_tools_without_orphan_results_and_direct_still_delivers() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (actor, directory) = fixture().await;
            let args = json!({"target_file":directory.path().join("note.txt")});
            let (text, failed, messages, _, prepared) =
                nested(&actor, "read_file", args.clone()).await;
            assert!(!failed, "{text}");
            assert!(text.contains("nested-needle"), "{text}");
            assert!(
                prepared.unwrap()["tool_id"]
                    .as_str()
                    .unwrap()
                    .contains("read_file")
            );
            assert!(
                messages
                    .iter()
                    .any(|m| matches!(m, ConversationItem::ToolResult(_)))
            );
            assert!(
                actor
                    .chat_state_handle
                    .get_conversation()
                    .await
                    .iter()
                    .all(|m| !matches!(m, ConversationItem::ToolResult(_)))
            );
            let call = crate::sampling::types::ToolCallResponse {
                id: "direct".into(),
                kind: "function".into(),
                function: crate::sampling::types::ToolCallFunction::new(
                    "read_file",
                    args.to_string(),
                ),
            };
            actor
                .execute_regular_tool_calls(vec![call], None)
                .await
                .unwrap();
            assert!(
                tool_result_text(&actor, "direct")
                    .await
                    .contains("nested-needle")
            );
            assert!(
                parent_id().is_none(),
                "nested sink must not leak into direct call"
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn nested_parse_and_execution_errors_do_not_enter_model_history() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (actor, directory) = fixture().await;
            for input in [
                json!({"target_file":false}),
                json!({"target_file":directory.path().join("missing.txt")}),
            ] {
                let (text, failed, _, _, _) = nested(&actor, "read_file", input).await;
                assert!(failed, "{text}");
            }
            assert!(
                actor
                    .chat_state_handle
                    .get_conversation()
                    .await
                    .iter()
                    .all(|m| !matches!(m, ConversationItem::ToolResult(_)))
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn nested_edits_respect_existing_plan_mode_gate() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (actor, directory) = fixture().await;
            activate_plan_mode(&actor);
            let path = directory.path().join("note.txt");
            let (text, failed, _, _, _) = nested(
                &actor,
                "search_replace",
                json!({"file_path":path,"old_string":"nested-needle","new_string":"changed"}),
            )
            .await;
            assert!(failed, "{text}");
            assert!(text.to_lowercase().contains("plan"), "{text}");
            assert_eq!(std::fs::read_to_string(path).unwrap(), "nested-needle\n");
        })
        .await;
}
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn nested_calls_run_pre_tool_hooks_and_denial_blocks_dispatch() {
    tokio::task::LocalSet::new().run_until(async {
        let (mut actor,directory)=fixture().await;
        install_pre_tool_use_hooks(&mut actor,vec![pre_tool_use_spec("deny-nested",Some("read_file"),"printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"fixture denied\"}}'")]);
        let (text,failed,_,_,_)=nested(&actor,"read_file",json!({"target_file":directory.path().join("note.txt")})).await;
        assert!(failed,"{text}");
        assert!(!text.contains("nested-needle"));
        assert!(text.contains("fixture denied"),"{text}");
    }).await;
}
#[tokio::test(flavor = "current_thread")]
async fn per_run_file_locks_are_shared_across_nested_scopes() {
    let locks = Locks::default();
    let first = Rc::new(NestedSink {
        parent: "outer".into(),
        locks: locks.clone(),
        ..Default::default()
    });
    let second = Rc::new(NestedSink {
        parent: "outer".into(),
        locks,
        ..Default::default()
    });
    let a = NESTED
        .scope(first, async { file_lock("file").unwrap() })
        .await;
    let b = NESTED
        .scope(second, async { file_lock("file").unwrap() })
        .await;
    assert!(Arc::ptr_eq(&a, &b));
    let held = a.lock().await;
    assert!(b.try_lock().is_err());
    drop(held);
    assert!(b.try_lock().is_ok());
    assert!(file_lock("file").is_none());
}
#[test]
fn only_explicit_leaf_kinds_are_composable() {
    for kind in [
        ToolKind::Task,
        ToolKind::Workflow,
        ToolKind::UseTool,
        ToolKind::SearchTool,
        ToolKind::EnterPlan,
        ToolKind::ExitPlan,
        ToolKind::AskUser,
        ToolKind::Other,
    ] {
        assert!(!eligible(Some(kind)), "{kind:?}");
    }
    assert!(!eligible(None));
    for kind in [ToolKind::Read, ToolKind::Edit, ToolKind::Execute] {
        assert!(eligible(Some(kind)));
    }
}
#[tokio::test(flavor = "current_thread")]
async fn changed_catalog_and_control_tools_are_rejected_before_dispatch() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (actor, _) = fixture().await;
            let mut frozen = actor
                .prepare_tool_definitions_inner()
                .await
                .into_iter()
                .find(|d| d.function.name == "read_file")
                .unwrap();
            frozen.function.parameters = json!({"changed":true});
            assert!(
                actor
                    .execute_nested_code_call(
                        "outer",
                        "child",
                        "read_file",
                        json!({}),
                        Some(frozen),
                        None,
                        Locks::default()
                    )
                    .await
                    .is_err()
            );
            assert!(
                actor
                    .execute_nested_code_call(
                        "outer",
                        "child",
                        "execute",
                        json!({}),
                        None,
                        None,
                        Locks::default()
                    )
                    .await
                    .is_err()
            );
            assert!(actor.chat_state_handle.get_conversation().await.is_empty());
        })
        .await;
}
#[test]
fn trace_keeps_unfinished_outcomes_on_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace.jsonl");
    {
        let mut trace = RunTrace {
            file: std::fs::File::create(&path).unwrap(),
            rows: BTreeMap::new(),
            finished: false,
        };
        trace.start("child", "test", &json!({})).unwrap();
    }
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines.last().unwrap()["unfinished"][0]["status"],
        "outcome_unknown"
    );
}

// Run after `cargo build -p xai-grok-tools --example code_mode_worker`.
// Explicit executable injection is private to the host, never model-controlled.
#[ignore = "requires the standalone code_mode_worker example; see docs/code-mode.md"]
#[tokio::test(flavor = "current_thread")]
async fn real_worker_and_session_round_trip_export_only_outer_result() {
    tokio::task::LocalSet::new().run_until(async {
        let worker=std::env::var_os("GROK_TEST_CODE_MODE_WORKER").expect("set absolute worker example path");
        let (mut actor,directory)=fixture().await;
        actor.session_info.id=acp::SessionId::new(format!("code-mode-test-{}",uuid::Uuid::new_v4()));
        actor.workspace_ops.bind_local_session(&actor.session_id_string(),actor.tool_context.cwd.as_path().to_path_buf(),actor.tool_context.hunk_tracker_handle.clone(),actor.tool_bridge_handle().toolset(),None).unwrap();
        let path=directory.path().join("note.txt");
        let path_json=serde_json::to_string(&path).unwrap();
        let code=format!("const paths=[{path_json},{path_json}]; const r=await Promise.all(paths.map(p=>tools.read_file({{target_file:p}}))); return r.filter(s=>s.includes('nested-needle')).length;");
        let request=crate::sampling::types::ToolCallResponse{id:"outer-worker".into(),kind:"function".into(),function:crate::sampling::types::ToolCallFunction::new("execute",json!({"code":code}).to_string())};
        actor.chat_state_handle.push_assistant_response(ConversationItem::assistant_tool_calls(vec![xai_grok_sampling_types::ToolCall {
            id: request.id.clone().into(), name: request.function.name.clone(), arguments: request.function.arguments.clone().into(),
        }]));
        actor.execute_code_mode(request,None,Some(Path::new(&worker))).await.unwrap();
        let result:serde_json::Value=serde_json::from_str(&tool_result_text(&actor,"outer-worker").await).unwrap();
        assert_eq!(result["ok"],true,"{result}");
        assert_eq!(result["value"],2);
        assert_eq!(result["calls"].as_array().unwrap().len(),2);
        let conversation=actor.chat_state_handle.get_conversation().await;
        let ids:Vec<_>=conversation.iter().filter_map(|m|if let ConversationItem::ToolResult(r)=m{Some(r.tool_call_id.as_str())}else{None}).collect();
        assert_eq!(ids,vec!["outer-worker"]);
        assert!(!xai_grok_sampling_types::has_dangling_tool_calls(&conversation));
        let dir=crate::session::persistence::session_dir(&actor.session_info);
        let trace=std::fs::read_to_string(dir.join(result["trace"].as_str().unwrap())).unwrap();
        assert!(trace.contains("nested-needle"));
        assert!(trace.contains("outer-worker/code/1"));
        std::fs::remove_dir_all(dir).unwrap();
    }).await;
}

#[test]
fn aggregate_host_result_budget_covers_images_and_stops_before_buffer_growth() {
    let mut budget = ResultBudget(32);
    assert!(serde_json::to_writer(&mut budget, &json!({"image":"x".repeat(64)})).is_err());
}

#[ignore = "requires the standalone code_mode_worker example; see docs/code-mode.md"]
#[tokio::test(flavor = "current_thread")]
async fn real_worker_cancellation_reaps_process_and_releases_slot() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let worker = std::env::var_os("GROK_TEST_CODE_MODE_WORKER")
                .expect("set absolute worker example path");
            let (mut actor, _directory) = fixture().await;
            actor.session_info.id =
                acp::SessionId::new(format!("code-mode-cancel-{}", uuid::Uuid::new_v4()));
            let request = crate::sampling::types::ToolCallResponse {
                id: "cancel-worker".into(),
                kind: "function".into(),
                function: crate::sampling::types::ToolCallFunction::new(
                    "execute",
                    json!({"code":"for(;;){}"}).to_string(),
                ),
            };
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    actor.execute_code_mode(request, None, Some(Path::new(&worker)))
                )
                .await
                .is_err()
            );
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while WORKERS.available_permits() != 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("slot must remain held only until actual child exit");
            let dir = crate::session::persistence::session_dir(&actor.session_info);
            std::fs::remove_dir_all(dir).unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn nested_reads_respect_real_permission_policy_denial() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use xai_grok_workspace::permission::types::{
                PatternMode, PermissionConfig, PermissionRule, RuleAction, ToolFilter,
            };
            use xai_grok_workspace::permission::{ClientType, spawn_permission_manager};
            let (mut actor, directory) = fixture().await;
            let (gateway, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let policy = PermissionConfig::new(vec![PermissionRule {
                action: RuleAction::Deny,
                tool: ToolFilter::Read,
                pattern: None,
                pattern_mode: PatternMode::Glob,
            }]);
            let (permissions, _events) = spawn_permission_manager(
                actor.session_info.id.clone(),
                xai_acp_lib::AcpAgentGatewaySender::new(gateway),
                actor.tool_context.cwd.clone(),
                ClientType::Generic,
                Some(policy),
                vec![],
                vec![],
                false,
                None,
            );
            actor.permissions = permissions;
            let (text, failed, _, _, _) = nested(
                &actor,
                "read_file",
                json!({"target_file":directory.path().join("note.txt")}),
            )
            .await;
            assert!(failed, "{text}");
            assert!(
                !text.contains("nested-needle"),
                "denied read must not dispatch"
            );
            assert!(
                actor
                    .chat_state_handle
                    .get_conversation()
                    .await
                    .iter()
                    .all(|m| !matches!(m, ConversationItem::ToolResult(_)))
            );
        })
        .await;
}

#[ignore = "requires the standalone code_mode_worker example; see docs/code-mode.md"]
#[tokio::test(flavor = "current_thread")]
async fn real_worker_failure_preserves_completed_write_and_trace() {
    tokio::task::LocalSet::new().run_until(async {
        let worker=std::env::var_os("GROK_TEST_CODE_MODE_WORKER").expect("set absolute worker example path");
        let (mut actor,directory)=fixture().await;
        actor.session_info.id=acp::SessionId::new(format!("code-mode-write-{}",uuid::Uuid::new_v4()));
        actor.workspace_ops.bind_local_session(&actor.session_id_string(),actor.tool_context.cwd.as_path().to_path_buf(),actor.tool_context.hunk_tracker_handle.clone(),actor.tool_bridge_handle().toolset(),None).unwrap();
        let path=directory.path().join("note.txt");
        let path_json=serde_json::to_string(&path).unwrap();
        let code=format!("await tools.search_replace({{file_path:{path_json},old_string:'nested-needle',new_string:'committed'}}); throw new Error('after-write');");
        let request=crate::sampling::types::ToolCallResponse{id:"outer-write".into(),kind:"function".into(),function:crate::sampling::types::ToolCallFunction::new("execute",json!({"code":code}).to_string())};
        actor.execute_code_mode(request,None,Some(Path::new(&worker))).await.unwrap();
        let result:serde_json::Value=serde_json::from_str(&tool_result_text(&actor,"outer-write").await).unwrap();
        assert_eq!(result["ok"],false,"{result}");
        assert!(result["error"].as_str().unwrap().contains("after-write"),"{result}");
        assert_eq!(std::fs::read_to_string(path).unwrap(),"committed\n");
        assert_eq!(result["calls"][0]["status"],"completed");
        let dir=crate::session::persistence::session_dir(&actor.session_info);
        let trace=std::fs::read_to_string(dir.join(result["trace"].as_str().unwrap())).unwrap();
        assert!(trace.contains("committed"));
        std::fs::remove_dir_all(dir).unwrap();
    }).await;
}

#[ignore = "requires the standalone code_mode_worker example; see docs/code-mode.md"]
#[tokio::test(flavor = "current_thread")]
async fn real_worker_session_scope_can_reclaim_busy_worker() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let worker = std::env::var_os("GROK_TEST_CODE_MODE_WORKER")
                .expect("set absolute worker example path");
            let (mut actor, _directory) = fixture().await;
            actor.session_info.id =
                acp::SessionId::new(format!("code-mode-scope-{}", uuid::Uuid::new_v4()));
            let scope = xai_tty_utils::ProcessScope::new();
            actor.tool_context.process_scope = Some(scope.clone());
            let request = crate::sampling::types::ToolCallResponse {
                id: "scope-worker".into(),
                kind: "function".into(),
                function: crate::sampling::types::ToolCallFunction::new(
                    "execute",
                    json!({"code":"for(;;){}"}).to_string(),
                ),
            };
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                let (outcome, ()) = tokio::join!(
                    actor.execute_code_mode(request, None, Some(Path::new(&worker))),
                    async {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        scope.kill_all();
                    }
                );
                outcome.unwrap();
            })
            .await
            .expect("session supervisor must reclaim the busy worker");
            let result: serde_json::Value =
                serde_json::from_str(&tool_result_text(&actor, "scope-worker").await).unwrap();
            assert_eq!(result["ok"], false, "{result}");
            let dir = crate::session::persistence::session_dir(&actor.session_info);
            std::fs::remove_dir_all(dir).unwrap();
        })
        .await;
}
