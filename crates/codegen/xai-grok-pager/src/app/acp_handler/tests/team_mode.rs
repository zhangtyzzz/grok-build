//! Fork: Team Leader mode indicator (`TeamModeChanged`).
use super::*;

fn team_changed(session: &str, enabled: bool) -> AcpClientMessage {
    make_ext_session_notification(session, XaiSessionUpdate::TeamModeChanged { enabled })
}

#[test]
fn team_mode_changed_toggles_the_indicator() {
    let mut app = make_app_with_agent("sess-team");
    assert!(!test_agent(&app, AgentId(0)).team_mode_active);

    let _ = handle(team_changed("sess-team", true), &mut app);
    assert!(test_agent(&app, AgentId(0)).team_mode_active);

    let _ = handle(team_changed("sess-team", false), &mut app);
    assert!(!test_agent(&app, AgentId(0)).team_mode_active);
}

#[test]
fn replayed_team_mode_changed_restores_the_indicator() {
    let mut app = make_app_with_agent("sess-team");
    app.agents
        .get_mut(&AgentId(0))
        .unwrap()
        .session
        .loading_replay = true;
    let _ = handle(team_changed("sess-team", true), &mut app);
    assert!(test_agent(&app, AgentId(0)).team_mode_active);
}
