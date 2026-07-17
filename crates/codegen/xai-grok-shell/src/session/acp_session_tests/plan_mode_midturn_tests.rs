//! Mid-turn plan-mode toggle: `handle_session_mode("plan")` while a turn is
//! running must activate the tracker immediately and buffer the activation
//! reminder for the running turn (previously, toggling plan mode while
//! the model is thinking was ignored until the next prompt, so the model
//! jumped straight into implementation).
use super::support::*;
use super::*;
use xai_grok_tools::types::output::ToolOutput;

/// Park a fake in-flight turn on the actor so `handle_session_mode` sees
/// `running_task.is_some()`. The never-completing task is torn down when the
/// test's `LocalSet` is dropped.
async fn fake_running_turn(actor: &SessionActor) {
    actor.state.lock().await.running_task = Some(AgentTask {
        prompt_id: "running-turn".into(),
        handle: tokio::task::spawn_local(std::future::pending::<()>()).abort_handle(),
    });
}

/// Toggling plan mode ON mid-turn activates immediately (Pending is skipped)
/// and buffers the activation reminder on the tracker; the flush delivers it
/// into the conversation as a `<system-reminder>` user message and only then
/// advances the full/sparse alternation.
#[tokio::test]
async fn midturn_plan_toggle_activates_and_buffers_reminder() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;
            fake_running_turn(&actor).await;

            actor
                .handle_session_mode(acp::SessionModeId::new("plan"))
                .await
                .expect("mock persistence acknowledges plan entry");

            {
                let tracker = actor.plan_mode.lock();
                assert_eq!(
                    tracker.state(),
                    crate::session::plan_mode::PlanModeState::Active,
                    "mid-turn toggle must activate immediately, not park in Pending"
                );
                assert!(tracker.has_pending_activation());
                // Recorded at delivery, not buffer time.
                assert!(tracker.should_use_full_reminder());
            }
            // Buffered — not pushed directly into the conversation (a direct
            // push could interleave with an in-flight tool batch).
            assert_eq!(
                actor.chat_state_handle.get_conversation_len().await,
                0,
                "reminder must be buffered, not pushed mid-batch"
            );

            // The running turn's next safe point delivers the buffer.
            actor.flush_pending_skill_reminders().await;
            let conv = actor.chat_state_handle.get_conversation().await;
            assert_eq!(conv.len(), 1);
            let text = conv[0].text_content();
            assert!(
                text.contains("<system-reminder>"),
                "reminder must be system-reminder wrapped: {text}"
            );
            assert!(
                text.contains("Plan mode is active"),
                "reminder must carry the plan-mode activation text: {text}"
            );
            {
                let tracker = actor.plan_mode.lock();
                assert!(!tracker.has_pending_activation());
                // Delivery advanced the alternation: next injection is sparse.
                assert!(!tracker.should_use_full_reminder());
            }

            // Exactly-once: the turn flushes at multiple safe points (loop
            // top, after each tool batch, cancel/idle) — later flushes must
            // not deliver the reminder again.
            actor.flush_pending_skill_reminders().await;
            actor.flush_pending_skill_reminders().await;
            assert_eq!(
                actor.chat_state_handle.get_conversation_len().await,
                1,
                "repeated flushes must not duplicate the activation reminder"
            );
        })
        .await;
}

/// Idle toggle keeps the existing deferred behavior: tracker parks in
/// `Pending` and the reminder is injected at the next turn start
/// (`inject_plan_mode_reminders`), not buffered here.
#[tokio::test]
async fn idle_plan_toggle_stays_pending_without_buffered_reminder() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;

            actor
                .handle_session_mode(acp::SessionModeId::new("plan"))
                .await
                .expect("mock persistence acknowledges plan entry");

            {
                let tracker = actor.plan_mode.lock();
                assert_eq!(
                    tracker.state(),
                    crate::session::plan_mode::PlanModeState::Pending,
                    "idle toggle must keep the deferred Pending flow"
                );
                assert!(!tracker.has_pending_activation());
            }
            assert_eq!(actor.chat_state_handle.get_conversation_len().await, 0);
        })
        .await;
}

