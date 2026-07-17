use anyhow::Result;
use clap::Subcommand;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use xai_acp_lib::acp_send;
use xai_grok_shell::agent::config::Config as AgentConfig;
use xai_grok_shell::auth::{AuthManager, try_ensure_fresh_auth};
use xai_grok_shell::extensions::session_notify::{
    MAX_NOTIFICATION_TEXT_BYTES, SessionNotifyRequest, SessionNotifyResponse, SessionNotifyStatus,
};
use xai_grok_shell::leader::{
    ClientCapabilities, ClientMode, LeaderClient, ReconnectPolicy, socket_path_for_ws_url,
};
use xai_grok_shell::session::merge::MergedSession;
use xai_grok_shell::util::grok_home::grok_home;

// Server-side delivery can spend up to 60s behind an in-flight session/load,
// then up to 10s awaiting the actor mailbox ACK. Leave a small transport
// margin so the CLI never reports timeout before the server's own contract.
const NOTIFY_RESPONSE_TIMEOUT: Duration = Duration::from_secs(75);

#[derive(Debug, clap::Args, Clone)]
pub struct SessionsArgs {
    #[command(subcommand)]
    command: SessionsCommand,
}

#[derive(Debug, Subcommand, Clone)]
enum SessionsCommand {
    /// List recent sessions (same as search with no query)
    List {
        /// Maximum number of sessions to show
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
    },
    /// Search sessions by keyword
    Search {
        /// Search query (searches summaries and first prompts).
        query: String,
        /// Maximum number of sessions to show
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
    },
    /// Permanently delete a session from history
    Delete {
        /// Session id to delete.
        id: String,
    },
    /// Inject an external-agent result into a live leader-hosted session
    Notify {
        /// Live target session ID.
        #[arg(long)]
        session: String,
        /// Stable idempotency key, for example `review:<repo>:<commit>`.
        #[arg(long = "id")]
        notification_id: String,
        /// External agent/source label.
        #[arg(long, default_value = "reviewer")]
        kind: String,
        /// Notification text supplied directly on the command line.
        #[arg(
            long,
            conflicts_with = "message_file",
            required_unless_present = "message_file"
        )]
        message: Option<String>,
        /// Read notification text from a UTF-8 file, or `-` for stdin.
        #[arg(
            long,
            value_name = "PATH",
            value_hint = clap::ValueHint::FilePath,
            conflicts_with = "message",
            required_unless_present = "message"
        )]
        message_file: Option<PathBuf>,
        /// Start a model turn when the target session is idle.
        #[arg(long)]
        wake: bool,
        /// Emit the acknowledgement as JSON.
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(args: SessionsArgs, agent_config: &AgentConfig) -> Result<()> {
    if let SessionsCommand::Notify {
        session,
        notification_id,
        kind,
        message,
        message_file,
        wake,
        json,
    } = &args.command
    {
        let text = read_notification_text(message.as_deref(), message_file.as_deref())?;
        return notify_live_session(
            agent_config,
            SessionNotifyRequest {
                session_id: session.clone(),
                notification_id: notification_id.clone(),
                kind: kind.clone(),
                text,
                wake: *wake,
            },
            *json,
        )
        .await;
    }

    // Best-effort only. Do not force an interactive public login for enterprise
    // deployments that only configure a deployment_key + custom xai_api_base_url.
    // If the user has previously run the interactive `grok` TUI (which succeeds
    // for these setups), any cached credential will be used. Otherwise we still
    // proceed so the SessionRegistryClient can use the deployment_key when
    // talking to the custom proxy.
    let auth = try_ensure_fresh_auth(&agent_config.grok_com_config).await;

    let auth_manager = std::sync::Arc::new(AuthManager::new(
        &grok_home(),
        agent_config.grok_com_config.clone(),
    ));

    let client = xai_grok_shell::agent::session_registry_client::SessionRegistryClient::new(
        agent_config.endpoints.proxy_url(),
        String::new(),
    )
    .with_deployment_key(agent_config.endpoints.deployment_key.clone())
    .with_alpha_test_key(agent_config.endpoints.alpha_test_key.clone())
    .with_auth(auth_manager.clone());

    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());

    match args.command {
        SessionsCommand::List { limit } => {
            let sessions = xai_grok_shell::session::merge::fetch_merged(
                Some(&client),
                cwd.to_str(),
                None,
                limit,
            )
            .await;
            print_sessions_grouped(&sessions);
        }
        SessionsCommand::Search { query, limit } => {
            use std::collections::HashSet;
            use xai_grok_shell::session::merge::REMOTE_TIMEOUT;
            use xai_grok_shell::session::storage::search::{SessionSearchRequest, execute_search};

            let req = SessionSearchRequest {
                query,
                cwd: Some(cwd.to_string_lossy().to_string()),
                limit,
                offset: 0,
                include_content: true,
            };
            let root = grok_home();

            let remote_limit = (limit * 3).max(100) as i64;
            let (local_resp, remote_results) = tokio::join!(execute_search(&root, &req), async {
                tokio::time::timeout(
                    REMOTE_TIMEOUT,
                    client.search(Some(&req.query), remote_limit),
                )
                .await
                .unwrap_or_else(|_| {
                    eprintln!(
                        "warning: remote session search timed out, showing local results only"
                    );
                    Ok(Vec::new())
                })
                .unwrap_or_else(|e| {
                    eprintln!("warning: remote session search failed: {e}");
                    Vec::new()
                })
            });

            let resp = local_resp?;
            let local_ids: HashSet<&str> =
                resp.results.iter().map(|r| r.session_id.as_str()).collect();

            for hit in &resp.results {
                let title = if hit.title.is_empty() {
                    "(untitled)"
                } else {
                    &hit.title
                };
                let time = chrono::DateTime::from_timestamp(hit.updated_at_unix, 0)
                    .map(|dt| {
                        dt.with_timezone(&chrono::Local)
                            .format("%b %d, %l:%M%P")
                            .to_string()
                    })
                    .unwrap_or_default();
                println!(
                    "{} (score: {:.2})  {}\n  {}\n  {}",
                    hit.session_id,
                    hit.score,
                    time,
                    title,
                    hit.snippet.as_deref().unwrap_or("")
                );
            }

            let remaining = limit.saturating_sub(resp.results.len());
            let mut remote_shown = 0usize;
            for r in &remote_results {
                if remote_shown >= remaining {
                    break;
                }
                if local_ids.contains(r.session_id.as_str()) {
                    continue;
                }
                let title = if r.summary.is_empty() {
                    "(untitled)"
                } else {
                    &r.summary
                };
                let time = chrono::DateTime::parse_from_rfc3339(&r.updated_at)
                    .map(|dt| {
                        dt.with_timezone(&chrono::Local)
                            .format("%b %d, %l:%M%P")
                            .to_string()
                    })
                    .unwrap_or_default();
                let snippet: String = r
                    .first_prompt
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect();
                println!(
                    "{} (remote)  {}\n  {}\n  {}",
                    r.session_id, time, title, snippet
                );
                remote_shown += 1;
            }

            println!("\nTotal: {}", resp.results.len() + remote_shown);
        }
        SessionsCommand::Delete { id } => {
            // Always attempt the remote delete when authenticated and not
            // ZDR — `list` / `search` likewise query remote unconditionally
            // rather than gating on storage mode (which the CLI cannot
            // resolve here: it builds config without remote settings). The
            // backend delete is idempotent (a `404` is treated as success),
            // so this is safe for local-only sessions with no remote copy.
            // ZDR teams never upload, so there is nothing remote to delete.
            let needs_remote = auth.as_ref().is_some_and(|a| !a.is_zdr_team());

            // Pass `cwd = None` so the session is found by id regardless of
            // which workspace it was created in; the local delete still uses
            // the resolved per-session cwd.
            let deletion = xai_grok_shell::session::persistence::delete_session_history(
                &id,
                None,
                needs_remote,
                auth_manager.clone(),
            )
            .await?;

            if deletion.any_removed() {
                println!("Deleted session {id}");
            } else {
                println!("No session found with id {id}.");
            }
        }
        SessionsCommand::Notify { .. } => {
            unreachable!("notify is handled before auth/session-registry setup")
        }
    }

    Ok(())
}

