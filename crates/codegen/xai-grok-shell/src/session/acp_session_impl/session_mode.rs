//! Session/plan-mode concern for `SessionActor` (`handle_session_mode`,
//! plan-mode reminders and persistence, active-template detection).
use super::*;
pub(super) fn prompt_mode_from_session_mode_id(session_mode_id: &acp::SessionModeId) -> PromptMode {
    use xai_grok_tools::types::SessionMode;
    match SessionMode::from_id(session_mode_id.0.as_ref()) {
        SessionMode::Plan => PromptMode::Plan,
        SessionMode::Ask => PromptMode::Ask,
        SessionMode::Default => PromptMode::Agent,
    }
}
/// Pass-through twin: no toolset in this build carries a plan-gated tool.
pub(super) fn filter_cursor_tools_by_plan_mode(
    defs: Vec<ToolDefinition>,
    _plan_active: bool,
) -> Vec<ToolDefinition> {
    defs
}
impl SessionActor {
    pub(super) async fn persist_plan_mode_state_durable(&self) -> Result<(), acp::Error> {
        let state = self.plan_mode.lock().snapshot();
        let (respond_to, response) = tokio::sync::oneshot::channel();
        self.notifications
            .persistence_tx
            .send(PersistenceMsg::PlanModeStateAndAck { state, respond_to })
            .map_err(|_| {
                acp::Error::internal_error()
                    .data("durable plan-mode persistence actor is unavailable")
            })?;
        response
            .await
            .map_err(|_| {
                acp::Error::internal_error()
                    .data("durable plan-mode acknowledgement channel closed")
            })?
            .map_err(|error| {
                acp::Error::internal_error()
                    .data(format!("durable plan-mode write failed: {error}"))
            })
    }

    async fn current_plan_model_locator(
        &self,
    ) -> Option<crate::session::plan_mode::PlanModelLocator> {
        self.chat_state_handle
            .get_sampling_config()
            .await
            .map(|config| crate::session::plan_mode::PlanModelLocator {
                route_ref: config.route_ref,
                model_ref: config.model_ref,
                model: config.model,
                base_url: config.base_url,
            })
    }

    fn sampling_config_for_plan_locator(
        &self,
        locator: &crate::session::plan_mode::PlanModelLocator,
    ) -> Option<(
        crate::agent::config::ModelEntry,
        xai_grok_sampler::SamplerConfig,
    )> {
        if let Some(route_ref) = locator.route_ref.as_deref() {
            self.models_manager.sampling_config_for_model_ref(route_ref)
        } else {
            self.models_manager.sampling_config_for_locator(
                locator.model_ref.as_deref(),
                &locator.model,
                &locator.base_url,
            )
        }
    }

    async fn emit_plan_scoped_model_changed(&self, model_id: &str) {
        let notification = crate::extensions::notification::SessionNotification {
            session_id: self.session_info.id.clone(),
            update: crate::extensions::notification::SessionUpdate::ModelChanged {
                model_id: model_id.to_owned(),
                reasoning_effort: None,
            },
            meta: None,
        };
        if let Ok(params) = serde_json::value::to_raw_value(&notification) {
            self.notifications
                .gateway
                .forward_fire_and_forget(acp::ExtNotification::new(
                    "x.ai/session_notification",
                    params.into(),
                ));
        }
    }

    /// Apply or release `[modes.plan].model` without changing the process-wide
    /// ModelsManager selection (leader mode may host sessions on different
    /// models). Exit uses compare-and-restore so a manual `/model` switch made
    /// while planning is never overwritten.
    pub(super) async fn apply_plan_model_scope(
        &self,
        entering: bool,
        inject_overlay: bool,
    ) -> Result<(), acp::Error> {
        let profile = self.models_manager.plan_mode_profile();
        if entering {
            self.apply_plan_model_scope_enter(&profile).await?;
            // Overlay delivery is independent of model resolution. A missing
            // credential, unavailable route, or absent sampling config must
            // not silently drop configured Plan Mode instructions/skills.
            if inject_overlay {
                self.queue_plan_profile_overlay().await;
            }
            return Ok(());
        }

        self.release_plan_model_scope(&profile).await
    }

