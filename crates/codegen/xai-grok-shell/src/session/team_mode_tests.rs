use super::*;
use std::path::PathBuf;

use xai_grok_tools::implementations::BashToolInput;
use xai_grok_tools::implementations::codex::apply_patch::ApplyPatchInput;
use xai_grok_tools::implementations::opencode::write::WriteInput;

fn tracker() -> PlanModeTracker {
    PlanModeTracker::new(PathBuf::from("/tmp/team-session"))
}

fn team_tracker() -> PlanModeTracker {
    let mut t = tracker();
    assert!(t.team.set_enabled(true));
    t
}

fn write(path: &str) -> ToolInput {
    ToolInput::Write(WriteInput {
        file_path: path.into(),
        content: "x".into(),
    })
}

fn bash(command: &str) -> ToolInput {
    let input: BashToolInput =
        serde_json::from_value(serde_json::json!({ "command": command, "description": "d" }))
            .unwrap();
    ToolInput::Bash(input)
}

fn gate(tracker: &PlanModeTracker, input: &ToolInput) -> TeamGate {
    team_tool_gate(tracker, input, &AccessKind::from(input))
}

#[test]
fn reminder_lifecycle() {
    let mut state = TeamModeState::default();
    assert_eq!(state.pending_reminder(), None);

    assert!(state.set_enabled(true));
    assert!(!state.set_enabled(true), "re-enabling is a no-op");
    assert_eq!(state.pending_reminder(), Some(TeamReminder::Activation));
    state.record_reminder_injected(TeamReminder::Activation);
    assert_eq!(state.pending_reminder(), Some(TeamReminder::Sparse));
    state.record_reminder_injected(TeamReminder::Sparse);
    assert_eq!(state.pending_reminder(), Some(TeamReminder::Sparse));

    assert!(state.set_enabled(false));
    assert_eq!(state.pending_reminder(), Some(TeamReminder::Exit));
    state.record_reminder_injected(TeamReminder::Exit);
    assert_eq!(state.pending_reminder(), None);
}

#[test]
fn toggling_off_before_announcement_owes_nothing() {
    let mut state = TeamModeState::default();
    state.set_enabled(true);
    state.set_enabled(false);
    assert_eq!(state.pending_reminder(), None);
}

#[test]
fn compaction_repeats_full_instructions_only_while_enabled() {
    let mut state = TeamModeState::enabled_from_start();
    state.record_reminder_injected(TeamReminder::Activation);
    state.reset_after_compaction();
    assert_eq!(state.pending_reminder(), Some(TeamReminder::Activation));

    state.record_reminder_injected(TeamReminder::Activation);
    state.set_enabled(false);
    state.reset_after_compaction();
    assert_eq!(
        state.pending_reminder(),
        Some(TeamReminder::Exit),
        "a pending exit note survives compaction"
    );
}

#[test]
fn tracker_compaction_resets_team_state_without_plan_mode() {
    let mut t = team_tracker();
    t.team.record_reminder_injected(TeamReminder::Activation);
    t.reset_after_compaction();
    assert_eq!(t.team.pending_reminder(), Some(TeamReminder::Activation));
}

#[test]
fn snapshot_round_trip_and_old_files() {
    let mut t = team_tracker();
    t.team.record_reminder_injected(TeamReminder::Activation);
    let json = serde_json::to_value(t.snapshot()).unwrap();
    let restored = PlanModeTracker::from_snapshot(
        PathBuf::from("/tmp/team-session"),
        serde_json::from_value(json.clone()).unwrap(),
    );
    assert_eq!(restored.team, t.team);

    // `plan_mode.json` written before team mode existed has no `team` key.
    let mut old = json;
    old.as_object_mut().unwrap().remove("team");
    let snapshot: PlanModeSnapshot = serde_json::from_value(old).unwrap();
    assert_eq!(snapshot.team, TeamModeState::default());
}

#[test]
fn gate_is_open_when_team_mode_is_off() {
    let t = tracker();
    assert_eq!(gate(&t, &write("/tmp/src/main.rs")), TeamGate::Allow);
    assert_eq!(gate(&t, &bash("cargo test")), TeamGate::Allow);
}

#[test]
fn gate_rejects_edits_and_patches() {
    let t = team_tracker();
    assert_eq!(gate(&t, &write("/tmp/src/main.rs")), TeamGate::RejectEdit);
    assert_eq!(
        gate(
            &t,
            &ToolInput::ApplyPatch(ApplyPatchInput {
                patch: String::new()
            })
        ),
        TeamGate::RejectEdit
    );
}

