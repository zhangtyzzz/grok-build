//! Fork: Team Leader mode state, delegation gate, and prompt text.
//!
//! In team mode the top-level session only plans, delegates, and reviews; subagents do the hands-on work.
//! That lets a strong (expensive) model manage while a cheaper `[team] worker_model` executes.
//! The mode is an orthogonal session flag, not a `SessionMode`: it composes with plan mode and every permission mode.
//!
//! This module is pure (no `SessionActor`, no I/O). The state rides inside [`PlanModeTracker`] / `plan_mode.json`,
//! so persistence, resume, and session fork need no plumbing of their own.
//! The system prompt is never touched: the model learns about the mode from `<system-reminder>` messages, which keeps the prompt cache intact.
//!
//! The gate is a delegation rule, not a security boundary: a call it allows still goes through the normal permission flow.
use std::path::Path;

use serde::{Deserialize, Serialize};
use xai_grok_tools::types::ToolInput;
use xai_grok_workspace::permission::{AccessKind, bash_is_read_only_query};

use xai_grok_config_types::BoolFlag;

use crate::session::plan_mode::{PlanModeSnapshot, PlanModeTracker};
/// `[team]` config section.
/// Precedence per key: env (`GROK_TEAM_MODE`, `GROK_TEAM_WORKER_MODEL`) > this config > default.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TeamConfig {
    /// Start every new top-level session in Team Leader mode. `/team on|off` still toggles per session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Model for subagents spawned while team mode is on, used when nothing more specific picks one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_model: Option<String>,
}
impl TeamConfig {
    /// Whether new top-level sessions start in team mode.
    pub(crate) fn resolve_enabled(&self) -> bool {
        BoolFlag::env("GROK_TEAM_MODE")
            .config(self.enabled)
            .default(false)
            .resolve()
            .value
    }
    /// Fallback model for subagents spawned by a session in team mode.
    pub(crate) fn resolve_worker_model(&self) -> Option<String> {
        xai_grok_env::env_string("GROK_TEAM_WORKER_MODEL")
            .or_else(|| self.worker_model.clone())
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty())
    }
}
/// `[team] worker_model` for a subagent being spawned, when its parent session is in team mode.
/// It ranks below `[subagents.models]` and an agent definition's own model, and above inheriting the parent's model.
pub(crate) fn spawn_worker_model(
    parent_tracker: Option<&parking_lot::Mutex<PlanModeTracker>>,
    config: &TeamConfig,
) -> Option<String> {
    parent_tracker.filter(|tracker| tracker.lock().team.is_enabled())?;
    config.resolve_worker_model()
}
/// Keeps the worker model only for spawns the lead's model issued with its task tool.
/// Harness roles (the `/goal` planner, verifier, strategist, summarizer), workflow agents, and scheduled spawns
/// have no `tool_call_id` or a workflow owner: they keep their own model choice, so `/goal` verification stays on the strong model.
pub(crate) fn worker_model_for_spawn(
    worker_model: Option<String>,
    issued_by_model_tool_call: bool,
    workflow_owned: bool,
) -> Option<String> {
    worker_model.filter(|_| issued_by_model_tool_call && !workflow_owned)
}
/// Initial `plan_mode.json` state for a brand-new session: `None` (nothing to restore) unless `[team] enabled`.
pub(crate) fn new_session_plan_snapshot(config: &TeamConfig) -> Option<PlanModeSnapshot> {
    if !config.resolve_enabled() {
        return None;
    }
    let mut snapshot = PlanModeTracker::new(std::path::PathBuf::new()).snapshot();
    snapshot.team = TeamModeState::enabled_from_start();
    Some(snapshot)
}
/// Session-scoped team mode state, persisted as the `team` field of `plan_mode.json`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TeamModeState {
    /// The delegation gate is live.
    enabled: bool,
    /// The model's context holds the leader instructions and it has not been told the mode ended.
    /// `enabled && !announced` owes the full reminder; `!enabled && announced` owes the exit reminder.
    announced: bool,
}
/// Which reminder a turn start owes the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TeamReminder {
    /// First turn after the mode was turned on (or after compaction dropped the instructions).
    Activation,
    /// Every later turn while the mode stays on.
    Sparse,
    /// First turn after the mode was turned off.
    Exit,
}
impl TeamModeState {
    pub(crate) fn enabled_from_start() -> Self {
        Self {
            enabled: true,
            announced: false,
        }
    }
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
    /// Returns whether the flag changed.
    pub(crate) fn set_enabled(&mut self, enabled: bool) -> bool {
        let changed = self.enabled != enabled;
        self.enabled = enabled;
        changed
    }
    /// The reminder the next turn should carry, without recording it as delivered.
    pub(crate) fn pending_reminder(&self) -> Option<TeamReminder> {
        match (self.enabled, self.announced) {
            (true, false) => Some(TeamReminder::Activation),
            (true, true) => Some(TeamReminder::Sparse),
            (false, true) => Some(TeamReminder::Exit),
            (false, false) => None,
        }
    }
    /// Called after the reminder from [`Self::pending_reminder`] was pushed into the conversation.
    pub(crate) fn record_reminder_injected(&mut self, reminder: TeamReminder) {
        self.announced = !matches!(reminder, TeamReminder::Exit);
    }
    /// Compaction may have summarized the leader instructions away, so the next turn repeats the full reminder.
    /// A pending exit reminder is kept: the summary can still describe the session as a team leader.
    pub(crate) fn reset_after_compaction(&mut self) {
        if self.enabled {
            self.announced = false;
        }
    }
}
/// Outcome of the team mode delegation gate for one tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TeamGate {
    Allow,
    /// A file edit the leader must hand to a subagent.
    RejectEdit,
    /// A shell command that is not a plain read-only inspection.
    RejectCommand,
}
/// Decide whether the leader may run this call itself. The caller has already checked that the session is a
/// top-level one and that a subagent tool exists (a leader with nobody to delegate to must not be bricked).
///
/// Only edits and shell commands are gated. Everything else stays with the leader: spawning and steering
/// subagents, reading and searching, the todo list, questions to the user, plan tools, web, MCP, skills.
/// `apply_patch` is `AccessKind::Tool` (its files are named inside the patch text) and is always an edit.
/// The plan file stays writable while plan mode is active, so plan mode and team mode compose.
pub(crate) fn team_tool_gate(
    tracker: &PlanModeTracker,
    tool_input: &ToolInput,
    access_kind: &AccessKind,
) -> TeamGate {
    if !tracker.team.is_enabled() {
        return TeamGate::Allow;
    }
    if matches!(tool_input, ToolInput::ApplyPatch(_)) {
        return TeamGate::RejectEdit;
    }
    match access_kind {
        AccessKind::Edit(path) if !tracker.should_auto_approve_edit(Path::new(path)) => {
            TeamGate::RejectEdit
        }
        AccessKind::Bash(command) if !bash_is_read_only_query(command) => TeamGate::RejectCommand,
        _ => TeamGate::Allow,
    }
}
const TASK_TOOL_PLACEHOLDER: &str = "{task_tool}";
const ACTIVATION_REMINDER: &str = "\
Team Leader mode is active. You are the team lead for this session: you plan, delegate, and review. \
Your subagents do the hands-on work.