    async fn apply_plan_model_scope_enter(
        &self,
        profile: &crate::agent::config::PlanModeProfileConfig,
    ) -> Result<(), acp::Error> {
        let Some(current) = self.current_plan_model_locator().await else {
            tracing::warn!(
                session_id = %self.session_info.id.0,
                "Plan model override skipped: session has no sampling config"
            );
            return Err(
                acp::Error::internal_error().data("session has no sampling config for Plan Mode")
            );
        };

        // Recover a crash between the write-ahead snapshot and the commit
        // snapshot. The live locator tells us which side of the switch became
        // durable. A third locator is a manual switch and always wins.
        // Bind the snapshot before `if let`: a mutex guard created directly in
        // the condition lives through the whole branch and would deadlock when
        // recovery commits or aborts the scope below.
        let pending_scope = { self.plan_mode.lock().pending_model_scope().cloned() };
        if let Some(pending) = pending_scope {
            // A pending scope is actionable only after the persistence actor
            // confirms that exact write-ahead snapshot is durable.
            self.persist_plan_mode_state_durable().await?;
            if current.same_selection(&pending.applied) {
                // The in-memory chat state is changed before CurrentModel's
                // disk ACK. A previous write failure can therefore leave the
                // live locator at `applied` while summary.json is still at
                // `base`. Re-issue the durable model write before committing
                // the scope; duplicate success is harmless.
                let Some((entry, sampling)) =
                    self.sampling_config_for_plan_locator(&pending.applied)
                else {
                    return Err(acp::Error::internal_error()
                        .data("pending Plan Mode model is no longer available"));
                };
                return self
                    .apply_prepared_plan_model(entry, sampling, &pending.applied)
                    .await;
            }
            if !current.same_selection(&pending.base) {
                self.plan_mode.lock().abort_prepared_model_scope();
                self.persist_plan_mode_state_durable().await?;
                tracing::info!(
                    session_id = %self.session_info.id.0,
                    model = %current.model,
                    "Abandoned pending Plan Mode model switch because the live model changed"
                );
                return Ok(());
            }

            if let Some((entry, sampling)) = self.sampling_config_for_plan_locator(&pending.applied)
            {
                self.apply_prepared_plan_model(entry, sampling, &pending.applied)
                    .await?;
                return Ok(());
            }

            // The exact pending target disappeared (for example a route's
            // credential changed while the process was down). Clear the stale
            // transaction and resolve the configured reference afresh below.
            self.plan_mode.lock().abort_prepared_model_scope();
            self.persist_plan_mode_state_durable().await?;
        }

        let committed_scope = { self.plan_mode.lock().model_scope().cloned() };
        if let Some(scope) = committed_scope {
            if current.same_selection(&scope.applied) {
                return Ok(());
            }
            if current.same_selection(&scope.base) {
                let Some((entry, sampling)) = self.sampling_config_for_plan_locator(&scope.applied)
                else {
                    return Err(acp::Error::internal_error()
                        .data("committed Plan Mode model is no longer available"));
                };
                let threshold = self
                    .models_manager
                    .auto_compact_threshold_for_model(&sampling.model);
                let model_id = self
                    .handle_set_session_model_durable(
                        sampling,
                        entry.info.use_concise,
                        false,
                        true,
                        threshold,
                    )
                    .await?;
                self.emit_plan_scoped_model_changed(model_id.0.as_ref())
                    .await;
                return Ok(());
            }

            // A third locator is an explicit/manual model choice. Preserve it
            // and release ownership rather than forcing the configured
            // planner over the user's selection during recovery.
            self.plan_mode.lock().finish_model_scope(&current, false);
            if let Err(error) = self.persist_plan_mode_state_durable().await {
                self.plan_mode
                    .lock()
                    .begin_model_scope(scope.base, scope.applied);
                return Err(error);
            }
            return Ok(());
        }
        let Some(model_ref) = profile.model.as_deref() else {
            return Ok(());
        };
        let Some((entry, sampling)) = self.models_manager.sampling_config_for_model_ref(model_ref)
        else {
            tracing::error!(
                session_id = %self.session_info.id.0,
                model = %model_ref,
                "Plan model override skipped: model/route unavailable or provider credential missing"
            );
            return Ok(());
        };
        let applied = crate::session::plan_mode::PlanModelLocator {
            route_ref: sampling.route_ref.clone(),
            model_ref: sampling.model_ref.clone(),
            model: sampling.model.clone(),
            base_url: sampling.base_url.clone(),
        };

        // Write-ahead ordering is intentional: PersistenceMsg is FIFO, so the
        // recoverable scope reaches plan_mode.json before CurrentModel can be
        // emitted by handle_set_session_model.
        if !self
            .plan_mode
            .lock()
            .prepare_model_scope(current, applied.clone())
        {
            return Ok(());
        }
        self.persist_plan_mode_state_durable().await?;
        self.apply_prepared_plan_model(entry, sampling, &applied)
            .await
    }