#[test]
fn gate_keeps_plan_file_writable_in_plan_mode() {
    let mut t = team_tracker();
    assert_eq!(
        gate(&t, &write("/tmp/team-session/plan.md")),
        TeamGate::RejectEdit,
        "outside plan mode the plan file is just a file"
    );
    assert!(t.enter_pending());
    assert!(t.activate());
    assert_eq!(
        gate(&t, &write("/tmp/team-session/plan.md")),
        TeamGate::Allow
    );
    assert_eq!(gate(&t, &write("/tmp/src/main.rs")), TeamGate::RejectEdit);
}

#[test]
fn gate_allows_read_only_commands_only() {
    let t = team_tracker();
    for command in ["ls -la", "git status", "rg -n TODO src"] {
        assert_eq!(gate(&t, &bash(command)), TeamGate::Allow, "{command}");
    }
    for command in ["cargo test", "echo hi > out.txt", "rm -rf target"] {
        assert_eq!(
            gate(&t, &bash(command)),
            TeamGate::RejectCommand,
            "{command}"
        );
    }
}

#[test]
fn gate_leaves_delegation_and_reading_to_the_lead() {
    let t = team_tracker();
    for access in [
        AccessKind::Read(Some("/tmp/src/main.rs".into())),
        AccessKind::Grep {
            path: None,
            glob: None,
        },
        AccessKind::Tool("task".into()),
        AccessKind::AgentMessage {
            subagent_id: "a1".into(),
        },
        AccessKind::WebSearch("rust".into()),
    ] {
        assert_eq!(
            team_tool_gate(&t, &write("/unused"), &access),
            TeamGate::Allow,
            "{access:?}"
        );
    }
}

#[test]
fn texts_name_the_task_tool() {
    for reminder in [
        TeamReminder::Activation,
        TeamReminder::Sparse,
        TeamReminder::Exit,
    ] {
        let text = team_reminder_text(reminder, "spawn_subagent");
        assert!(!text.contains(TASK_TOOL_PLACEHOLDER), "{reminder:?}");
    }
    assert!(
        team_reminder_text(TeamReminder::Activation, "spawn_subagent").contains("spawn_subagent")
    );
    assert_eq!(team_rejection_text(TeamGate::Allow, "task"), None);
    for gate in [TeamGate::RejectEdit, TeamGate::RejectCommand] {
        let text = team_rejection_text(gate, "spawn_subagent").unwrap();
        assert!(text.contains("spawn_subagent"), "{gate:?}");
    }
}

#[test]
fn parse_team_arg_values() {
    assert_eq!(parse_team_arg("on"), Some(true));
    assert_eq!(parse_team_arg(" Off "), Some(false));
    assert_eq!(parse_team_arg(""), None);
    assert_eq!(parse_team_arg("status"), None);
}

#[test]
fn new_session_snapshot_follows_config() {
    let off = TeamConfig::default();
    let on = TeamConfig {
        enabled: Some(true),
        worker_model: None,
    };
    // Env overrides config; only assert when the test process does not set it.
    if std::env::var_os("GROK_TEAM_MODE").is_none() {
        assert!(new_session_plan_snapshot(&off).is_none());
        let snapshot = new_session_plan_snapshot(&on).unwrap();
        assert_eq!(snapshot.team, TeamModeState::enabled_from_start());
        let restored = PlanModeTracker::from_snapshot(PathBuf::from("/tmp/s"), snapshot);
        assert!(!restored.is_active(), "plan mode stays off");
        assert_eq!(
            restored.team.pending_reminder(),
            Some(TeamReminder::Activation)
        );
    }
}

#[test]
fn worker_model_needs_a_leading_parent() {
    if std::env::var_os("GROK_TEAM_WORKER_MODEL").is_some() {
        return;
    }
    let config = TeamConfig {
        enabled: None,
        worker_model: Some(" cheap-model ".into()),
    };
    let leader = parking_lot::Mutex::new(team_tracker());
    let plain = parking_lot::Mutex::new(tracker());
    assert_eq!(
        spawn_worker_model(Some(&leader), &config).as_deref(),
        Some("cheap-model")
    );
    assert_eq!(spawn_worker_model(Some(&plain), &config), None);
    assert_eq!(spawn_worker_model(None, &config), None);
    assert_eq!(
        spawn_worker_model(Some(&leader), &TeamConfig::default()),
        None
    );
}