## Your role
- Understand the request, decide the approach, split the work into well-scoped tasks, and hand each task to a subagent with the {task_tool} tool.
- Keep your own context for planning and decisions. Do not edit files, and do not run builds, tests, installs, or any other state-changing command yourself.
- You may still look at things directly when it helps you decide: read files, search, list directories, and run read-only shell commands (for example `ls`, `cat`, `rg`, `git status`, `git diff`, `git log`).
- This is enforced. File edits and shell commands that are not plain read-only inspection are rejected before they run. When that happens, delegate the work; do not look for another way to do it yourself.
- In plan mode you still write the plan file yourself.

## Delegating
- A subagent starts with none of your context. Brief it the way you would brief a senior engineer joining the project: what you need and why, what you already know (paths, symbols, decisions already made), what the end state looks like, and how you will judge that it is done.
- Describe outcomes, not keystrokes. Share your opinion on the approach as guidance and trust the subagent with the details.
- Run independent tasks in parallel instead of one after another. Give each task a clear boundary (the files or modules it owns) so parallel workers do not collide.
- Use an exploration subagent for wide codebase investigation and an implementation subagent for changes, builds, and tests.

## Reviewing
- A subagent's report is a claim, not evidence. Before you tell the user that something works, check it: read the diff yourself, or send a separate reviewer subagent that did not write the change to run the tests and try to break it.
- When a result is wrong or incomplete, send it back with specific feedback or start a new subagent with a corrected brief. Do not finish the work yourself.
- Report to the user in your own words: what was done, what was verified and how, and what is still open.

