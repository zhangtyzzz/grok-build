//! A separate test executable can initialize a release identity without changing other tests' development identity.

use std::time::Duration;

use xai_grok_shell::leader::{ClientCapabilities, ClientMode, LeaderClient, spawn_leader_server};

#[tokio::test]
async fn release_registration_compares_plain_runtime_versions() {
    // This separate current-thread test process must not inherit the trust bypass.
    unsafe { std::env::remove_var(xai_grok_version::TEST_VERSION_ENV) };
    xai_grok_telemetry::unified_log::redirect_to_temp_for_tests();
    const VERSION: &str = "1.2.3-alpha.4";
    xai_grok_version::initialize(xai_grok_version::BuildInfo {
        version: VERSION,
        full_version: "1.2.3-alpha.4 (abcdef012345)",
        is_dev_build: false,
    })
    .unwrap();
    assert!(!xai_grok_version::is_dev_build());
    assert!(
        !xai_grok_workspace::folder_trust::folder_trust_inert(),
        "a runtime release identity must retain the folder-trust gate"
    );

    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("leader.sock");
    let server = spawn_leader_server(socket.clone()).await.unwrap();
    let mut matching = LeaderClient::connect(
        socket.clone(),
        "version-test",
        ClientMode::Stdio,
        ClientCapabilities {
            client_version: Some(VERSION.to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        matching.registration().leader_binary_version.as_deref(),
        Some(VERSION)
    );

    let mut different = LeaderClient::connect(
        socket,
        "version-test",
        ClientMode::Stdio,
        ClientCapabilities {
            client_version: Some("1.2.3-alpha.5".to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let payload = tokio::time::timeout(Duration::from_secs(5), different.recv())
        .await
        .expect("a different release must receive a mismatch notification")
        .expect("the leader must keep the client connected");
    let notice: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        notice
            .pointer("/method")
            .and_then(serde_json::Value::as_str),
        Some("x.ai/leader/version_mismatch")
    );
    assert_eq!(
        notice
            .pointer("/params/leaderVersion")
            .and_then(serde_json::Value::as_str),
        Some(VERSION)
    );
    assert_eq!(
        notice
            .pointer("/params/clientVersion")
            .and_then(serde_json::Value::as_str),
        Some("1.2.3-alpha.5")
    );
    // The second registration proves the server processed the first registration too.
    // Including a commit suffix on the leader side would wrongly notify this matching client.
    assert!(
        tokio::time::timeout(Duration::from_millis(500), matching.recv())
            .await
            .is_err(),
        "equal plain versions must not emit a mismatch notification"
    );
    matching.cancel();
    different.cancel();
    server.cancel.cancel();
}
