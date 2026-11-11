//! Fork: Team Leader mode hooks for `SessionActor` (turn-start reminders, the delegation gate, `/team`).
//! The rules and texts live in the pure [`crate::session::team_mode`] module; this file only wires them to the actor.
use super::*;
use crate::session::team_mode::{
    TeamGate, TeamReminder, team_rejection_text, team_reminder_text, team_status_text,
    team_tool_gate,
};
impl SessionActor {
    /// Model-facing name of the subagent tool, or `None` when this session cannot delegate.
    /// Subagent sessions never lead: they are the workers, even when forked from a leader's conversation.
    async fn team_task_tool(&self) -> Option<String> {
        if self.tool_context.subagent_depth != 0 {
            return None;
        }
        self.tool_bridge_handle()
            .tool_for_kind(xai_grok_tools::types::tool::ToolKind::Task)
            .await
    }
    /// Called once per turn from `handle_prompt()`, right after the plan mode reminders.
    /// Full instructions on the first turn (and after compaction), a one-line reminder after that, one exit note when turned off.
    pub(super) async fn inject_team_mode_reminders(&self) {
        let Some(reminder) = self.plan_mode.lock().team.pending_reminder() else {
            return;
        };
        let Some(task_tool) = self.team_task_tool().await else {
            return;
        };
        let text = team_reminder_text(reminder, &task_tool);
        self.push_system_reminder_with_tag(&text, self.reminder_wrapper_tag());
        self.plan_mode
            .lock()
            .team
            .record_reminder_injected(reminder);
        if reminder == TeamReminder::Sparse {
            return;
        }
        self.persist_plan_mode_state();
        tracing::info!(session_id = %self.session_info.id.0, ?reminder, "Team mode: injected system-reminder");
        if reminder == TeamReminder::Activation {
            // Covers sessions that start in team mode from config, which never pass through `/team`.
            self.send_xai_notification(XaiSessionUpdate::TeamModeChanged { enabled: true })
                .await;
        }
    }
    /// Delegation gate, run right after the plan mode gate. Returns `Ok(true)` when the call was rejected
    /// and already reported to the model as not executed.
    pub(super) async fn team_mode_reject_tool_call(
        &self,
        call: &crate::sampling::types::ToolCallResponse,
        tool_call_id: &acp::ToolCallId,
        model_id_str: &str,
        tool_input: &ToolInput,
        access_kind: &AccessKind,
    ) -> Result<bool, acp::Error> {
        let gate = team_tool_gate(&self.plan_mode.lock(), tool_input, access_kind);
        if gate == TeamGate::Allow {
            return Ok(false);
        }
        // Fail open: a lead with nobody to delegate to must still be able to work.
        let Some(task_tool) = self.team_task_tool().await else {
            return Ok(false);
        };
        let Some(msg) = team_rejection_text(gate, &task_tool) else {
            return Ok(false);
        };
        xai_grok_telemetry::event_span!(
            "tool.decision",
            tool_name = %call.function.name,
            tool_use_id = %call.id,
            model_id = %model_id_str,
            decision = "deny",
            source = "team_mode",
            wait_ms = 0_i64,
        );
        self.handle_tool_not_executed(&call.id, tool_call_id, msg)
            .await?;
        Ok(true)
    }
    /// `/team on|off|status`.
    pub(super) async fn handle_team_command(&self, enabled: Option<bool>) {
        let can_delegate = self.team_task_tool().await.is_some();
        let (now, changed) = {
            let mut tracker = self.plan_mode.lock();
            let changed = enabled.is_some_and(|enabled| tracker.team.set_enabled(enabled));
            (tracker.team.is_enabled(), changed)
        };
        if changed {
            self.persist_plan_mode_state();
            self.send_xai_notification(XaiSessionUpdate::TeamModeChanged { enabled: now })
                .await;
            tracing::info!(session_id = %self.session_info.id.0, enabled = now, "Team mode toggled");
        }
        self.send_host_turn_slash_command_output(&team_status_text(now, can_delegate))
            .await;
    }
}