fn read_notification_text(message: Option<&str>, message_file: Option<&Path>) -> Result<String> {
    let text = match (message, message_file) {
        (Some(message), None) => message.to_string(),
        (None, Some(path)) if path == Path::new("-") => {
            read_bounded_notification_text(std::io::stdin().lock())?
        }
        (None, Some(path)) => {
            let file = std::fs::File::open(path)
                .map_err(|e| anyhow::anyhow!("failed to open {}: {e}", path.display()))?;
            read_bounded_notification_text(file)
                .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?
        }
        _ => anyhow::bail!("exactly one of --message or --message-file is required"),
    };
    if text.trim().is_empty() {
        anyhow::bail!("notification message must not be empty");
    }
    if text.len() > MAX_NOTIFICATION_TEXT_BYTES {
        anyhow::bail!(
            "notification message exceeds the {}-byte limit",
            MAX_NOTIFICATION_TEXT_BYTES
        );
    }
    Ok(text)
}

/// Read at most one byte beyond the wire limit, so an accidental device,
/// pipe, or very large report cannot make the short-lived notifier allocate
/// the entire input before rejecting it.
fn read_bounded_notification_text(reader: impl Read) -> Result<String> {
    let mut bytes = Vec::with_capacity(MAX_NOTIFICATION_TEXT_BYTES + 1);
    reader
        .take((MAX_NOTIFICATION_TEXT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_NOTIFICATION_TEXT_BYTES {
        anyhow::bail!(
            "notification message exceeds the {}-byte limit",
            MAX_NOTIFICATION_TEXT_BYTES
        );
    }
    String::from_utf8(bytes)
        .map_err(|e| anyhow::anyhow!("notification message must be valid UTF-8: {e}"))
}

async fn notify_live_session(
    agent_config: &AgentConfig,
    request: SessionNotifyRequest,
    json: bool,
) -> Result<()> {
    let socket = socket_path_for_ws_url(&agent_config.grok_com_config.grok_ws_url);
    let client = LeaderClient::connect(
        socket.clone(),
        "grok-session-notify-cli",
        ClientMode::Stdio,
        ClientCapabilities {
            client_version: Some(crate::client_identity::PAGER_CLIENT_VERSION.to_string()),
            ..ClientCapabilities::default()
        },
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "no running leader is available at {} ({e}); \
             session notify only targets live leader-hosted sessions",
            socket.display()
        )
    })?;

    // Use the typed ACP bridge so ExtRequest keeps the exact same wire shape
    // as every other client. Deliberately do not call connect_or_spawn: a
    // notifier must never create a second owner for the parent session.
    let cancel = CancellationToken::new();
    let (leader_tx, leader_rx) = client.into_channels();
    let bridge = crate::acp::leader_bridge::bridge_channels(
        leader_tx,
        leader_rx,
        cancel.clone(),
        None,
        ReconnectPolicy::bounded(),
    )?;
    let params = serde_json::value::to_raw_value(&request)?;
    let ext_request = agent_client_protocol::ExtRequest::new("x.ai/session/notify", params.into());

    let result = tokio::time::timeout(
        NOTIFY_RESPONSE_TIMEOUT,
        acp_send(ext_request, &bridge.channel.tx),
    )
    .await;
    cancel.cancel();

    let ext_response: agent_client_protocol::ExtResponse = result
        .map_err(|_| {
            anyhow::anyhow!(
                "session notify timed out after {} seconds",
                NOTIFY_RESPONSE_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| anyhow::anyhow!("session notify failed: {e}"))?;
    let response: SessionNotifyResponse = serde_json::from_str(ext_response.0.get())
        .map_err(|e| anyhow::anyhow!("invalid session notify response: {e}"))?;

    if json {
        println!("{}", serde_json::to_string(&response)?);
    } else {
        match response.status {
            SessionNotifyStatus::Queued => {
                if response.turn_running {
                    println!(
                        "Queued {} notification {} for the active turn.",
                        request.kind, response.notification_id
                    );
                } else if response.will_wake {
                    println!(
                        "Queued {} notification {} and requested a new turn.",
                        request.kind, response.notification_id
                    );
                } else {
                    println!(
                        "Queued {} notification {} for the session's next turn.",
                        request.kind, response.notification_id
                    );
                }
            }
            SessionNotifyStatus::Duplicate => {
                println!(
                    "Notification {} was already accepted by this resident session actor.",
                    response.notification_id
                );
            }
        }
    }
    Ok(())
}

/// Print sessions grouped by worktree label, preserving the original table
/// format with a `Label: <label>` header before each group.
fn print_sessions_grouped(sessions: &[MergedSession]) {
    if sessions.is_empty() {
        println!("No sessions found.");
        return;
    }

    // Group by worktree_label, sort alphabetically, None last.
    let mut groups: std::collections::BTreeMap<Option<&str>, Vec<&MergedSession>> =
        std::collections::BTreeMap::new();
    for s in sessions {
        groups
            .entry(s.worktree_label.as_deref())
            .or_default()
            .push(s);
    }

    let header = format!(
        "{:<36}  {:<10}  {:<10}  {:<10}  {}",
        "SESSION ID", "CREATED", "UPDATED", "STATUS", "SUMMARY"
    );

    // Labeled groups first (alphabetical), then unlabeled last.
    let none_group = groups.remove(&None);
    let print_group = |label_line: &str, members: &[&MergedSession]| {
        println!("\n{label_line}");
        println!("{header}");
        for s in members {
            let first_line;
            let summary: &str = if !s.summary.is_empty() {
                &s.summary
            } else if let Some(ref fp) = s.first_prompt
                && let Some(line) = fp.lines().find(|l| !l.trim().is_empty())
            {
                first_line = line.trim().to_string();
                &first_line
            } else {
                "(no summary)"
            };
            let truncated: String = summary.chars().take(50).collect();
            let created = &s.created_at[..s.created_at.len().min(10)];
            let updated = &s.updated_at[..s.updated_at.len().min(10)];
            println!(
                "{}  {}  {}  {}  {}",
                s.session_id, created, updated, s.source, truncated
            );
        }
    };

    for (label, members) in &groups {
        let line = format!("Label: {}", label.unwrap_or(""));
        print_group(&line, members);
    }
    if let Some(members) = &none_group {
        print_group("(no label)", members);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn notify_cli_accepts_message_file_and_wake() {
        let args = crate::app::cli::PagerArgs::try_parse_from([
            "grok",
            "sessions",
            "notify",
            "--session",
            "session-1",
            "--id",
            "review:repo:abc",
            "--message-file",
            "review.txt",
            "--wake",
            "--json",
        ])
        .expect("notify CLI should parse");
        let Some(crate::app::cli::Command::Sessions(args)) = args.command else {
            panic!("expected sessions command");
        };
        assert!(matches!(
            args.command,
            SessionsCommand::Notify {
                wake: true,
                json: true,
                ..
            }
        ));
    }

    #[test]
    fn notify_cli_requires_exactly_one_message_source() {
        assert!(
            crate::app::cli::PagerArgs::try_parse_from([
                "grok",
                "sessions",
                "notify",
                "--session",
                "session-1",
                "--id",
                "review:abc",
            ])
            .is_err()
        );
        assert!(
            crate::app::cli::PagerArgs::try_parse_from([
                "grok",
                "sessions",
                "notify",
                "--session",
                "session-1",
                "--id",
                "review:abc",
                "--message",
                "inline",
                "--message-file",
                "review.txt",
            ])
            .is_err()
        );
    }

    #[test]
    fn reads_utf8_notification_file_and_enforces_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review.txt");
        std::fs::write(&path, "review complete\n").unwrap();
        assert_eq!(
            read_notification_text(None, Some(&path)).unwrap(),
            "review complete\n"
        );

        let oversized = "x".repeat(MAX_NOTIFICATION_TEXT_BYTES + 1);
        assert!(read_notification_text(Some(&oversized), None).is_err());

        let oversized_path = dir.path().join("oversized-review.txt");
        std::fs::write(&oversized_path, oversized).unwrap();
        let error = read_notification_text(None, Some(&oversized_path)).unwrap_err();
        assert!(
            error.to_string().contains("byte limit"),
            "file input must be rejected after a bounded read: {error:#}"
        );
    }

    #[test]
    fn rejects_non_utf8_notification_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binary-review.txt");
        std::fs::write(&path, [0xf0, 0x28, 0x8c, 0x28]).unwrap();
        let error = read_notification_text(None, Some(&path)).unwrap_err();
        assert!(
            error.to_string().contains("valid UTF-8"),
            "notification reports must be valid UTF-8: {error:#}"
        );
    }
}