/// Toggling OFF again before the buffered reminder is delivered withdraws it:
/// the model never saw plan mode, so the activation rolls back cleanly — no
/// stale "plan mode is active" delivery, no deferred exit, no exit reminder.
/// Covers Shift+Tab cycling past Plan (Plan → Auto) mid-turn.
#[tokio::test]
async fn midturn_toggle_off_withdraws_undelivered_reminder() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;
            fake_running_turn(&actor).await;

            actor
                .handle_session_mode(acp::SessionModeId::new("plan"))
                .await
                .expect("mock persistence acknowledges plan entry");
            assert!(actor.plan_mode.lock().has_pending_activation());

            actor
                .handle_session_mode(acp::SessionModeId::new("default"))
                .await
                .expect("mock persistence acknowledges plan exit");
            {
                let tracker = actor.plan_mode.lock();
                assert_eq!(
                    tracker.state(),
                    crate::session::plan_mode::PlanModeState::Inactive,
                    "undelivered activation must roll back to Inactive, not ExitPending"
                );
                assert!(!tracker.has_pending_activation());
                assert!(
                    !tracker.has_pending_exit_reminder(),
                    "no exit reminder for an entry the model never saw"
                );
            }

            // Nothing plan-related reaches the conversation.
            actor.flush_pending_skill_reminders().await;
            assert_eq!(actor.chat_state_handle.get_conversation_len().await, 0);
        })
        .await;
}

/// Once the buffered reminder HAS been delivered, toggling OFF mid-turn takes
/// the normal deferred-exit path (`ExitPending`), and toggling back ON before
/// the turn ends re-enters `Active` without buffering a duplicate reminder.
#[tokio::test]
async fn midturn_reentry_after_delivery_buffers_no_duplicate_reminder() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;
            fake_running_turn(&actor).await;

            // Enter plan mode mid-turn and deliver the reminder (drain point).
            actor
                .handle_session_mode(acp::SessionModeId::new("plan"))
                .await
                .expect("mock persistence acknowledges plan entry");
            actor.flush_pending_skill_reminders().await;
            assert_eq!(actor.chat_state_handle.get_conversation_len().await, 1);

            // Toggle off mid-turn: model saw plan mode → deferred exit.
            actor
                .handle_session_mode(acp::SessionModeId::new("default"))
                .await
                .expect("mock persistence acknowledges deferred exit");
            assert_eq!(
                actor.plan_mode.lock().state(),
                crate::session::plan_mode::PlanModeState::ExitPending
            );

            // Back on before the turn ends: straight to Active, no new buffer.
            actor
                .handle_session_mode(acp::SessionModeId::new("plan"))
                .await
                .expect("mock persistence acknowledges plan reentry");
            {
                let tracker = actor.plan_mode.lock();
                assert_eq!(
                    tracker.state(),
                    crate::session::plan_mode::PlanModeState::Active
                );
                assert!(
                    !tracker.has_pending_activation(),
                    "ExitPending → Active re-entry must not buffer a second reminder"
                );
            }
            assert_eq!(actor.chat_state_handle.get_conversation_len().await, 1);
        })
        .await;
}

fn plan_models_manager(profile_model: &str) -> crate::agent::models::ModelsManager {
    let raw: toml::Value = toml::from_str(&format!(
        r#"
[provider.local]
base_url = "http://localhost"
auth = "none"

[model.executor]
provider = "local"
model = "test"
context_window = 256000

[model.planner]
provider = "local"
model = "planner-upstream"
context_window = 256000

[model.manual]
provider = "local"
model = "manual-upstream"
context_window = 256000

[modes.plan]
model = "{profile_model}"
instructions = "Use the configured planner checklist."
restore_model = true
"#
    ))
    .expect("valid test config");
    let cfg = crate::agent::config::Config::new_from_toml_cfg(&raw).expect("valid model config");
    let models = crate::agent::models::resolve_model_catalog(&cfg, None);
    let auth_root = std::env::temp_dir().join("grok-plan-barrier-auth");
    let auth_manager = std::sync::Arc::new(crate::auth::AuthManager::new(
        &auth_root,
        crate::auth::GrokComConfig::default(),
    ));
    crate::agent::models::ModelsManager::new(
        None,
        models,
        acp::ModelId::new("executor"),
        auth_manager,
        cfg,
    )
}

