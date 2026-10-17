use super::support::*;
use super::*;

struct EnvRestore {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvRestore {
    fn set(key: &'static str, value: Option<&str>) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        Self { key, original }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        unsafe {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

fn route_manager() -> crate::agent::remote_config::ModelsManager {
    let raw = toml::from_str::<toml::Value>(
        r#"
[provider.primary]
base_url = "https://primary.example/v1"
env_key = "GROK_ACTOR_ROUTE_PRIMARY_KEY"

[provider.secondary]
base_url = "https://secondary.example/v1"
env_key = "GROK_ACTOR_ROUTE_SECONDARY_KEY"

[model.primary-shared]
provider = "primary"
model = "shared-upstream-slug"
context_window = 131072
auto_compact_threshold_percent = 70

[model.secondary-shared]
provider = "secondary"
model = "shared-upstream-slug"
context_window = 65536
context_windows = [32768, 65536]
auto_compact_threshold_percent = 80

[model_route.main]
candidates = ["primary-shared", "secondary-shared"]
"#,
    )
    .expect("valid route test TOML");
    let cfg =
        crate::agent::config::Config::new_from_toml_cfg(&raw).expect("valid route test config");
    let catalog = crate::agent::remote_config::resolve_model_catalog(&cfg, None);
    let auth_root = std::env::temp_dir().join("grok-route-preflight-auth");
    let auth_manager = std::sync::Arc::new(crate::auth::AuthManager::new(
        &auth_root,
        crate::auth::GrokComConfig::default(),
    ));
    crate::agent::remote_config::ModelsManager::new(
        None,
        catalog,
        acp::ModelId::new("route:main"),
        auth_manager,
        cfg,
    )
}

fn auth_none_manager(base_url: &str) -> crate::agent::remote_config::ModelsManager {
    let raw = toml::from_str::<toml::Value>(&format!(
        r#"
[provider.anon]
base_url = "{base_url}"
auth = "none"
api_backend = "chat_completions"

[model.anon-direct]
provider = "anon"
model = "anonymous-upstream"
context_window = 131072

[model_route.anon]
candidates = ["anon-direct"]
"#,
    ))
    .expect("valid auth-none provider TOML");
    let cfg = crate::agent::config::Config::new_from_toml_cfg(&raw)
        .expect("valid auth-none provider config");
    let catalog = crate::agent::remote_config::resolve_model_catalog(&cfg, None);
    let auth_root = std::env::temp_dir().join("grok-auth-none-provider-manager");
    let auth_manager = std::sync::Arc::new(crate::auth::AuthManager::new(
        &auth_root,
        crate::auth::GrokComConfig::default(),
    ));
    crate::agent::remote_config::ModelsManager::new(
        None,
        catalog,
        acp::ModelId::new("anon-direct"),
        auth_manager,
        cfg,
    )
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn resident_session_reselects_route_before_each_request_and_fails_closed() {
    const PRIMARY: &str = "GROK_ACTOR_ROUTE_PRIMARY_KEY";
    const SECONDARY: &str = "GROK_ACTOR_ROUTE_SECONDARY_KEY";
    let _primary_restore = EnvRestore::set(PRIMARY, None);
    let _secondary_restore = EnvRestore::set(SECONDARY, Some("secondary-secret"));

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let (mut actor, _event_rx) =
                create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.models_manager = route_manager();
            let actor = std::sync::Arc::new(actor);

            let (entry, initial) = actor
                .models_manager
                .sampling_config_for_model_ref("route:main")
                .expect("secondary route candidate");
            let selected =
                actor
                    .handle_set_session_model(crate::session::SessionModelSwitch {
                        sampling_config: initial,
                        use_concise: entry.info.use_concise,
                        is_family_switch: false,
                        apply_prompt_override: false,
                        skip_prompt_rewrite: true,
                        auto_compact_threshold_percent: 80,
                        system_prompt_label: entry.info.system_prompt_label.clone().unwrap_or_else(
                            || xai_grok_agent::DEFAULT_SYSTEM_PROMPT_LABEL.to_owned(),
                        ),
                        context_window_selection: crate::session::SwitchContextWindow::Preserve,
                        supported_context_windows: Vec::new(),
                    })
                    .await
                    .expect("route model switch");
            assert_eq!(selected.0.as_ref(), "route:main");
            assert!(matches!(
                persistence_rx.try_recv(),
                Ok(PersistenceMsg::CurrentModel { model_id, .. })
                    if model_id.0.as_ref() == "route:main"
            ));

            actor
                .preflight_active_route_for_request()
                .await
                .expect("secondary preflight");
            let secondary = actor
                .chat_state_handle
                .get_sampling_config()
                .await
                .expect("secondary config");
            assert_eq!(secondary.route_ref.as_deref(), Some("route:main"));
            assert_eq!(secondary.model_ref.as_deref(), Some("secondary-shared"));
            assert_eq!(secondary.base_url, "https://secondary.example/v1");
            assert_eq!(secondary.context_window.get(), 65_536);
            assert_eq!(actor.compaction.threshold_percent.get(), 80);
            assert_eq!(
                actor
                    .chat_state_handle
                    .get_credentials()
                    .await
                    .api_key
                    .as_deref(),
                Some("secondary-secret")
            );

            unsafe {
                std::env::set_var(PRIMARY, "primary-secret");
            }
            actor.refresh_sampler_for_retry().await;
            let retried = actor
                .chat_state_handle
                .get_sampling_config()
                .await
                .expect("retry config");
            assert_eq!(retried.model_ref.as_deref(), Some("secondary-shared"));
            assert_eq!(retried.base_url, "https://secondary.example/v1");
            assert_eq!(
                actor
                    .chat_state_handle
                    .get_credentials()
                    .await
                    .api_key
                    .as_deref(),
                Some("secondary-secret"),
                "retry must not adopt a newly available candidate"
            );
            actor
                .preflight_active_route_for_request()
                .await
                .expect("primary preflight in the same resident actor");
            let primary = actor
                .chat_state_handle
                .get_sampling_config()
                .await
                .expect("primary config");
            assert_eq!(primary.route_ref.as_deref(), Some("route:main"));
            assert_eq!(primary.model_ref.as_deref(), Some("primary-shared"));
            assert_eq!(primary.base_url, "https://primary.example/v1");
            assert_eq!(primary.context_window.get(), 131_072);
            assert_eq!(actor.compaction.threshold_percent.get(), 70);
            assert_eq!(
                actor
                    .chat_state_handle
                    .get_credentials()
                    .await
                    .api_key
                    .as_deref(),
                Some("primary-secret")
            );

            unsafe {
                std::env::remove_var(PRIMARY);
                std::env::remove_var(SECONDARY);
            }
            let error = actor
                .preflight_active_route_for_request()
                .await
                .expect_err("an unavailable route must fail before sampler submission");
            assert!(
                error.to_string().contains("preflight-available"),
                "unexpected route error: {error}"
            );
            let unchanged = actor
                .chat_state_handle
                .get_sampling_config()
                .await
                .expect("diagnostic snapshot remains available");
            assert_eq!(unchanged.route_ref.as_deref(), Some("route:main"));
            assert_eq!(unchanged.model_ref.as_deref(), Some("primary-shared"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn route_preflight_preserves_supported_context_window_selection() {
    const PRIMARY: &str = "GROK_ACTOR_ROUTE_PRIMARY_KEY";
    const SECONDARY: &str = "GROK_ACTOR_ROUTE_SECONDARY_KEY";
    let _primary_restore = EnvRestore::set(PRIMARY, None);
    let _secondary_restore = EnvRestore::set(SECONDARY, Some("secondary-secret"));
    tokio::task::LocalSet::new()
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let (mut actor, _event_rx) =
                create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.models_manager = route_manager();
            let actor = std::sync::Arc::new(actor);
            let (entry, sampling_config) = actor
                .models_manager
                .sampling_config_for_model_ref("route:main")
                .expect("secondary route candidate");
            let selection = std::num::NonZeroU64::new(32_768).unwrap();
            actor
                .handle_set_session_model(crate::session::SessionModelSwitch {
                    sampling_config,
                    use_concise: false,
                    is_family_switch: false,
                    apply_prompt_override: false,
                    skip_prompt_rewrite: true,
                    auto_compact_threshold_percent: 80,
                    system_prompt_label: xai_grok_agent::DEFAULT_SYSTEM_PROMPT_LABEL.to_owned(),
                    context_window_selection: crate::session::SwitchContextWindow::Set(Some(
                        selection,
                    )),
                    supported_context_windows: entry.info.context_windows.clone(),
                })
                .await
                .expect("select the route with a smaller context window");
            actor
                .prepare_sampler_for_turn()
                .await
                .expect("supported selection preflight");
            let selected = actor.chat_state_handle.get_sampling_config().await.unwrap();
            assert_eq!(selected.context_window, selection);

            unsafe {
                std::env::set_var(PRIMARY, "primary-secret");
            }
            actor
                .prepare_sampler_for_turn()
                .await
                .expect("new candidate preflight");
            let fallback = actor.chat_state_handle.get_sampling_config().await.unwrap();
            assert_eq!(fallback.model_ref.as_deref(), Some("primary-shared"));
            assert_eq!(
                fallback.context_window.get(),
                131_072,
                "a selection unsupported by the new candidate uses its default"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn auth_none_provider_never_emits_session_authorization_for_direct_or_route_requests() {
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{Value, json};

    #[derive(Debug, PartialEq, Eq)]
    struct CapturedAuthHeaders {
        authorization: Option<String>,
        x_api_key: Option<String>,
    }

    async fn capture_headers(
        State(tx): State<tokio::sync::mpsc::UnboundedSender<CapturedAuthHeaders>>,
        headers: HeaderMap,
        Json(_body): Json<Value>,
    ) -> Json<Value> {
        let authorization = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let x_api_key = headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        tx.send(CapturedAuthHeaders {
            authorization,
            x_api_key,
        })
        .expect("capture receiver remains live");
        Json(json!({
            "id": "chatcmpl-auth-boundary",
            "object": "chat.completion",
            "created": 1,
            "model": "anonymous-upstream",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2
            }
        }))
    }

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (header_tx, mut header_rx) = tokio::sync::mpsc::unbounded_channel();
            let app = Router::new()
                .route("/v1/chat/completions", post(capture_headers))
                .with_state(header_tx);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind auth capture server");
            let addr = listener.local_addr().expect("capture server address");
            let server = tokio::task::spawn_local(async move {
                axum::serve(listener, app)
                    .await
                    .expect("auth capture server");
            });
            let base_url = format!("http://{addr}/v1");

            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let (mut actor, _event_rx) =
                create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await;

            let auth_dir = tempfile::tempdir().expect("auth tempdir");
            let auth_manager = std::sync::Arc::new(crate::auth::AuthManager::new(
                auth_dir.path(),
                crate::auth::GrokComConfig::default(),
            ));
            auth_manager.hot_swap(crate::auth::GrokAuth {
                key: "must-never-cross-provider-boundary".to_owned(),
                auth_mode: crate::auth::AuthMode::Oidc,
                refresh_token: Some("refresh-token".to_owned()),
                expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                ..crate::auth::GrokAuth::test_default()
            });
            actor.auth_manager = Some(auth_manager);
            actor.auth_method_id = test_auth_method_id("oidc");
            actor.models_manager = auth_none_manager(&base_url);
            let actor = std::sync::Arc::new(actor);

            actor
                .chat_state_handle
                .update_credentials(xai_chat_state::Credentials {
                    api_key: Some("stale-session-token".to_owned()),
                    auth_type: xai_chat_state::AuthType::SessionToken,
                    ..Default::default()
                });
            let (direct_entry, direct_sampling) = actor
                .models_manager
                .sampling_config_for_model_ref("anon-direct")
                .expect("direct auth-none model");
            assert!(direct_entry.opts_out_of_ambient_credentials());
            actor
                .handle_set_session_model(crate::session::SessionModelSwitch {
                    sampling_config: direct_sampling,
                    use_concise: false,
                    is_family_switch: false,
                    apply_prompt_override: false,
                    skip_prompt_rewrite: true,
                    auto_compact_threshold_percent: 85,
                    system_prompt_label: direct_entry
                        .info
                        .system_prompt_label
                        .clone()
                        .unwrap_or_else(|| xai_grok_agent::DEFAULT_SYSTEM_PROMPT_LABEL.to_owned()),
                    context_window_selection: crate::session::SwitchContextWindow::Preserve,
                    supported_context_windows: Vec::new(),
                })
                .await
                .expect("switch to direct auth-none provider");
            let direct_client = actor
                .prepare_chat_completion(false)
                .await
                .expect("prepare direct auth-none request");
            let direct = actor.reconstruct_full_config().await;
            assert_eq!(direct.api_key, None);
            assert!(direct.bearer_resolver.is_none());
            direct_client
                .conversation(xai_grok_sampling_types::ConversationRequest::from_items(
                    vec![xai_grok_sampling_types::ConversationItem::user("direct")],
                ))
                .await
                .expect("direct request succeeds");

            actor
                .chat_state_handle
                .update_credentials(xai_chat_state::Credentials {
                    api_key: Some("stale-session-token".to_owned()),
                    auth_type: xai_chat_state::AuthType::SessionToken,
                    ..Default::default()
                });
            let (route_entry, route_sampling) = actor
                .models_manager
                .sampling_config_for_model_ref("route:anon")
                .expect("auth-none route");
            actor
                .handle_set_session_model(crate::session::SessionModelSwitch {
                    sampling_config: route_sampling,
                    use_concise: false,
                    is_family_switch: false,
                    apply_prompt_override: false,
                    skip_prompt_rewrite: true,
                    auto_compact_threshold_percent: 85,
                    system_prompt_label: route_entry
                        .info
                        .system_prompt_label
                        .clone()
                        .unwrap_or_else(|| xai_grok_agent::DEFAULT_SYSTEM_PROMPT_LABEL.to_owned()),
                    context_window_selection: crate::session::SwitchContextWindow::Preserve,
                    supported_context_windows: Vec::new(),
                })
                .await
                .expect("switch to auth-none route");
            let route_client = actor
                .prepare_chat_completion(false)
                .await
                .expect("prepare request-time auth-none route");
            let route = actor.reconstruct_full_config().await;
            assert_eq!(route.api_key, None);
            assert!(route.bearer_resolver.is_none());
            route_client
                .conversation(xai_grok_sampling_types::ConversationRequest::from_items(
                    vec![xai_grok_sampling_types::ConversationItem::user("route")],
                ))
                .await
                .expect("route request succeeds");

            assert_eq!(
                header_rx.recv().await,
                Some(CapturedAuthHeaders {
                    authorization: None,
                    x_api_key: None,
                }),
                "direct provider request must omit authentication headers"
            );
            assert_eq!(
                header_rx.recv().await,
                Some(CapturedAuthHeaders {
                    authorization: None,
                    x_api_key: None,
                }),
                "route provider request must omit authentication headers"
            );
            server.abort();
        })
        .await;
}

fn cold_helper_route_manager(
    provider: xai_grok_login::AuthProviderRef,
) -> crate::agent::remote_config::ModelsManager {
    let raw = toml::from_str::<toml::Value>(
        r#"
        [auth_provider.route-helper]
        command = "false"

        [model_providers.gateway]
        base_url = "https://helper-route.example/v1"
        auth_scheme = "bearer"
        auth_provider = "route-helper"

        [model.selected]
        model_provider = "gateway"
        model = "shared-wire-slug"

        [model_route.main]
        candidates = ["selected"]
        "#,
    )
    .unwrap();
    let cfg = crate::agent::config::Config::new_from_toml_cfg(&raw).unwrap();
    let mut catalog = crate::agent::remote_config::resolve_model_catalog(&cfg, None);
    catalog.get_mut("selected").unwrap().auth_provider = Some(provider);
    let auth_root = tempfile::tempdir().unwrap();
    let auth_manager = std::sync::Arc::new(crate::auth::AuthManager::new(
        auth_root.path(),
        crate::auth::GrokComConfig::default(),
    ));
    crate::agent::remote_config::ModelsManager::new(
        None,
        catalog,
        acp::ModelId::new("route:main"),
        auth_manager,
        cfg,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn cold_route_helper_mints_before_first_request_and_failure_blocks_dispatch() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            for mint_succeeds in [true, false] {
                let dir = tempfile::tempdir().unwrap();
                let provider = if mint_succeeds {
                    xai_grok_login::test_counting_provider("route-cold-counting", dir.path())
                } else {
                    xai_grok_login::AuthProviderRef::new(
                        "route-cold-failing".to_owned(),
                        xai_grok_login::AuthProviderConfig {
                            command: "exit 1".to_owned(),
                            args: None,
                            token_ttl_secs: Some(3600),
                            timeout_secs: None,
                            cwd: None,
                        },
                    )
                };
                let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
                let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
                let (mut actor, _event_rx) =
                    create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await;
                actor.models_manager = cold_helper_route_manager(provider);
                let actor = std::sync::Arc::new(actor);
                let (_entry, initial) = actor
                    .models_manager
                    .sampling_config_for_model_ref("route:main")
                    .expect("cold helper remains a selectable candidate");
                assert!(initial.api_key.is_none());
                let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
                cfg.route_ref = Some("route:main".to_owned());
                cfg.base_url = "https://previous-provider.example/v1".to_owned();
                actor.chat_state_handle.update_sampling_config(cfg);
                let mut credentials = actor.chat_state_handle.get_credentials().await;
                credentials.api_key = Some("previous-provider-token".to_owned());
                actor.chat_state_handle.update_credentials(credentials);

                let result = actor.preflight_active_route_for_request().await;
                let current = actor.chat_state_handle.get_sampling_config().await.unwrap();
                let credentials = actor.chat_state_handle.get_credentials().await;
                if mint_succeeds {
                    result.expect("mint before the first route request");
                    assert_eq!(current.model_ref.as_deref(), Some("selected"));
                    assert_eq!(current.base_url, "https://helper-route.example/v1");
                    assert_eq!(credentials.api_key.as_deref(), Some("tok-1"));
                    actor.preflight_active_route_for_request().await.unwrap();
                    assert_eq!(
                        std::fs::read_to_string(dir.path().join("count"))
                            .unwrap()
                            .lines()
                            .count(),
                        1
                    );
                } else {
                    assert!(result.is_err(), "failed helper must block dispatch");
                    assert_eq!(current.base_url, "https://previous-provider.example/v1");
                    assert_eq!(
                        credentials.api_key.as_deref(),
                        Some("previous-provider-token")
                    );
                }
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_route_helper_cannot_overwrite_a_concurrent_model_switch() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let dir = tempfile::tempdir().unwrap();
        let started = dir.path().join("started");
        let release = dir.path().join("release");
        let provider = xai_grok_login::AuthProviderRef::new(
            "delayed-route-selection-cas".to_owned(),
            xai_grok_login::AuthProviderConfig {
                command: format!(
                    "touch '{}'; while [ ! -f '{}' ]; do sleep 0.01; done; printf old-route-token",
                    started.display(), release.display(),
                ),
                args: None,
                token_ttl_secs: Some(3600),
                timeout_secs: Some(5),
                cwd: None,
            },
        );
        let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
        let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut actor, _event_rx) = create_test_actor_ex(
            0, 256_000, 85, gateway_tx, persistence_tx,
        ).await;
        actor.models_manager = cold_helper_route_manager(provider);
        let actor = std::sync::Arc::new(actor);
        let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
        cfg.route_ref = Some("route:main".to_owned());
        actor.chat_state_handle.update_sampling_config(cfg.clone());
        let preflight = async { actor.preflight_active_route_for_request().await };
        let switch = async {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !started.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }).await.expect("route helper reached the barrier");
            cfg.model_ref = Some("new-user-selection".to_owned());
            cfg.route_ref = None;
            cfg.base_url = "https://user-selection.example/v1".to_owned();
            let mut credentials = actor.chat_state_handle.get_credentials().await;
            credentials.api_key = Some("new-user-token".to_owned());
            actor.chat_state_handle.replace_sampling_config_and_credentials(
                cfg, credentials,
            ).await.unwrap();
            std::fs::write(&release, "continue").unwrap();
        };
        let (result, _) = tokio::join!(preflight, switch);
        assert!(result.is_err(), "stale route preflight must block dispatch");
        let (cfg, credentials) = actor.chat_state_handle.get_sampling_config_and_credentials().await.unwrap();
        assert_eq!(cfg.model_ref.as_deref(), Some("new-user-selection"));
        assert!(cfg.route_ref.is_none());
        assert_eq!(credentials.api_key.as_deref(), Some("new-user-token"));
        assert!(actor.model_auth_memo.borrow().is_none(), "stale result must not publish provider memo");
    }).await;
}