    async fn apply_prepared_plan_model(
        &self,
        entry: crate::agent::config::ModelEntry,
        sampling: xai_grok_sampler::SamplerConfig,
        applied: &crate::session::plan_mode::PlanModelLocator,
    ) -> Result<(), acp::Error> {
        let threshold = self
            .models_manager
            .auto_compact_threshold_for_model(&sampling.model);
        match self
            .handle_set_session_model_durable(
                sampling,
                entry.info.use_concise,
                false,
                true,
                threshold,
            )
            .await
        {
            Ok(model_id) => {
                self.plan_mode.lock().commit_prepared_model_scope();
                if let Err(error) = self.persist_plan_mode_state_durable().await {
                    self.plan_mode.lock().rollback_model_scope_commit();
                    return Err(error);
                }
                self.emit_plan_scoped_model_changed(model_id.0.as_ref())
                    .await;
                Ok(())
            }
            Err(error) => {
                tracing::error!(
                    session_id = %self.session_info.id.0,
                    model = %applied.model,
                    ?error,
                    "Plan model override persistence failed; retaining the write-ahead scope"
                );
                Err(error)
            }
        }
    }

    async fn release_plan_model_scope(
        &self,
        profile: &crate::agent::config::PlanModeProfileConfig,
    ) -> Result<(), acp::Error> {
        let Some(current) = self.current_plan_model_locator().await else {
            return Err(acp::Error::internal_error()
                .data("session has no sampling config for Plan Mode restore"));
        };

        let pending_scope = { self.plan_mode.lock().pending_model_scope().cloned() };
        if let Some(pending) = pending_scope {
            self.persist_plan_mode_state_durable().await?;
            if current.same_selection(&pending.applied) {
                self.plan_mode.lock().commit_prepared_model_scope();
            } else {
                // Still at the base means the apply never landed; any third
                // locator is a manual model switch. Neither case needs restore.
                self.plan_mode.lock().abort_prepared_model_scope();
                self.persist_plan_mode_state_durable().await?;
                return Ok(());
            }
            if let Err(error) = self.persist_plan_mode_state_durable().await {
                self.plan_mode.lock().rollback_model_scope_commit();
                return Err(error);
            }
        }

        let Some(scope) = self.plan_mode.lock().model_scope().cloned() else {
            return Ok(());
        };
        if !profile.restore_model || !current.same_selection(&scope.applied) {
            self.plan_mode
                .lock()
                .finish_model_scope(&current, profile.restore_model);
            if let Err(error) = self.persist_plan_mode_state_durable().await {
                self.plan_mode
                    .lock()
                    .begin_model_scope(scope.base, scope.applied);
                return Err(error);
            }
            return Ok(());
        }
        let Some((entry, sampling)) = self.sampling_config_for_plan_locator(&scope.base) else {
            tracing::error!(
                session_id = %self.session_info.id.0,
                model = %scope.base.model,
                "Plan model restore deferred: original model is no longer available"
            );
            return Err(acp::Error::internal_error()
                .data("original Plan Mode model is no longer available"));
        };
        let threshold = self
            .models_manager
            .auto_compact_threshold_for_model(&sampling.model);
        match self
            .handle_set_session_model_durable(
                sampling,
                entry.info.use_concise,
                false,
                true,
                threshold,
            )
            .await
        {
            Ok(model_id) => {
                self.plan_mode
                    .lock()
                    .finish_model_scope(&current, profile.restore_model);
                if let Err(error) = self.persist_plan_mode_state_durable().await {
                    self.plan_mode
                        .lock()
                        .begin_model_scope(scope.base, scope.applied);
                    return Err(error);
                }
                self.emit_plan_scoped_model_changed(model_id.0.as_ref())
                    .await;
                Ok(())
            }
            Err(error) => {
                tracing::error!(
                    session_id = %self.session_info.id.0,
                    model = %scope.base.model,
                    ?error,
                    "Plan model restore failed; retaining scope for a later retry"
                );
                Err(error)
            }
        }
    }