async fn mark_executor_locator(actor: &SessionActor) {
    let mut config = actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .expect("sampling config");
    config.model_ref = Some("executor".to_owned());
    actor.chat_state_handle.update_sampling_config(config);
    let _ = actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .expect("sampling actor remains live");
}

/// End-to-end regression for the actor barrier: even if the notification
/// command wins the race, the completed EnterPlanMode result waits for a
/// second acknowledged idempotent command. When it returns, the very next
/// sampling request observes the planner model and exactly one profile
/// overlay/UI transition.
#[tokio::test(flavor = "current_thread")]
async fn enter_plan_tool_result_waits_for_planner_model_barrier() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let (mut actor, mut event_rx) =
                create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.models_manager = plan_models_manager("planner");
            mark_executor_locator(&actor).await;

            let (persist_order_tx, mut persist_order_rx) =
                tokio::sync::mpsc::unbounded_channel::<&'static str>();
            tokio::task::spawn_local(async move {
                while let Some(message) = persistence_rx.recv().await {
                    match message {
                        PersistenceMsg::PlanModeStateAndAck { state, respond_to } => {
                            let marker = if state.pending_model_scope.is_some() {
                                "pending"
                            } else if state.model_scope.is_some() {
                                "commit"
                            } else {
                                "state"
                            };
                            let _ = persist_order_tx.send(marker);
                            let _ = respond_to.send(Ok(()));
                        }
                        PersistenceMsg::CurrentModelAndAck { respond_to, .. } => {
                            let _ = persist_order_tx.send("current-model");
                            let _ = respond_to.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
            actor.tool_context.session_cmd_tx = Some(cmd_tx.clone());
            let actor = std::sync::Arc::new(actor);
            let transition_actor = actor.clone();
            let transition_worker = tokio::task::spawn_local(async move {
                let mut outcomes = Vec::new();
                while let Some(command) = cmd_rx.recv().await {
                    let SessionCommand::ApplyPlanToolTransition {
                        entering,
                        responds_to,
                    } = command
                    else {
                        panic!("unexpected command in plan transition worker");
                    };
                    let outcome = transition_actor
                        .apply_plan_tool_transition(entering)
                        .await
                        .expect("mock persistence acknowledges transition");
                    outcomes.push(outcome);
                    if let Some(tx) = responds_to {
                        let _ = tx.send(Ok(()));
                        break;
                    }
                }
                outcomes
            });

            // Simulate the notification bridge winning the race.
            cmd_tx
                .send(SessionCommand::ApplyPlanToolTransition {
                    entering: true,
                    responds_to: None,
                })
                .unwrap();

            let result = ToolRunResult {
                output: ToolOutput::EnterPlanMode(
                    xai_grok_tools::types::output::EnterPlanModeOutput::Entered {
                        message: "entered".into(),
                        plan_file_path: "/tmp/test-session/plan.md".into(),
                        tool_hints: Default::default(),
                        plan_file_seed: Default::default(),
                    },
                ),
                prompt_text: "entered".into(),
                effective_tool_name: None,
            };
            actor
                .handle_bridge_tool_success(
                    &acp::ToolCallId::new("enter-plan-1"),
                    "enter-plan-1",
                    "enter_plan_mode",
                    "enter_plan_mode",
                    result,
                    0,
                    "test",
                    &serde_json::json!({}),
                )
                .await
                .expect("barrier acknowledged");

            assert_eq!(
                transition_worker.await.unwrap(),
                vec![true, false],
                "notification activates once; acknowledged duplicate is an idempotent barrier"
            );
            let sampling = actor
                .chat_state_handle
                .get_sampling_config()
                .await
                .expect("sampling config");
            assert_eq!(sampling.model_ref.as_deref(), Some("planner"));
            assert_eq!(sampling.model, "planner-upstream");
            assert_eq!(
                actor.plan_mode.lock().state(),
                crate::session::plan_mode::PlanModeState::Active
            );
            assert!(actor.plan_mode.lock().pending_model_scope().is_none());
            assert!(actor.plan_mode.lock().model_scope().is_some());

            let overlays = actor.pending_skill_reminders.lock();
            assert_eq!(overlays.len(), 1, "profile overlay must be queued once");
            assert!(
                overlays[0]
                    .text_content()
                    .contains("configured planner checklist")
            );
            drop(overlays);

            let mut mode_updates = 0;
            while let Ok(event) = event_rx.try_recv() {
                if let SessionEvent::Notification(SessionNotification::Acp(notification)) = event
                    && matches!(
                        notification.update,
                        acp::SessionUpdate::CurrentModeUpdate(_)
                    )
                {
                    mode_updates += 1;
                }
            }
            assert_eq!(mode_updates, 1, "UI mode transition must be emitted once");

            let mut pending_index = None;
            let mut current_model_index = None;
            let mut index = 0;
            while let Ok(marker) = persist_order_rx.try_recv() {
                match marker {
                    "pending" => {
                        pending_index.get_or_insert(index);
                    }
                    "current-model" => {
                        current_model_index.get_or_insert(index);
                    }
                    _ => {}
                }
                index += 1;
            }
            let pending_index = pending_index.expect("pending scope persisted");
            let current_model_index = current_model_index.expect("current model persisted");
            assert!(
                pending_index < current_model_index,
                "write-ahead scope must persist before CurrentModel: pending={pending_index} current={current_model_index}"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn failed_plan_model_resolution_still_queues_profile_overlay() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            spawn_test_persistence_acknowledger(_persistence_rx);
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.models_manager = plan_models_manager("missing-model");

            assert!(
                actor
                    .apply_plan_tool_transition(true)
                    .await
                    .expect("mock persistence acknowledges transition")
            );
            assert_eq!(
                actor
                    .chat_state_handle
                    .get_sampling_config()
                    .await
                    .unwrap()
                    .model,
                "test"
            );
            assert_eq!(actor.pending_skill_reminders.lock().len(), 1);
            assert!(!actor.plan_mode.lock().has_any_model_scope());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn closed_persistence_channel_fails_plan_barrier_before_model_switch() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            drop(persistence_rx);
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.models_manager = plan_models_manager("planner");
            mark_executor_locator(&actor).await;

            let error = actor
                .apply_plan_tool_transition(true)
                .await
                .expect_err("closed persistence must fail the transition");
            assert!(
                format!("{error:?}").contains("persistence actor is unavailable"),
                "exact channel failure should reach the caller: {error}"
            );
            let sampling = actor.chat_state_handle.get_sampling_config().await.unwrap();
            assert_eq!(sampling.model_ref.as_deref(), Some("executor"));
            assert!(!actor.plan_mode.lock().has_any_model_scope());
            assert_eq!(
                actor.plan_mode.lock().state(),
                crate::session::plan_mode::PlanModeState::Active,
                "the in-process gate remains fail-closed after the ACK failure"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn slash_plan_first_prompt_injects_configured_profile_overlay() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            spawn_test_persistence_acknowledger(_persistence_rx);
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.models_manager = plan_models_manager("planner");
            mark_executor_locator(&actor).await;

            actor
                .handle_session_mode(acp::SessionModeId::new("plan"))
                .await
                .expect("mock persistence acknowledges plan entry");
            actor
                .inject_plan_mode_reminders()
                .await
                .expect("mock persistence acknowledges activation");

            let conversation = actor.chat_state_handle.get_conversation().await;
            let joined = conversation
                .iter()
                .map(ConversationItem::text_content)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(joined.contains("configured planner checklist"));
            assert_eq!(
                joined.matches("configured planner checklist").count(),
                1,
                "the first /plan prompt must inject one profile overlay"
            );
            assert_eq!(
                actor
                    .chat_state_handle
                    .get_sampling_config()
                    .await
                    .unwrap()
                    .model_ref
                    .as_deref(),
                Some("planner")
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn pending_scope_recovers_and_manual_model_switch_wins_on_exit() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            spawn_test_persistence_acknowledger(_persistence_rx);
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.models_manager = plan_models_manager("planner");
            mark_executor_locator(&actor).await;
            assert!(actor.plan_mode.lock().activate_from_tool());

            let base = crate::session::plan_mode::PlanModelLocator {
                route_ref: None,
                model_ref: Some("executor".into()),
                model: "test".into(),
                base_url: "http://localhost".into(),
            };
            let applied = crate::session::plan_mode::PlanModelLocator {
                route_ref: None,
                model_ref: Some("planner".into()),
                model: "planner-upstream".into(),
                base_url: "http://localhost".into(),
            };
            assert!(actor.plan_mode.lock().prepare_model_scope(base, applied));

            // Simulated restart with the write-ahead record durable but the
            // current model still at the base: entry safely retries.
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.apply_plan_model_scope(true, false),
            )
            .await
            .expect("pending model scope recovery must not hang")
            .expect("pending model scope recovers");
            assert_eq!(
                actor
                    .chat_state_handle
                    .get_sampling_config()
                    .await
                    .unwrap()
                    .model_ref
                    .as_deref(),
                Some("planner")
            );
            assert!(actor.plan_mode.lock().pending_model_scope().is_none());
            assert!(actor.plan_mode.lock().model_scope().is_some());

            let (manual_entry, manual_sampling) = actor
                .models_manager
                .sampling_config_for_model_ref("manual")
                .expect("manual model");
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.handle_set_session_model(
                    manual_sampling,
                    manual_entry.info.use_concise,
                    false,
                    true,
                    85,
                ),
            )
            .await
            .expect("manual model switch must not hang")
            .unwrap();
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    actor.apply_plan_tool_transition(false),
                )
                .await
                .expect("Plan Mode exit must not hang")
                .expect("mock persistence acknowledges exit")
            );
            let sampling = actor.chat_state_handle.get_sampling_config().await.unwrap();
            assert_eq!(sampling.model_ref.as_deref(), Some("manual"));
            assert_eq!(sampling.model, "manual-upstream");
            assert!(!actor.plan_mode.lock().has_any_model_scope());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn committed_scope_with_base_model_reapplies_planner_on_recovery() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            spawn_test_persistence_acknowledger(persistence_rx);
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.models_manager = plan_models_manager("planner");
            mark_executor_locator(&actor).await;
            assert!(actor.plan_mode.lock().activate_from_tool());
            assert!(actor.plan_mode.lock().begin_model_scope(
                crate::session::plan_mode::PlanModelLocator {
                    route_ref: None,
                    model_ref: Some("executor".into()),
                    model: "test".into(),
                    base_url: "http://localhost".into(),
                },
                crate::session::plan_mode::PlanModelLocator {
                    route_ref: None,
                    model_ref: Some("planner".into()),
                    model: "planner-upstream".into(),
                    base_url: "http://localhost".into(),
                },
            ));

            actor
                .apply_plan_model_scope(true, false)
                .await
                .expect("startup recovery reapplies the committed planner");
            let sampling = actor.chat_state_handle.get_sampling_config().await.unwrap();
            assert_eq!(sampling.model_ref.as_deref(), Some("planner"));
            assert!(actor.plan_mode.lock().model_scope().is_some());
        })
        .await;
}
