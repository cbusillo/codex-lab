//! End-to-end coverage for cancelling a *running* Background Review over
//! `review/background/control`.
//!
//! `suite::v2::review` already covers the request-validation path, but nothing
//! proved that the RPC reaches a live run: it has to interrupt the in-flight
//! review turn, publish a terminal `review/backgroundStatus/changed`, and leave
//! a durable cancel reason behind for the next client that reads the run.

#![cfg(unix)]

use anyhow::Context;
use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::BackgroundAutoReviewControlAction;
use codex_app_server_protocol::BackgroundAutoReviewControlParams;
use codex_app_server_protocol::BackgroundAutoReviewControlReason;
use codex_app_server_protocol::BackgroundAutoReviewStatus;
use codex_app_server_protocol::BackgroundAutoReviewStatusChangedNotification;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_auto_review::AutoReviewRunStatus;
use codex_auto_review::AutoReviewStore;
use core_test_support::responses;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::time::timeout;

/// The scheduler debounces for 2s before launching the review, so allow slack.
const STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

const ADD_FEATURE_PATCH: &str = "*** Begin Patch\n*** Add File: feature.rs\n+pub fn feature() -> u32 {\n+    7\n+}\n*** End Patch";

fn run_git(cwd: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Background Review only schedules when the worktree fingerprint changes, so
/// the turn has to run inside a repository with a committed baseline.
fn init_git_repo(path: &Path) -> Result<()> {
    for args in [
        &["init", "--quiet", "-b", "main"][..],
        &["config", "user.email", "background-review@example.invalid"][..],
        &["config", "user.name", "Background Review"][..],
        &["commit", "--quiet", "--allow-empty", "-m", "baseline"][..],
    ] {
        run_git(path, args)?;
    }
    Ok(())
}

fn chunk(event: Value) -> StreamingSseChunk {
    StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![event]),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_auto_review_control_cancels_a_running_review() -> Result<()> {
    // The review response never arrives: the gate is held for the lifetime of
    // the test so the run stays `Running` until the client cancels it.
    let (_review_gate_tx, review_gate_rx) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            chunk(responses::ev_response_created("resp-turn-tool")),
            chunk(responses::ev_apply_patch_exec_command_call_via_heredoc(
                "call-patch",
                ADD_FEATURE_PATCH,
            )),
            chunk(responses::ev_completed("resp-turn-tool")),
        ],
        vec![
            chunk(responses::ev_response_created("resp-turn-final")),
            chunk(responses::ev_assistant_message(
                "msg-final",
                "added the feature",
            )),
            chunk(responses::ev_completed("resp-turn-final")),
        ],
        vec![StreamingSseChunk {
            gate: Some(review_gate_rx),
            body: responses::sse(vec![responses::ev_completed("resp-review")]),
        }],
    ])
    .await;

    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(server.uri())
        .with_provider_name("Mock provider")
        // The turn has to be able to write the patch into the worktree.
        .with_sandbox_mode("danger-full-access")
        .write(codex_home.path())?;

    let workspace = TempDir::new()?;
    let workspace_path = std::fs::canonicalize(workspace.path())?;
    init_git_repo(&workspace_path)?;

    // Auto-env would move the thread off the prepared repository, which is the
    // path Background Review scopes its store to.
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized()
        .await?;
    let ThreadStartResponse { thread, .. } = mcp
        .request(|request_id| ClientRequest::ThreadStart {
            request_id,
            params: ThreadStartParams {
                model: Some("mock-model".to_string()),
                cwd: Some(workspace_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        })
        .await?;
    let thread_id = thread.id;

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread_id.clone(),
                client_user_message_id: None,
                input: vec![UserInput::Text {
                    text: "add the feature".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let running = read_background_status_until(&mut mcp, BackgroundAutoReviewStatus::Running)
        .await
        .context("background review must reach Running before it can be cancelled")?;
    assert_eq!(running.thread_id, thread_id);

    mcp.send_background_auto_review_control_request(BackgroundAutoReviewControlParams {
        thread_id: thread_id.clone(),
        run_id: running.run_id.clone(),
        action: BackgroundAutoReviewControlAction::Cancel,
        reason: BackgroundAutoReviewControlReason::UserRequested,
    })
    .await?;

    let cancelled = read_background_status_until(&mut mcp, BackgroundAutoReviewStatus::Cancelled)
        .await
        .context("cancel must publish a terminal status")?;
    assert_eq!(
        cancelled,
        BackgroundAutoReviewStatusChangedNotification {
            thread_id: thread_id.clone(),
            run_id: running.run_id.clone(),
            status: BackgroundAutoReviewStatus::Cancelled,
            review_target: running.review_target.clone(),
            error_summary: Some("background auto review was cancelled by request".to_string()),
        }
    );

    // The cancellation has to survive the session: a client that reads the run
    // later must be able to tell why it stopped.
    let store = AutoReviewStore::for_scope(codex_home.path(), &workspace_path);
    let run = store
        .load_run(&running.run_id)
        .context("cancelled run must be persisted")?;
    assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(
        run.cancel_reason.as_deref(),
        Some("background_auto_review_control")
    );
    assert_eq!(
        run.error_summary.as_deref(),
        Some("background auto review was cancelled by request")
    );

    server.shutdown().await;
    Ok(())
}

/// Drains `review/backgroundStatus/changed` notifications until the requested
/// status shows up.
async fn read_background_status_until(
    mcp: &mut TestAppServer,
    status: BackgroundAutoReviewStatus,
) -> Result<BackgroundAutoReviewStatusChangedNotification> {
    let deadline = tokio::time::Instant::now() + STATUS_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let notification = timeout(
            remaining,
            mcp.read_stream_until_notification_message("review/backgroundStatus/changed"),
        )
        .await??;
        let notification: BackgroundAutoReviewStatusChangedNotification = serde_json::from_value(
            notification
                .params
                .context("background status notification must carry params")?,
        )?;
        if notification.status == status {
            return Ok(notification);
        }
    }
}