These rules bind the lead of this session only. A subagent is a worker even when it was started with a copy of this conversation: it edits files and runs commands itself.";
const SPARSE_REMINDER: &str = "\
Team Leader mode is still active: plan, delegate with the {task_tool} tool, and review. \
File edits and shell commands other than read-only inspection are rejected for you; delegate them.";
const EXIT_REMINDER: &str = "\
Team Leader mode has been turned off. You may edit files and run commands yourself again; \
delegating to subagents is now optional.";
const EDIT_REJECTED: &str = "\
Team Leader mode is active: the team lead does not edit files. \
Delegate this change to a subagent with the {task_tool} tool. \
Tell it which files to change, what the change should achieve, and how to verify it. \
Do not retry the edit yourself.";
const COMMAND_REJECTED: &str = "\
Team Leader mode is active: the team lead only runs read-only inspection commands \
(for example `ls`, `cat`, `head`, `wc`, `rg`, `git status`, `git diff`, `git log`), \
with no output redirection to files, command substitution, or background jobs. \
Delegate builds, tests, installs, and anything that changes state to a subagent with the {task_tool} tool. \
For plain inspection, use your file read, search, and list tools.";
fn with_task_tool(text: &str, task_tool: &str) -> String {
    text.replace(TASK_TOOL_PLACEHOLDER, task_tool)
}
/// Reminder body (unwrapped) for a turn start. `task_tool` is the model-facing name of the subagent tool.
pub(crate) fn team_reminder_text(reminder: TeamReminder, task_tool: &str) -> String {
    let template = match reminder {
        TeamReminder::Activation => ACTIVATION_REMINDER,
        TeamReminder::Sparse => SPARSE_REMINDER,
        TeamReminder::Exit => EXIT_REMINDER,
    };
    with_task_tool(template, task_tool)
}
/// Tool result text for a rejected call. `None` for [`TeamGate::Allow`].
pub(crate) fn team_rejection_text(gate: TeamGate, task_tool: &str) -> Option<String> {
    let template = match gate {
        TeamGate::Allow => return None,
        TeamGate::RejectEdit => EDIT_REJECTED,
        TeamGate::RejectCommand => COMMAND_REJECTED,
    };
    Some(with_task_tool(template, task_tool))
}
/// `/team` argument: `Some(true|false)` turns the mode on or off, `None` (empty, `status`, anything else) reports it.
pub(crate) fn parse_team_arg(args: &str) -> Option<bool> {
    match args.trim().to_lowercase().as_str() {
        "on" | "true" | "1" | "yes" | "enable" => Some(true),
        "off" | "false" | "0" | "no" | "disable" => Some(false),
        _ => None,
    }
}
/// `/team` output after a toggle or a status query.
pub(crate) fn team_status_text(enabled: bool, can_delegate: bool) -> String {
    match (enabled, can_delegate) {
        (true, true) => "Team Leader mode is on. I plan, delegate to subagents, and review; \
             file edits and non-read-only commands are handed to subagents. Use /team off to turn it off."
            .to_owned(),
        (true, false) => "Team Leader mode is on, but this session has no subagent tool, \
             so it has no effect until subagents are available. Use /team off to turn it off."
            .to_owned(),
        (false, _) => "Team Leader mode is off. Use /team on to turn it on.".to_owned(),
    }
}
#[cfg(test)]
#[path = "team_mode_tests.rs"]
mod tests;
