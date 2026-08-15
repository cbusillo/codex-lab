//! End-to-end coverage for the `validation/completed` notification.
//!
//! Project Validation is configured in `config.toml` and runs inside the turn,
//! so the only way to prove the notification contract is to let a real turn
//! trigger a real validation command and read what lands on the wire. The client
//! here deliberately initializes *without* the experimental API capability:
//! `validation/completed` is stable surface and must reach ordinary clients.

use anyhow::Context;
use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::create_shell_command_sse_response;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ConfigBatchWriteParams;
use codex_app_server_protocol::ConfigEdit;
use codex_app_server_protocol::ConfigWriteResponse;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::MergeStrategy;
use codex_app_server_protocol::ProjectValidationCompletedNotification;
use codex_app_server_protocol::ProjectValidationSkipReason;
use codex_app_server_protocol::ProjectValidationStatus;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_app_server_protocol::WriteStatus;
use codex_core::config::set_project_trust_level;
use codex_protocol::config_types::TrustLevel;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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

/// Project Validation only runs inside a repository, so the workspace needs a
/// committed baseline before the turn starts.
fn init_git_repo(path: &Path) -> Result<()> {
    for args in [
        &["init", "--quiet"][..],
        &["config", "user.email", "validation@example.invalid"][..],
        &["config", "user.name", "Project Validation"][..],
        &["commit", "--quiet", "--allow-empty", "-m", "baseline"][..],
    ] {
        run_git(path, args)?;
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn project_validation_completion_reaches_a_client_without_experimental_api() -> Result<()> {
    let server = create_mock_responses_server_sequence(vec![
        create_shell_command_sse_response(
            vec!["true".to_string()],
            /*workdir*/ None,
            /*timeout_ms*/ None,
            "shell-call-1",
        )?,
        create_final_assistant_message_sse_response("done")?,
    ])
    .await;

    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_extra_config(
            "[validation.project_command]\ncommand = [\"/bin/sh\", \"-c\", \"printf validation-pass\"]\ntimeout_ms = 30000\n",
        )
        .write(codex_home.path())?;

    let workspace = TempDir::new()?;
    // Canonicalize so the notification `cwd` matches what the test expects on
    // platforms where the temp root is a symlink.
    let workspace_path = std::fs::canonicalize(workspace.path())?;
    init_git_repo(&workspace_path)?;

    // Auto-env would move the thread off the prepared repository.
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    let initialized = mcp
        .initialize_with_capabilities(
            ClientInfo {
                name: "codex-app-server-tests".to_string(),
                title: None,
                version: "0.1.0".to_string(),
            },
            Some(InitializeCapabilities {
                experimental_api: false,
                ..Default::default()
            }),
        )
        .await?;
    let JSONRPCMessage::Response(_) = initialized else {
        anyhow::bail!("expected initialize response, got {initialized:?}");
    };

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

    let TurnStartResponse { turn } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread_id.clone(),
                client_user_message_id: None,
                input: vec![UserInput::Text {
                    text: "make the change".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let notification = loop {
        let message = timeout(DEFAULT_READ_TIMEOUT, mcp.read_next_message()).await??;
        match message {
            JSONRPCMessage::Notification(notification)
                if notification.method == "validation/completed" =>
            {
                break notification;
            }
            JSONRPCMessage::Notification(notification)
                if notification.method == "turn/completed" =>
            {
                anyhow::bail!("turn/completed arrived before validation/completed");
            }
            _ => {}
        }
    };
    let completed: ProjectValidationCompletedNotification = serde_json::from_value(
        notification
            .params
            .context("validation/completed must carry params")?,
    )?;

    assert_eq!(
        completed,
        ProjectValidationCompletedNotification {
            thread_id: thread_id.clone(),
            turn_id: turn.id.clone(),
            // The duration and item id are assigned at run time.
            duration_ms: completed.duration_ms,
            item_id: completed.item_id.clone(),
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf validation-pass".to_string(),
            ],
            command_truncated: false,
            cwd: Some(workspace_path.clone().try_into()?),
            status: ProjectValidationStatus::Passed,
            skip_reason: None,
            changed_file_count: None,
            exit_code: Some(0),
            output: "validation-pass".to_string(),
            output_truncated: false,
        }
    );
    assert!(
        !completed.turn_id.is_empty(),
        "validation must be attributed to the turn that triggered it"
    );

    let turn_completed: TurnCompletedNotification = loop {
        let message = timeout(DEFAULT_READ_TIMEOUT, mcp.read_next_message()).await??;
        let JSONRPCMessage::Notification(notification) = message else {
            continue;
        };
        if notification.method != "turn/completed" {
            continue;
        }
        break serde_json::from_value(
            notification
                .params
                .context("turn/completed must carry params")?,
        )?;
    };
    assert_eq!(turn_completed.thread_id, thread_id);
    assert_eq!(turn_completed.turn.id, turn.id);

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn automatic_validation_config_write_applies_to_the_next_turn() -> Result<()> {
    let server =
        create_mock_responses_server_sequence(vec![create_final_assistant_message_sse_response(
            "done",
        )?])
        .await;

    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(codex_home.path())?;

    let workspace = TempDir::new()?;
    let workspace_path = std::fs::canonicalize(workspace.path())?;
    init_git_repo(&workspace_path)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
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

    let request_id = mcp
        .send_config_batch_write_request(ConfigBatchWriteParams {
            edits: vec![ConfigEdit {
                key_path: "validation.groups.functional".to_string(),
                value: json!(true),
                merge_strategy: MergeStrategy::Replace,
            }],
            file_path: None,
            expected_version: None,
            reload_user_config: true,
        })
        .await?;
    let write_response: ConfigWriteResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;
    assert_eq!(write_response.status, WriteStatus::Ok);

    let ThreadReadResponse { thread } = mcp
        .request(|request_id| ClientRequest::ThreadRead {
            request_id,
            params: ThreadReadParams {
                thread_id: thread.id.clone(),
                include_turns: false,
            },
        })
        .await?;
    assert_eq!(
        thread.extra.map(|extra| extra.automatic_validation_enabled),
        Some(true)
    );

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![UserInput::Text {
                    text: "respond without changing files".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("validation/completed"),
    )
    .await??;
    let completed: ProjectValidationCompletedNotification = serde_json::from_value(
        notification
            .params
            .context("validation/completed must carry params")?,
    )?;
    assert_eq!(completed.status, ProjectValidationStatus::Skipped);
    assert_eq!(
        completed.skip_reason,
        Some(ProjectValidationSkipReason::UnchangedFingerprint)
    );

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn automatic_validation_config_write_respects_project_override() -> Result<()> {
    let server =
        create_mock_responses_server_sequence(vec![create_final_assistant_message_sse_response(
            "done",
        )?])
        .await;

    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(codex_home.path())?;

    let workspace = TempDir::new()?;
    let workspace_path = std::fs::canonicalize(workspace.path())?;
    init_git_repo(&workspace_path)?;
    let project_config_dir = workspace_path.join(".codex");
    std::fs::create_dir_all(&project_config_dir)?;
    std::fs::write(
        project_config_dir.join("config.toml"),
        "[validation.groups]\nfunctional = false\n",
    )?;
    set_project_trust_level(codex_home.path(), &workspace_path, TrustLevel::Trusted)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
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

    let request_id = mcp
        .send_config_batch_write_request(ConfigBatchWriteParams {
            edits: vec![ConfigEdit {
                key_path: "validation.groups.functional".to_string(),
                value: json!(true),
                merge_strategy: MergeStrategy::Replace,
            }],
            file_path: None,
            expected_version: None,
            reload_user_config: true,
        })
        .await?;
    let write_response: ConfigWriteResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;
    assert_eq!(write_response.status, WriteStatus::Ok);

    let ThreadReadResponse { thread } = mcp
        .request(|request_id| ClientRequest::ThreadRead {
            request_id,
            params: ThreadReadParams {
                thread_id: thread.id.clone(),
                include_turns: false,
            },
        })
        .await?;
    assert_eq!(
        thread.extra.map(|extra| extra.automatic_validation_enabled),
        Some(false)
    );

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![UserInput::Text {
                    text: "respond without changing files".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("validation/completed"),
    )
    .await??;
    let completed: ProjectValidationCompletedNotification = serde_json::from_value(
        notification
            .params
            .context("validation/completed must carry params")?,
    )?;
    assert_eq!(completed.status, ProjectValidationStatus::Skipped);
    assert_eq!(
        completed.skip_reason,
        Some(ProjectValidationSkipReason::ValidationDisabled)
    );

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn automatic_validation_config_write_respects_thread_override() -> Result<()> {
    let server =
        create_mock_responses_server_sequence(vec![create_final_assistant_message_sse_response(
            "done",
        )?])
        .await;

    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(codex_home.path())?;

    let workspace = TempDir::new()?;
    let workspace_path = std::fs::canonicalize(workspace.path())?;
    init_git_repo(&workspace_path)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .request(|request_id| ClientRequest::ThreadStart {
            request_id,
            params: ThreadStartParams {
                model: Some("mock-model".to_string()),
                cwd: Some(workspace_path.to_string_lossy().into_owned()),
                config: Some(HashMap::from([(
                    "validation.groups.functional".to_string(),
                    json!(false),
                )])),
                ..Default::default()
            },
        })
        .await?;

    let request_id = mcp
        .send_config_batch_write_request(ConfigBatchWriteParams {
            edits: vec![ConfigEdit {
                key_path: "validation.groups.functional".to_string(),
                value: json!(true),
                merge_strategy: MergeStrategy::Replace,
            }],
            file_path: None,
            expected_version: None,
            reload_user_config: true,
        })
        .await?;
    let write_response: ConfigWriteResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;
    assert_eq!(write_response.status, WriteStatus::Ok);

    let ThreadReadResponse { thread } = mcp
        .request(|request_id| ClientRequest::ThreadRead {
            request_id,
            params: ThreadReadParams {
                thread_id: thread.id.clone(),
                include_turns: false,
            },
        })
        .await?;
    assert_eq!(
        thread.extra.map(|extra| extra.automatic_validation_enabled),
        Some(false)
    );

    let _: TurnStartResponse = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                client_user_message_id: None,
                input: vec![UserInput::Text {
                    text: "respond without changing files".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;

    let notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("validation/completed"),
    )
    .await??;
    let completed: ProjectValidationCompletedNotification = serde_json::from_value(
        notification
            .params
            .context("validation/completed must carry params")?,
    )?;
    assert_eq!(completed.status, ProjectValidationStatus::Skipped);
    assert_eq!(
        completed.skip_reason,
        Some(ProjectValidationSkipReason::ValidationDisabled)
    );

    Ok(())
}