    async fn render_plan_profile_overlay(&self) -> Option<String> {
        let profile = self.models_manager.plan_mode_profile();
        let mut parts = Vec::new();
        if let Some(instructions) = profile
            .instructions
            .as_deref()
            .map(str::trim)
            .filter(|instructions| !instructions.is_empty())
        {
            parts.push(instructions.to_owned());
        }
        if !profile.skills.is_empty() {
            let bridge = self.agent.borrow().tool_bridge().clone();
            let discovered = bridge.slash_skills().await;
            for requested in &profile.skills {
                let Some(skill) = discovered.iter().find(|skill| {
                    skill.name.eq_ignore_ascii_case(requested)
                        || skill.dedup_key().eq_ignore_ascii_case(requested)
                }) else {
                    tracing::warn!(
                        session_id = %self.session_info.id.0,
                        skill = %requested,
                        "Configured Plan Mode skill was not found"
                    );
                    continue;
                };
                match xai_grok_tools::implementations::skills::skill::load_skill_with_body(skill)
                    .await
                {
                    Ok(loaded) => {
                        if let Some(body) = loaded.body.as_deref() {
                            parts.push(
                                xai_grok_tools::implementations::skills::skill::build_skill_message(
                                    &loaded, body,
                                ),
                            );
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            session_id = %self.session_info.id.0,
                            skill = %requested,
                            %error,
                            "Configured Plan Mode skill could not be loaded"
                        );
                    }
                }
            }
        }
        (!parts.is_empty()).then(|| {
            format!(
                "<plan-mode-profile>\n{}\n</plan-mode-profile>",
                parts.join("\n\n")
            )
        })
    }

    async fn queue_plan_profile_overlay(&self) {
        let Some(overlay) = self.render_plan_profile_overlay().await else {
            return;
        };
        let tag = self.reminder_wrapper_tag();
        self.pending_skill_reminders
            .lock()
            .push(ConversationItem::system_reminder(format!(
                "<{tag}>\n{overlay}\n</{tag}>"
            )));
    }

    /// Apply an agent-tool Plan Mode transition entirely on the session actor.
    ///
    /// Both the fire-and-forget notification bridge and the completed tool
    /// result call this path. The state-machine transition is the idempotency
    /// key: only the caller that changes state emits UI/persistence, applies or
    /// releases the model scope, and queues the profile overlay.
    pub(super) async fn apply_plan_tool_transition(
        &self,
        entering: bool,
    ) -> Result<bool, acp::Error> {
        use crate::session::plan_mode::PromptMode;
        use xai_grok_tools::types::SessionMode;

        if entering {
            let activated = self.plan_mode.lock().activate_from_tool();
            if !activated && !self.plan_mode.lock().is_active() {
                return Ok(false);
            }
            if activated {
                *self.current_prompt_mode.lock() = PromptMode::Plan;
                *self.turn_prompt_mode.lock() = PromptMode::Plan;
            }
            // The duplicate acknowledged command is also a durable retry
            // barrier: always re-assert Active before touching the model scope.
            self.persist_plan_mode_state_durable().await?;
            if activated {
                self.enqueue_current_mode_update(acp::SessionModeId::new(
                    SessionMode::Plan.as_id(),
                ));
                self.queue_plan_profile_overlay().await;
            }
            self.apply_plan_model_scope(true, false).await?;
            tracing::info!(
                session_id = %self.session_info.id.0,
                "Plan Mode tool entry committed before next sampling request"
            );
            return Ok(activated);
        }

        let deactivated = {
            let mut tracker = self.plan_mode.lock();
            let deactivated = tracker.deactivate_approved();
            if deactivated
                && self
                    .queue_exit_reminder_on_approved_exit
                    .load(std::sync::atomic::Ordering::Relaxed)
            {
                tracker.queue_exit_reminder();
            }
            deactivated
        };
        if deactivated {
            *self.current_prompt_mode.lock() = PromptMode::Agent;
            *self.turn_prompt_mode.lock() = PromptMode::Agent;
            if let Err(error) = self.persist_plan_mode_state_durable().await {
                self.plan_mode.lock().rollback_failed_approved_exit();
                *self.current_prompt_mode.lock() = PromptMode::Plan;
                *self.turn_prompt_mode.lock() = PromptMode::Plan;
                return Err(error);
            }
            self.enqueue_current_mode_update(acp::SessionModeId::new(SessionMode::Default.as_id()));
        } else if self.plan_mode.lock().is_active() {
            return Ok(false);
        } else {
            // Idempotent exit barrier after a partial restore/commit.
            self.persist_plan_mode_state_durable().await?;
        }
        self.apply_plan_model_scope(false, false).await?;
        tracing::info!(
            session_id = %self.session_info.id.0,
            "Plan Mode tool exit committed before next sampling request"
        );
        Ok(deactivated)
    }

    pub(super) fn apply_prompt_modes_to_snapshot(&self, snapshot: &mut TurnDeltaSnapshot) {
        snapshot.start_prompt_mode = Some(self.turn_start_prompt_mode.lock().to_string());
        snapshot.end_prompt_mode = Some(self.turn_prompt_mode.lock().to_string());
    }
    /// `false` twin: this template integration is not compiled into this
    /// build, so no session runs it. Keeps ungated call sites compiling in
    /// both configurations.
    pub(super) fn is_cursor_harness(&self) -> bool {
        false
    }
    pub(super) async fn handle_session_mode(
        &self,
        session_mode_id: acp::SessionModeId,
    ) -> Result<(), acp::Error> {
        use xai_grok_tools::types::SessionMode;
        let prompt_mode = prompt_mode_from_session_mode_id(&session_mode_id);
        let previous_prompt_mode = *self.current_prompt_mode.lock();
        let mode = SessionMode::from_id(session_mode_id.0.as_ref());
        if mode.is_plan() {
            let before = self.plan_mode.lock().clone();
            let entered = self.plan_mode.lock().enter_pending();
            if entered {
                *self.current_prompt_mode.lock() = prompt_mode;
            }
            if let Err(error) = self.persist_plan_mode_state_durable().await {
                if entered {
                    *self.plan_mode.lock() = before;
                    *self.current_prompt_mode.lock() = previous_prompt_mode;
                }
                tracing::error!(
                    session_id = %self.session_info.id.0,
                    ?error,
                    "Plan mode toggle rejected because its durable state write failed"
                );
                return Err(error);
            }
            *self.current_prompt_mode.lock() = prompt_mode;
            if entered {
                self.enqueue_current_mode_update(acp::SessionModeId::new(
                    SessionMode::Plan.as_id(),
                ));
            }
            tracing::info!(
                session_id = %self.session_info.id.0,
                entered,
                "Plan mode toggled ON (Pending)"
            );
            let turn_in_flight = self.state.lock().await.running_task.is_some();
            if entered
                && turn_in_flight
                && let Err(error) = self.activate_plan_mode_mid_turn().await
            {
                tracing::error!(
                    session_id = %self.session_info.id.0,
                    ?error,
                    "Mid-turn Plan mode activation was not durably persisted"
                );
                return Err(error);
            }
            if let Err(error) = self.apply_plan_model_scope(true, false).await {
                tracing::error!(
                    session_id = %self.session_info.id.0,
                    ?error,
                    "Plan mode model scope was not durably applied"
                );
                return Err(error);
            }
            xai_grok_telemetry::session_ctx::log_event(
                xai_grok_telemetry::events::PlanModeToggled {
                    enabled: true,
                    trigger: xai_grok_telemetry::events::PlanModeTrigger::User,
                    turn_in_flight,
                    was_previously_active: !entered,
                },
            );
            if entered {
                tracing::info_span!(
                    "session.permission_mode_changed",
                    from_mode =
                        super::telemetry::permission_mode_label(self.permissions.is_yolo_mode()),
                    to_mode = "plan",
                    trigger = "user",
                    enabled = true,
                )
                .in_scope(|| {});
            }
            return Ok(());
        }
        let was_plan = {
            let tracker = self.plan_mode.lock();
            tracker.state() != crate::session::plan_mode::PlanModeState::Inactive
        };
        let has_model_scope = self.plan_mode.lock().has_any_model_scope();
        if was_plan {
            let turn_in_flight = self.state.lock().await.running_task.is_some();
            let before = self.plan_mode.lock().clone();
            self.plan_mode.lock().user_exit(turn_in_flight);
            if let Err(error) = self.persist_plan_mode_state_durable().await {
                *self.plan_mode.lock() = before;
                *self.current_prompt_mode.lock() = previous_prompt_mode;
                tracing::error!(
                    session_id = %self.session_info.id.0,
                    ?error,
                    "Plan mode exit rejected because its durable state write failed"
                );
                return Err(error);
            }
            if !turn_in_flight && let Err(error) = self.apply_plan_model_scope(false, false).await {
                tracing::error!(
                    session_id = %self.session_info.id.0,
                    ?error,
                    "Plan mode model restore is incomplete; it will be retried"
                );
                return Err(error);
            }
            *self.current_prompt_mode.lock() = prompt_mode;
            self.enqueue_current_mode_update(session_mode_id.clone());
            tracing::info!(
                session_id = %self.session_info.id.0,
                new_mode = %session_mode_id.0,
                turn_in_flight,
                "Plan mode toggled OFF"
            );
            xai_grok_telemetry::session_ctx::log_event(
                xai_grok_telemetry::events::PlanModeToggled {
                    enabled: false,
                    trigger: xai_grok_telemetry::events::PlanModeTrigger::User,
                    turn_in_flight,
                    was_previously_active: true,
                },
            );
            tracing::info_span!(
                "session.permission_mode_changed",
                from_mode = "plan",
                to_mode = %session_mode_id.0,
                trigger = "user",
                enabled = false,
            )
            .in_scope(|| {});
        } else if has_model_scope {
            if let Err(error) = self.apply_plan_model_scope(false, false).await {
                tracing::error!(
                    session_id = %self.session_info.id.0,
                    ?error,
                    "Incomplete Plan mode model restore could not be retried"
                );
                return Err(error);
            }
            *self.current_prompt_mode.lock() = prompt_mode;
        } else {
            *self.current_prompt_mode.lock() = prompt_mode;
        }
        let agent_def = match session_mode_id.0.as_ref() {
            "browser_use" => Some(AgentDefinition::browser_use()),
            name => {
                let cwd = self.tool_context.cwd.as_path();
                xai_grok_agent::discovery::by_name_in_cwd(name, cwd)
            }
        };
        if let Some(ref def) = agent_def {
            tracing::info!(
                session_id = %self.session_info.id.0,
                agent_name = %def.name,
                agent_scope = %def.scope,
                prompt_mode = ?def.prompt_mode,
                has_completion_req = def.completion_requirement.is_some(),
                tool_configs = def.tool_config.tools.len(),
                "Resolved AgentDefinition for session mode"
            );
            self.agent
                .borrow()
                .update_policies_from_definition(def)
                .await;
            *self.active_agent_type.lock() = Some(def.name.clone());
        }
        if let Some(ref def) = agent_def {
            let new_prompt = self.agent.borrow().render_prompt_for_definition(def).await;
            let mut conversation = self.chat_state_handle.get_conversation().await;
            for item in conversation.iter_mut() {
                if let ConversationItem::System(sys) = item {
                    sys.content = std::sync::Arc::<str>::from(new_prompt);
                    break;
                }
            }
            self.chat_state_handle.replace_conversation(conversation);
        }
        Ok(())
    }
    /// Bring the plan-mode tracker into agreement with the prompt's mode.
    ///
    /// Mirrors `handle_session_mode` but driven from `_meta.mode` on the
    /// prompt — the only signal the client sends. Both transitions are
    /// idempotent, so `set_mode`-driven flows are unaffected.
    pub(super) async fn reconcile_plan_mode_with_prompt(
        &self,
        prompt_mode: PromptMode,
    ) -> Result<(), acp::Error> {
        use crate::session::plan_mode::PlanModeState;
        let previous_prompt_mode = *self.current_prompt_mode.lock();
        *self.current_prompt_mode.lock() = prompt_mode;
        match prompt_mode {
            PromptMode::Plan => {
                let before = self.plan_mode.lock().clone();
                let entered = self.plan_mode.lock().enter_pending();
                if let Err(error) = self.persist_plan_mode_state_durable().await {
                    if entered {
                        *self.plan_mode.lock() = before;
                    }
                    *self.current_prompt_mode.lock() = previous_prompt_mode;
                    return Err(error);
                }
                self.apply_plan_model_scope(true, false).await?;
            }
            PromptMode::Agent | PromptMode::Ask => {
                let was_plan = {
                    let tracker = self.plan_mode.lock();
                    tracker.state() != PlanModeState::Inactive
                };
                let has_model_scope = self.plan_mode.lock().has_any_model_scope();
                if was_plan {
                    let before = self.plan_mode.lock().clone();
                    if before.state() == crate::session::plan_mode::PlanModeState::ExitPending {
                        self.plan_mode.lock().complete_deferred_exit();
                    } else {
                        self.plan_mode.lock().user_exit(false);
                    }
                    if let Err(error) = self.persist_plan_mode_state_durable().await {
                        *self.plan_mode.lock() = before;
                        *self.current_prompt_mode.lock() = previous_prompt_mode;
                        return Err(error);
                    }
                }
                if was_plan || has_model_scope {
                    self.apply_plan_model_scope(false, false).await?;
                }
            }
        }
        Ok(())
    }
    /// Inject plan mode system-reminders into the conversation.
    ///
    /// Called once per turn from `handle_prompt()`, before the user's actual
    /// message is pushed. Handles three mutually-ordered cases:
    ///
    /// 1. **Pending → Active**: First prompt after user toggled plan mode on.
    ///    Injects the full (or reentry) reminder and transitions to Active.
    /// 2. **Already Active**: Subsequent prompts while plan mode is on.
    ///    Injects an alternating full/sparse per-turn reminder.
    /// 3. **Exit reminder**: One-shot reminder after plan mode was exited.
    ///    Injected once, then the flag is cleared.
    ///
    /// All reminders are pushed as `<system-reminder>`-wrapped user messages
    /// so the model sees them in the same turn as the user's prompt.
    /// Tool names are resolved at render time via `TemplateRenderer`.
    pub(super) async fn inject_plan_mode_reminders(&self) -> Result<(), acp::Error> {
        use crate::session::plan_mode::{
            PlanModeState, plan_mode_exit_reminder_template, plan_mode_reminder_full_template,
            plan_mode_reminder_sparse_template,
        };
        let use_cursor_reminders = self.is_cursor_harness();
        let push_reminder = |this: &Self, content: &str| {
            this.push_system_reminder_with_tag(content, this.reminder_wrapper_tag());
        };
        let mut injected_this_turn = false;
        let activation = {
            let tracker = self.plan_mode.lock();
            (tracker.state() == PlanModeState::Pending)
                .then(|| (tracker.is_reentry(), tracker.plan_file_path().to_path_buf()))
        };
        if let Some((is_reentry, plan_path)) = activation {
            self.plan_mode.lock().activate();
            self.persist_plan_mode_state_durable().await?;
            let plan_has_content =
                crate::session::plan_mode::plan_file_has_content(&plan_path).await;
            let template = self.plan_activation_template(is_reentry);
            if let Some(rendered) = self
                .render_plan_template(template, &plan_path, plan_has_content)
                .await
            {
                push_reminder(self, &rendered);
                injected_this_turn = true;
                self.plan_mode.lock().record_reminder_injected();
                self.persist_plan_mode_state();
                tracing::info!(
                    session_id = %self.session_info.id.0,
                    is_reentry,
                    uses_template_reminders = use_cursor_reminders,
                    "Plan mode activated: injected system-reminder"
                );
            }
        }
        if !injected_this_turn {
            let per_turn = {
                let tracker = self.plan_mode.lock();
                tracker.is_active().then(|| {
                    (
                        tracker.should_use_full_reminder(),
                        tracker.plan_file_path().to_path_buf(),
                    )
                })
            };
            if let Some((use_full, plan_path)) = per_turn {
                let plan_has_content =
                    crate::session::plan_mode::plan_file_has_content(&plan_path).await;
                let template = if use_full {
                    plan_mode_reminder_full_template()
                } else {
                    plan_mode_reminder_sparse_template()
                };
                if let Some(rendered) = self
                    .render_plan_template(template, &plan_path, plan_has_content)
                    .await
                {
                    push_reminder(self, &rendered);
                    self.plan_mode.lock().record_reminder_injected();
                    self.persist_plan_mode_state();
                }
            }
        }
        if self.plan_mode.lock().is_active()
            && let Some(overlay) = self.render_plan_profile_overlay().await
        {
            push_reminder(self, &overlay);
        }
        if self.plan_mode.lock().has_pending_exit_reminder() {
            let plan_path = self.plan_mode.lock().plan_file_path().to_path_buf();
            let template = plan_mode_exit_reminder_template();
            if let Some(rendered) = self.render_plan_template(template, &plan_path, false).await {
                push_reminder(self, &rendered);
            }
            self.plan_mode.lock().clear_pending_exit_reminder();
            self.persist_plan_mode_state();
        }
        Ok(())
    }
    /// Activate plan mode for a turn that is already running.
    ///
    /// Mid-turn counterpart of `inject_plan_mode_reminders` case 1: the user
    /// toggled plan mode ON (Shift+Tab) while the model was thinking, so the
    /// tracker sits in `Pending` and the running turn would otherwise proceed
    /// without any plan-mode instruction. Activate immediately (so
    /// `is_active()` tool gating applies to subsequent calls) and buffer the
    /// activation reminder on the tracker; `flush_pending_skill_reminders`
    /// delivers it at the running turn's next safe point (loop top / after
    /// each tool batch) — or, if the turn ends first, the cancel/idle flush
    /// lands it for the next turn. Buffering (vs a direct conversation push)
    /// keeps the in-flight batch's tool_result blocks adjacent, and lets a
    /// toggle-off withdraw an undelivered reminder (`user_exit`).
    ///
    /// No-op unless the tracker is `Pending`: `enter_pending`'s
    /// `ExitPending → Active` re-entry needs no reminder (the model already
    /// has plan-mode context and no exit reminder was injected yet).
    ///
    /// A failed template render still activates (without a buffer), keeping
    /// gating in lockstep with the turn-start path.
    pub(super) async fn activate_plan_mode_mid_turn(&self) -> Result<(), acp::Error> {
        use crate::session::plan_mode::PlanModeState;
        let activation = {
            let tracker = self.plan_mode.lock();
            (tracker.state() == PlanModeState::Pending)
                .then(|| (tracker.is_reentry(), tracker.plan_file_path().to_path_buf()))
        };
        let Some((is_reentry, plan_path)) = activation else {
            return Ok(());
        };
        let plan_has_content = crate::session::plan_mode::plan_file_has_content(&plan_path).await;
        let template = self.plan_activation_template(is_reentry);
        let rendered = self
            .render_plan_template(template, &plan_path, plan_has_content)
            .await;
        let overlay = self.render_plan_profile_overlay().await;
        let rendered = match (rendered, overlay) {
            (Some(reminder), Some(overlay)) => Some(format!("{reminder}\n\n{overlay}")),
            (Some(reminder), None) => Some(reminder),
            (None, Some(overlay)) => Some(overlay),
            (None, None) => None,
        };
        let tag = self.reminder_wrapper_tag();
        let buffered = rendered.is_some();
        let activated = match rendered {
            Some(rendered) => self
                .plan_mode
                .lock()
                .activate_mid_turn(format!("<{tag}>\n{rendered}\n</{tag}>")),
            None => {
                tracing::warn!(
                    session_id = % self.session_info.id.0,
                    "Mid-turn plan activation: reminder render failed; \
                     activating without a buffered reminder"
                );
                self.plan_mode.lock().activate()
            }
        };
        if !activated {
            return Ok(());
        }
        self.persist_plan_mode_state_durable().await?;
        tracing::info!(
            session_id = %self.session_info.id.0,
            is_reentry,
            buffered,
            "Plan mode activated mid-turn"
        );
        Ok(())
    }
    /// The activation reminder template for the active template (no
    /// first-entry/reentry distinction), or grok's reentry/full variant.
    /// Shared by turn-start injection (`inject_plan_mode_reminders` case 1)
    /// and the mid-turn toggle (`activate_plan_mode_mid_turn`).
    fn plan_activation_template(&self, is_reentry: bool) -> &'static str {
        use crate::session::plan_mode::{
            plan_mode_reentry_reminder_template, plan_mode_reminder_full_template,
        };
        if is_reentry {
            plan_mode_reentry_reminder_template()
        } else {
            plan_mode_reminder_full_template()
        }
    }
    /// Render a plan mode template via the tool bridge's `TemplateRenderer`.
    ///
    /// Passes `plan_path` and `plan_has_content` as extra context alongside the
    /// registry's `tools.by_kind.*` mappings.
    pub(super) async fn render_plan_template(
        &self,
        template: &str,
        plan_path: &std::path::Path,
        plan_has_content: bool,
    ) -> Option<String> {
        let extra = serde_json::json!({
            "plan_path": plan_path.display().to_string(),
            "plan_has_content": plan_has_content,
        });
        self.agent
            .borrow()
            .tool_bridge()
            .render_prompt(template, &extra)
            .await
    }
    /// Persist the current plan mode state to disk.
    ///
    /// Called after every state transition so plan mode survives
    /// session reload/resume/reconnect.
    pub(super) fn persist_plan_mode_state(&self) {
        let snapshot = self.plan_mode.lock().snapshot();
        let _ = self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::PlanModeState(snapshot));
    }
}
