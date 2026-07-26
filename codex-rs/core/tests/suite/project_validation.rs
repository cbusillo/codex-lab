#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use codex_config::CargoValidationProviderConfig;
use codex_config::MAX_PROJECT_VALIDATION_TIMEOUT_MS;
use codex_config::ProjectValidationCommand;
use codex_config::ShellcheckValidationProviderConfig;
use codex_core::CodexThread;
use codex_core::StartThreadOptions;
use codex_exec_server::CreateDirectoryOptions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ProjectValidationCompletedEvent;
use codex_protocol::protocol::ProjectValidationSkipReason;
use codex_protocol::protocol::ProjectValidationStatus;
use codex_protocol::protocol::SKILLS_INSTRUCTIONS_OPEN_TAG;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::TurnEnvironmentSelections;
use codex_protocol::user_input::UserInput;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use codex_utils_string::approx_token_count;
use core_test_support::responses;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_apply_patch_custom_tool_call;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::ev_shell_command_call;
use core_test_support::responses::sse_completed;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::StreamingSseServer;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use core_test_support::wait_for_event;
use serde_json::Value;
use tempfile::tempdir;
use tokio::sync::oneshot;
use tokio::time::sleep;
use tokio::time::timeout;
use tracing_test::traced_test;
use wiremock::MockServer;

async fn run_validation_turn(command: ProjectValidationCommand) -> Result<Vec<EventMsg>> {
    run_validation_turn_with_source(command, /*session_source*/ None).await
}

async fn run_validation_turn_with_source(
    command: ProjectValidationCommand,
    session_source: Option<SessionSource>,
) -> Result<Vec<EventMsg>> {
    Ok(run_validation_turn_with_source_and_response_count(
        command,
        session_source,
        /*response_count*/ 1,
    )
    .await?
    .0)
}

async fn run_validation_correction_turn(
    command: ProjectValidationCommand,
) -> Result<(Vec<EventMsg>, Vec<ResponsesRequest>)> {
    run_validation_turn_with_source_and_response_count(
        command, /*session_source*/ None, /*response_count*/ 2,
    )
    .await
}

async fn run_validation_turn_with_source_and_response_count(
    command: ProjectValidationCommand,
    session_source: Option<SessionSource>,
    response_count: usize,
) -> Result<(Vec<EventMsg>, Vec<ResponsesRequest>)> {
    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        (0..response_count)
            .map(|index| sse_completed(&format!("resp-{}", index + 1)))
            .collect(),
    )
    .await;
    let test = build_validation_codex(&server, command).await?;
    let codex = if let Some(session_source) = session_source {
        test.thread_manager
            .start_thread(StartThreadOptions {
                session_source: Some(session_source),
                environments: Some(Vec::new()),
                ..StartThreadOptions::new(test.config.clone())
            })
            .await?
            .thread
    } else {
        test.codex.clone()
    };

    submit_user_input(&codex, &test, "finish the work").await?;
    let events = collect_events_until_terminal(&codex).await?;
    Ok((events, response_mock.requests()))
}

fn validation_events(events: &[EventMsg]) -> Vec<&ProjectValidationCompletedEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            EventMsg::ProjectValidationCompleted(event) => Some(event),
            _ => None,
        })
        .collect()
}

fn assert_single_validation_skip(
    events: &[EventMsg],
    reason: ProjectValidationSkipReason,
) -> &ProjectValidationCompletedEvent {
    let validation_events = validation_events(events);
    assert_eq!(validation_events.len(), 1);
    let event = validation_events[0];
    assert_eq!(event.status, ProjectValidationStatus::Skipped);
    assert_eq!(event.skip_reason, Some(reason));
    event
}

fn shell_command(script: &str, timeout_ms: u64) -> ProjectValidationCommand {
    shell_command_with_args(script, &[], timeout_ms)
}

fn shell_command_with_args(
    script: &str,
    args: &[&str],
    timeout_ms: u64,
) -> ProjectValidationCommand {
    let mut command = vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
    command.extend(args.iter().map(ToString::to_string));
    ProjectValidationCommand {
        command,
        timeout_ms,
    }
}

fn streaming_chunk(event: Value) -> StreamingSseChunk {
    StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![event]),
    }
}

fn gated_streaming_chunk(gate: oneshot::Receiver<()>, events: Vec<Value>) -> StreamingSseChunk {
    StreamingSseChunk {
        gate: Some(gate),
        body: responses::sse(events),
    }
}

fn response_completed_chunks(response_id: &str) -> Vec<StreamingSseChunk> {
    vec![
        streaming_chunk(ev_response_created(response_id)),
        streaming_chunk(ev_completed(response_id)),
    ]
}

async fn build_streaming_validation_codex(
    server: &StreamingSseServer,
    command: ProjectValidationCommand,
) -> Result<TestCodex> {
    let mut builder = test_codex().with_config(move |config| {
        config.validation.project_command = Some(command);
    });
    builder.build_with_streaming_server(server).await
}

async fn build_validation_codex(
    server: &MockServer,
    command: ProjectValidationCommand,
) -> Result<TestCodex> {
    let mut builder = test_codex().with_config(move |config| {
        config.validation.project_command = Some(command);
    });
    builder.build(server).await
}

async fn build_shellcheck_provider_codex(
    server: &MockServer,
    cwd: &Path,
    provider_command: Vec<String>,
    project_command: Option<ProjectValidationCommand>,
) -> Result<TestCodex> {
    let cwd = AbsolutePathBuf::try_from(cwd.to_path_buf())?;
    let mut builder = test_codex().with_config(move |config| {
        config.cwd = cwd.clone();
        config.validation.groups.functional = true;
        config.validation.providers.shellcheck = ShellcheckValidationProviderConfig {
            command: provider_command.clone(),
            timeout_ms: 5_000,
            ..ShellcheckValidationProviderConfig::default()
        };
        config.validation.project_command = project_command;
    });
    builder.build(server).await
}

async fn build_cargo_provider_codex(
    server: &MockServer,
    cwd: &Path,
    provider_command: String,
    timeout_ms: u64,
) -> Result<TestCodex> {
    let cwd = AbsolutePathBuf::try_from(cwd.to_path_buf())?;
    let mut builder = test_codex().with_config(move |config| {
        config.cwd = cwd.clone();
        config.validation.groups.functional = true;
        config.validation.providers.cargo = CargoValidationProviderConfig {
            command: vec![provider_command.clone()],
            timeout_ms,
            ..CargoValidationProviderConfig::default()
        };
        config.validation.providers.shellcheck.enabled = false;
    });
    builder.build(server).await
}

fn write_executable(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents)?;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

async fn submit_user_input(codex: &CodexThread, test: &TestCodex, text: &str) -> Result<()> {
    submit_user_input_at_cwd(codex, test, text, test.config.cwd.clone()).await
}

async fn steer_user_input(codex: &CodexThread, text: &str) -> Result<()> {
    codex
        .steer_input(
            vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            Default::default(),
            /*expected_turn_id*/ None,
            /*client_user_message_id*/ None,
            /*responsesapi_client_metadata*/ None,
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to steer input: {error:?}"))?;
    Ok(())
}

#[derive(Clone, Copy)]
enum TestTurnEnvironmentOverride {
    Inherit,
    NoEnvironments,
}

async fn submit_user_input_at_cwd(
    codex: &CodexThread,
    test: &TestCodex,
    text: &str,
    cwd: AbsolutePathBuf,
) -> Result<()> {
    submit_user_input_with_environment_override(
        codex,
        test,
        text,
        cwd,
        TestTurnEnvironmentOverride::Inherit,
    )
    .await
}

async fn submit_user_input_without_environments(
    codex: &CodexThread,
    test: &TestCodex,
    text: &str,
) -> Result<()> {
    submit_user_input_with_environment_override(
        codex,
        test,
        text,
        test.config.cwd.clone(),
        TestTurnEnvironmentOverride::NoEnvironments,
    )
    .await
}

async fn submit_user_input_with_environment_override(
    codex: &CodexThread,
    test: &TestCodex,
    text: &str,
    cwd: AbsolutePathBuf,
    environment_override: TestTurnEnvironmentOverride,
) -> Result<()> {
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, cwd.as_path());
    let session_model = test.session_configured.model.clone();
    let environments = match environment_override {
        TestTurnEnvironmentOverride::Inherit => None,
        TestTurnEnvironmentOverride::NoEnvironments => {
            Some(TurnEnvironmentSelections::new(cwd.clone(), Vec::new()))
        }
    };
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: codex_protocol::protocol::ThreadSettingsOverrides {
                environments,
                approval_policy: Some(AskForApproval::Never),
                sandbox_policy: Some(sandbox_policy),
                permission_profile,
                collaboration_mode: Some(codex_protocol::config_types::CollaborationMode {
                    mode: codex_protocol::config_types::ModeKind::Default,
                    settings: codex_protocol::config_types::Settings {
                        model: session_model,
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            },
        })
        .await?;
    Ok(())
}

async fn collect_events_until_terminal(codex: &CodexThread) -> Result<Vec<EventMsg>> {
    let mut events = Vec::new();
    loop {
        let event = timeout(Duration::from_secs(10), codex.next_event())
            .await
            .context("timed out waiting for terminal turn event")??;
        let terminal = matches!(
            event.msg,
            EventMsg::TurnAborted(_) | EventMsg::TurnComplete(_)
        );
        events.push(event.msg);
        if terminal {
            return Ok(events);
        }
    }
}

async fn start_root_thread_with_validation_command(
    test: &TestCodex,
    command: ProjectValidationCommand,
) -> Result<Arc<CodexThread>> {
    let mut config = test.config.clone();
    config.validation.project_command = Some(command);
    Ok(test
        .thread_manager
        .start_thread(StartThreadOptions::new(config))
        .await?
        .thread)
}

async fn start_root_thread_with_cwd_and_validation_command(
    test: &TestCodex,
    cwd: &Path,
    command: ProjectValidationCommand,
) -> Result<Arc<CodexThread>> {
    let mut config = test.config.clone();
    config.cwd = AbsolutePathBuf::from_absolute_path(cwd).context("test cwd should be absolute")?;
    config.validation.project_command = Some(command);
    Ok(test
        .thread_manager
        .start_thread(StartThreadOptions::new(config))
        .await?
        .thread)
}

async fn start_root_thread_with_cwd(test: &TestCodex, cwd: &Path) -> Result<Arc<CodexThread>> {
    let mut config = test.config.clone();
    config.cwd = AbsolutePathBuf::from_absolute_path(cwd).context("test cwd should be absolute")?;
    Ok(test
        .thread_manager
        .start_thread(StartThreadOptions::new(config))
        .await?
        .thread)
}

async fn wait_for_path(path: &Path) -> Result<()> {
    timeout(Duration::from_secs(5), async {
        while !path.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .with_context(|| format!("timed out waiting for {}", path.display()))?;
    Ok(())
}

async fn wait_for_repo_lease_contention(repo_root: &Path) -> Result<()> {
    let canonical_repo_root = dunce::canonicalize(repo_root)?;
    let repo_field = format!("repo_root={}", canonical_repo_root.display());
    timeout(Duration::from_secs(5), async {
        loop {
            let found = {
                let logs = tracing_test::internal::global_buf()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                String::from_utf8_lossy(&logs).lines().any(|line| {
                    line.contains("project validation waiting for repository lease")
                        && line.contains(&repo_field)
                })
            };
            if found {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("session did not wait for the repository lease")?;
    Ok(())
}

fn request_message_input_texts(body: &Value, role: &str) -> Vec<String> {
    body.get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter(|item| item.get("role").and_then(Value::as_str) == Some(role))
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|content| content.get("type").and_then(Value::as_str) == Some("input_text"))
        .filter_map(|content| {
            content
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn run_git(path: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git").args(args).current_dir(path).output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn run_git_stdout(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(path).output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn init_git_repo(path: &Path) -> Result<()> {
    for args in [
        &["init", "--quiet"][..],
        &["config", "user.email", "project-validation@example.com"][..],
        &["config", "user.name", "Project Validation"][..],
        &["add", "--all"][..],
        &["commit", "--quiet", "--allow-empty", "-m", "baseline"][..],
    ] {
        run_git(path, args)?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[traced_test]
async fn project_validation_serializes_concurrent_root_sessions() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let command = shell_command_with_args(
        "count=0; if [ -f \"$1\" ]; then count=$(cat \"$1\"); fi; count=$((count + 1)); printf '%s' \"$count\" > \"$1\"; if ! mkdir \"$2\"; then touch \"$3\"; fi; if [ \"$count\" -eq 1 ]; then touch \"$4\"; while [ ! -f \"$5\" ]; do sleep 0.01; done; else touch \"$6\"; fi; rmdir \"$2\"; printf 'validation-pass-%s' \"$count\"",
        &[
            "project-validation",
            "validation-count",
            "validation-active",
            "validation-overlap",
            "first-validation-started",
            "first-validation-release",
            "second-validation-started",
        ],
        /*timeout_ms*/ 10_000,
    );
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            streaming_chunk(ev_response_created("resp-a1")),
            streaming_chunk(ev_shell_command_call("shell-a1", "true")),
            streaming_chunk(ev_completed("resp-a1")),
        ],
        vec![
            streaming_chunk(ev_response_created("resp-a2")),
            streaming_chunk(ev_assistant_message("msg-a1", "first complete")),
            streaming_chunk(ev_completed("resp-a2")),
        ],
        vec![
            streaming_chunk(ev_response_created("resp-b1")),
            streaming_chunk(ev_shell_command_call("shell-b1", "true")),
            streaming_chunk(ev_completed("resp-b1")),
        ],
        vec![
            streaming_chunk(ev_response_created("resp-b2")),
            streaming_chunk(ev_assistant_message("msg-b1", "second complete")),
            streaming_chunk(ev_completed("resp-b2")),
        ],
    ])
    .await;
    let test = build_streaming_validation_codex(&server, command.clone()).await?;
    init_git_repo(test.cwd_path())?;
    let first_started = test.workspace_path("first-validation-started");
    let first_release = test.workspace_path("first-validation-release");
    let second_started = test.workspace_path("second-validation-started");
    let overlap = test.workspace_path("validation-overlap");
    let count = test.workspace_path("validation-count");

    submit_user_input(&test.codex, &test, "run the first change").await?;
    wait_for_path(&first_started).await?;

    let second = start_root_thread_with_validation_command(&test, command).await?;
    submit_user_input(&second, &test, "run the second change").await?;
    server.wait_for_request_count(/*count*/ 4).await;
    wait_for_repo_lease_contention(test.cwd_path()).await?;
    assert!(!second_started.exists());
    assert!(!overlap.exists());

    std::fs::write(&first_release, "release")?;
    let first_events = collect_events_until_terminal(&test.codex).await?;
    wait_for_path(&second_started).await?;
    let second_events = collect_events_until_terminal(&second).await?;

    let first_validation = validation_events(&first_events);
    assert_eq!(first_validation.len(), 1);
    assert_eq!(first_validation[0].status, ProjectValidationStatus::Passed);
    assert_eq!(first_validation[0].output, "validation-pass-1");
    let second_validation = validation_events(&second_events);
    assert_eq!(second_validation.len(), 1);
    assert_eq!(second_validation[0].status, ProjectValidationStatus::Passed);
    assert_eq!(second_validation[0].output, "validation-pass-2");
    assert_eq!(std::fs::read_to_string(count)?, "2");
    assert!(!overlap.exists());
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[traced_test]
async fn project_validation_serializes_linked_worktrees_from_same_repository() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let gate_dir = tempdir()?;
    let count = gate_dir.path().join("validation-count");
    let active = gate_dir.path().join("validation-active");
    let overlap = gate_dir.path().join("validation-overlap");
    let first_started = gate_dir.path().join("first-validation-started");
    let first_release = gate_dir.path().join("first-validation-release");
    let second_started = gate_dir.path().join("second-validation-started");
    let command = shell_command_with_args(
        "count=0; if [ -f \"$1\" ]; then count=$(cat \"$1\"); fi; count=$((count + 1)); printf '%s' \"$count\" > \"$1\"; owner=0; if mkdir \"$2\"; then owner=1; else touch \"$3\"; fi; if [ \"$count\" -eq 1 ]; then touch \"$4\"; while [ ! -f \"$5\" ]; do sleep 0.01; done; else touch \"$6\"; fi; if [ \"$owner\" -eq 1 ]; then rmdir \"$2\"; fi; printf 'validation-pass-%s' \"$count\"",
        &[
            "project-validation",
            count.to_str().context("count path should be UTF-8")?,
            active.to_str().context("active path should be UTF-8")?,
            overlap.to_str().context("overlap path should be UTF-8")?,
            first_started
                .to_str()
                .context("first started path should be UTF-8")?,
            first_release
                .to_str()
                .context("first release path should be UTF-8")?,
            second_started
                .to_str()
                .context("second started path should be UTF-8")?,
        ],
        /*timeout_ms*/ 10_000,
    );
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            streaming_chunk(ev_response_created("resp-a1")),
            streaming_chunk(ev_shell_command_call("shell-a1", "true")),
            streaming_chunk(ev_completed("resp-a1")),
        ],
        vec![
            streaming_chunk(ev_response_created("resp-a2")),
            streaming_chunk(ev_assistant_message("msg-a1", "first complete")),
            streaming_chunk(ev_completed("resp-a2")),
        ],
        vec![
            streaming_chunk(ev_response_created("resp-b1")),
            streaming_chunk(ev_shell_command_call("shell-b1", "true")),
            streaming_chunk(ev_completed("resp-b1")),
        ],
        vec![
            streaming_chunk(ev_response_created("resp-b2")),
            streaming_chunk(ev_assistant_message("msg-b1", "second complete")),
            streaming_chunk(ev_completed("resp-b2")),
        ],
    ])
    .await;
    let test = build_streaming_validation_codex(&server, command.clone()).await?;
    init_git_repo(test.cwd_path())?;
    let linked_worktree_parent = tempdir()?;
    let linked_worktree = linked_worktree_parent.path().join("linked-worktree");
    let linked_worktree_arg = linked_worktree
        .to_str()
        .context("linked worktree path should be UTF-8")?;
    run_git(
        test.cwd_path(),
        &["worktree", "add", "--detach", linked_worktree_arg, "HEAD"],
    )?;

    submit_user_input(&test.codex, &test, "run the first change").await?;
    wait_for_path(&first_started).await?;

    let second =
        start_root_thread_with_cwd_and_validation_command(&test, &linked_worktree, command).await?;
    let linked_cwd = AbsolutePathBuf::from_absolute_path(&linked_worktree)
        .context("linked worktree path should be absolute")?;
    submit_user_input_at_cwd(&second, &test, "run the second change", linked_cwd).await?;
    server.wait_for_request_count(/*count*/ 4).await;
    let contention = wait_for_repo_lease_contention(test.cwd_path()).await;
    if contention.is_err() {
        std::fs::write(&first_release, "release")?;
        let _ = collect_events_until_terminal(&test.codex).await;
        let _ = collect_events_until_terminal(&second).await;
        server.shutdown().await;
        return contention;
    }
    assert!(!second_started.exists());
    assert!(!overlap.exists());

    std::fs::write(&first_release, "release")?;
    let first_events = collect_events_until_terminal(&test.codex).await?;
    wait_for_path(&second_started).await?;
    let second_events = collect_events_until_terminal(&second).await?;

    let first_validation = validation_events(&first_events);
    assert_eq!(first_validation.len(), 1);
    assert_eq!(first_validation[0].status, ProjectValidationStatus::Passed);
    assert_eq!(first_validation[0].output, "validation-pass-1");
    let second_validation = validation_events(&second_events);
    assert_eq!(second_validation.len(), 1);
    assert_eq!(second_validation[0].status, ProjectValidationStatus::Passed);
    assert_eq!(second_validation[0].output, "validation-pass-2");
    assert_eq!(std::fs::read_to_string(count)?, "2");
    assert!(!overlap.exists());
    run_git(
        test.cwd_path(),
        &["worktree", "remove", "--force", linked_worktree_arg],
    )?;
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_validation_unchanged_turn_skips_without_waiting_for_repo_lease() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let command = shell_command(
        "touch owner-validation-started; while :; do sleep 0.05; done",
        /*timeout_ms*/ 10_000,
    );
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            streaming_chunk(ev_response_created("resp-a1")),
            streaming_chunk(ev_shell_command_call("shell-a1", "true")),
            streaming_chunk(ev_completed("resp-a1")),
        ],
        response_completed_chunks("resp-a2"),
        response_completed_chunks("resp-b1"),
    ])
    .await;
    let test = build_streaming_validation_codex(&server, command.clone()).await?;
    init_git_repo(test.cwd_path())?;
    let owner_started = test.workspace_path("owner-validation-started");

    submit_user_input(&test.codex, &test, "run the owner change").await?;
    wait_for_path(&owner_started).await?;

    let unchanged = start_root_thread_with_validation_command(&test, command).await?;
    submit_user_input(&unchanged, &test, "inspect without changes").await?;
    let unchanged_events = collect_events_until_terminal(&unchanged).await?;
    let unchanged_validation = validation_events(&unchanged_events);
    assert_eq!(unchanged_validation.len(), 1);
    assert_eq!(
        unchanged_validation[0].status,
        ProjectValidationStatus::Skipped
    );
    assert_eq!(
        unchanged_validation[0].skip_reason,
        Some(ProjectValidationSkipReason::UnchangedFingerprint)
    );
    assert!(owner_started.exists());

    test.codex.submit(Op::Interrupt).await?;
    let owner_events = collect_events_until_terminal(&test.codex).await?;
    assert!(
        owner_events
            .iter()
            .any(|event| matches!(event, EventMsg::TurnAborted(_)))
    );
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[traced_test]
async fn project_validation_waiter_rechecks_unchanged_worktree_after_lease() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let gate_dir = tempdir()?;
    let owner_started = gate_dir.path().join("owner-validation-started");
    let owner_release = gate_dir.path().join("owner-validation-release");
    let waiter_started = gate_dir.path().join("waiter-validation-started");
    let owner_command = shell_command_with_args(
        "touch \"$1\"; while [ ! -f \"$2\" ]; do sleep 0.01; done; printf owner-pass",
        &[
            "project-validation",
            owner_started
                .to_str()
                .context("owner started path should be UTF-8")?,
            owner_release
                .to_str()
                .context("owner release path should be UTF-8")?,
        ],
        /*timeout_ms*/ 10_000,
    );
    let waiter_command = shell_command_with_args(
        "touch \"$1\"; printf waiter-pass",
        &[
            "project-validation",
            waiter_started
                .to_str()
                .context("waiter started path should be UTF-8")?,
        ],
        /*timeout_ms*/ 10_000,
    );
    let (waiter_gate_tx, waiter_gate_rx) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            streaming_chunk(ev_response_created("resp-b1")),
            gated_streaming_chunk(
                waiter_gate_rx,
                vec![
                    ev_assistant_message("msg-b1", "inspection complete"),
                    ev_completed("resp-b1"),
                ],
            ),
        ],
        vec![
            streaming_chunk(ev_response_created("resp-a1")),
            streaming_chunk(ev_shell_command_call("shell-a1", "true")),
            streaming_chunk(ev_completed("resp-a1")),
        ],
        response_completed_chunks("resp-a2"),
    ])
    .await;
    let test = build_streaming_validation_codex(&server, waiter_command).await?;
    init_git_repo(test.cwd_path())?;
    let baseline_head = run_git_stdout(test.cwd_path(), &["rev-parse", "HEAD"])?;

    submit_user_input(&test.codex, &test, "inspect the current tree").await?;
    server.wait_for_request_count(/*count*/ 1).await;

    let owner = start_root_thread_with_validation_command(&test, owner_command).await?;
    submit_user_input(&owner, &test, "run the owner change").await?;
    wait_for_path(&owner_started).await?;
    run_git(
        test.cwd_path(),
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "temporary state",
        ],
    )?;

    let _ = waiter_gate_tx.send(());
    wait_for_repo_lease_contention(test.cwd_path()).await?;

    run_git(test.cwd_path(), &["reset", "--hard", &baseline_head])?;
    std::fs::write(&owner_release, "release")?;
    let owner_events = collect_events_until_terminal(&owner).await?;
    let waiter_events = collect_events_until_terminal(&test.codex).await?;

    let owner_validation = validation_events(&owner_events);
    assert_eq!(owner_validation.len(), 1);
    assert_eq!(owner_validation[0].status, ProjectValidationStatus::Passed);
    assert_eq!(owner_validation[0].output, "owner-pass");
    let waiter_validation = validation_events(&waiter_events);
    assert_eq!(waiter_validation.len(), 1);
    assert_eq!(
        waiter_validation[0].status,
        ProjectValidationStatus::Skipped
    );
    assert_eq!(
        waiter_validation[0].skip_reason,
        Some(ProjectValidationSkipReason::UnchangedFingerprint)
    );
    assert!(!waiter_started.exists());
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_validation_different_repositories_run_independently() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let owner_command = shell_command(
        "touch owner-validation-started; while :; do sleep 0.05; done",
        /*timeout_ms*/ 10_000,
    );
    let independent_command = shell_command(
        "touch independent-validation-started; printf independent-pass",
        /*timeout_ms*/ 10_000,
    );
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            streaming_chunk(ev_response_created("resp-a1")),
            streaming_chunk(ev_shell_command_call("shell-a1", "true")),
            streaming_chunk(ev_completed("resp-a1")),
        ],
        response_completed_chunks("resp-a2"),
        vec![
            streaming_chunk(ev_response_created("resp-b1")),
            streaming_chunk(ev_shell_command_call("shell-b1", "true")),
            streaming_chunk(ev_completed("resp-b1")),
        ],
        response_completed_chunks("resp-b2"),
    ])
    .await;
    let test = build_streaming_validation_codex(&server, owner_command).await?;
    init_git_repo(test.cwd_path())?;
    let owner_started = test.workspace_path("owner-validation-started");
    let independent_repo = tempdir()?;
    init_git_repo(independent_repo.path())?;
    let independent_cwd = AbsolutePathBuf::from_absolute_path(independent_repo.path())
        .context("independent repo should be absolute")?;

    submit_user_input(&test.codex, &test, "run the owner change").await?;
    wait_for_path(&owner_started).await?;

    let independent = start_root_thread_with_cwd_and_validation_command(
        &test,
        independent_repo.path(),
        independent_command,
    )
    .await?;
    submit_user_input_at_cwd(
        &independent,
        &test,
        "run the independent change",
        independent_cwd,
    )
    .await?;
    let independent_events = collect_events_until_terminal(&independent).await?;
    let independent_validation = validation_events(&independent_events);
    assert_eq!(independent_validation.len(), 1);
    assert_eq!(
        independent_validation[0].status,
        ProjectValidationStatus::Passed
    );
    assert_eq!(independent_validation[0].output, "independent-pass");
    assert!(
        independent_repo
            .path()
            .join("independent-validation-started")
            .exists()
    );
    assert!(owner_started.exists());

    test.codex.submit(Op::Interrupt).await?;
    let owner_events = collect_events_until_terminal(&test.codex).await?;
    assert!(
        owner_events
            .iter()
            .any(|event| matches!(event, EventMsg::TurnAborted(_)))
    );
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[traced_test]
async fn project_validation_owner_cancellation_releases_repo_lease() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let owner_command = shell_command(
        "touch owner-validation-started; while :; do sleep 0.05; done",
        /*timeout_ms*/ 10_000,
    );
    let waiter_command = shell_command(
        "touch waiter-validation-started; printf waiter-pass",
        /*timeout_ms*/ 10_000,
    );
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            streaming_chunk(ev_response_created("resp-a1")),
            streaming_chunk(ev_shell_command_call("shell-a1", "true")),
            streaming_chunk(ev_completed("resp-a1")),
        ],
        response_completed_chunks("resp-a2"),
        vec![
            streaming_chunk(ev_response_created("resp-b1")),
            streaming_chunk(ev_shell_command_call("shell-b1", "true")),
            streaming_chunk(ev_completed("resp-b1")),
        ],
        response_completed_chunks("resp-b2"),
    ])
    .await;
    let test = build_streaming_validation_codex(&server, owner_command).await?;
    init_git_repo(test.cwd_path())?;
    let owner_started = test.workspace_path("owner-validation-started");
    let waiter_started = test.workspace_path("waiter-validation-started");

    submit_user_input(&test.codex, &test, "run the owner change").await?;
    wait_for_path(&owner_started).await?;

    let waiter = start_root_thread_with_validation_command(&test, waiter_command).await?;
    submit_user_input(&waiter, &test, "run the waiter change").await?;
    server.wait_for_request_count(/*count*/ 4).await;
    wait_for_repo_lease_contention(test.cwd_path()).await?;
    assert!(!waiter_started.exists());

    test.codex.submit(Op::Interrupt).await?;
    let owner_events = collect_events_until_terminal(&test.codex).await?;
    let owner_validation = validation_events(&owner_events);
    assert_eq!(owner_validation.len(), 1);
    assert_eq!(
        owner_validation[0].status,
        ProjectValidationStatus::Cancelled
    );
    assert!(
        owner_events
            .iter()
            .any(|event| matches!(event, EventMsg::TurnAborted(_)))
    );

    wait_for_path(&waiter_started).await?;
    let waiter_events = collect_events_until_terminal(&waiter).await?;
    let waiter_validation = validation_events(&waiter_events);
    assert_eq!(waiter_validation.len(), 1);
    assert_eq!(waiter_validation[0].status, ProjectValidationStatus::Passed);
    assert_eq!(waiter_validation[0].output, "waiter-pass");
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_validation_configuration_error_does_not_wait_for_repo_lease() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let owner_command = shell_command(
        "touch owner-validation-started; while :; do sleep 0.05; done",
        /*timeout_ms*/ 10_000,
    );
    let invalid_command = ProjectValidationCommand {
        command: vec!["missing-project-validation-executable".to_string()],
        timeout_ms: 10_000,
    };
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            streaming_chunk(ev_response_created("resp-a1")),
            streaming_chunk(ev_shell_command_call("shell-a1", "true")),
            streaming_chunk(ev_completed("resp-a1")),
        ],
        response_completed_chunks("resp-a2"),
        response_completed_chunks("resp-b1"),
    ])
    .await;
    let test = build_streaming_validation_codex(&server, owner_command).await?;
    init_git_repo(test.cwd_path())?;
    let owner_started = test.workspace_path("owner-validation-started");

    submit_user_input(&test.codex, &test, "run the owner change").await?;
    wait_for_path(&owner_started).await?;

    let invalid = start_root_thread_with_validation_command(&test, invalid_command).await?;
    submit_user_input(&invalid, &test, "finish without changes").await?;
    let invalid_events = collect_events_until_terminal(&invalid).await?;
    let invalid_validation = validation_events(&invalid_events);
    assert_eq!(invalid_validation.len(), 1);
    assert_eq!(
        invalid_validation[0].status,
        ProjectValidationStatus::ConfigurationError
    );
    assert!(invalid_validation[0].output.contains("not found"));

    test.codex.submit(Op::Interrupt).await?;
    let owner_events = collect_events_until_terminal(&test.codex).await?;
    assert!(
        owner_events
            .iter()
            .any(|event| matches!(event, EventMsg::TurnAborted(_)))
    );
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[traced_test]
async fn project_validation_correction_rerun_reacquires_repo_lease() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let correction_command = shell_command(
        "count=0; if [ -f correction-validation-count ]; then count=$(cat correction-validation-count); fi; count=$((count + 1)); printf '%s' \"$count\" > correction-validation-count; if [ \"$count\" -eq 1 ]; then printf correction-fail >&2; exit 7; fi; touch correction-rerun-started; printf correction-pass",
        /*timeout_ms*/ 10_000,
    );
    let interleaved_command = shell_command(
        "touch interleaved-validation-started; while [ ! -f interleaved-validation-release ]; do sleep 0.01; done; printf interleaved-pass",
        /*timeout_ms*/ 10_000,
    );
    let (correction_gate_tx, correction_gate_rx) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            streaming_chunk(ev_response_created("resp-a1")),
            streaming_chunk(ev_shell_command_call("shell-a1", "true")),
            streaming_chunk(ev_completed("resp-a1")),
        ],
        response_completed_chunks("resp-a2"),
        vec![
            streaming_chunk(ev_response_created("resp-a3")),
            gated_streaming_chunk(
                correction_gate_rx,
                vec![
                    ev_assistant_message("msg-a2", "correction complete"),
                    ev_completed("resp-a3"),
                ],
            ),
        ],
        vec![
            streaming_chunk(ev_response_created("resp-b1")),
            streaming_chunk(ev_shell_command_call("shell-b1", "true")),
            streaming_chunk(ev_completed("resp-b1")),
        ],
        response_completed_chunks("resp-b2"),
    ])
    .await;
    let test = build_streaming_validation_codex(&server, correction_command).await?;
    init_git_repo(test.cwd_path())?;
    let interleaved_started = test.workspace_path("interleaved-validation-started");
    let interleaved_release = test.workspace_path("interleaved-validation-release");
    let correction_rerun_started = test.workspace_path("correction-rerun-started");

    submit_user_input(&test.codex, &test, "run the correction change").await?;
    let first_validation = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::ProjectValidationCompleted(event)
                if event.status == ProjectValidationStatus::ActionableFailure
        )
    })
    .await;
    server.wait_for_request_count(/*count*/ 3).await;

    let interleaved = start_root_thread_with_validation_command(&test, interleaved_command).await?;
    submit_user_input(&interleaved, &test, "run the interleaved change").await?;
    server.wait_for_request_count(/*count*/ 5).await;
    wait_for_path(&interleaved_started).await?;

    let _ = correction_gate_tx.send(());
    wait_for_repo_lease_contention(test.cwd_path()).await?;
    assert!(!correction_rerun_started.exists());

    std::fs::write(&interleaved_release, "release")?;
    let interleaved_events = collect_events_until_terminal(&interleaved).await?;
    wait_for_path(&correction_rerun_started).await?;
    let mut correction_events = vec![first_validation];
    correction_events.extend(collect_events_until_terminal(&test.codex).await?);

    let interleaved_validation = validation_events(&interleaved_events);
    assert_eq!(interleaved_validation.len(), 1);
    assert_eq!(
        interleaved_validation[0].status,
        ProjectValidationStatus::Passed
    );
    assert_eq!(interleaved_validation[0].output, "interleaved-pass");
    let correction_validation = validation_events(&correction_events);
    assert_eq!(correction_validation.len(), 2);
    assert_eq!(
        correction_validation[0].status,
        ProjectValidationStatus::ActionableFailure
    );
    assert_eq!(
        correction_validation[1].status,
        ProjectValidationStatus::Passed
    );
    assert_eq!(correction_validation[1].output, "correction-pass");
    assert_eq!(
        std::fs::read_to_string(test.workspace_path("correction-validation-count"))?,
        "2"
    );
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_skips_when_git_worktree_is_unchanged() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_once(&server, sse_completed("resp-1")).await;
    let test = build_validation_codex(
        &server,
        shell_command("touch project-validation-ran", /*timeout_ms*/ 5_000),
    )
    .await?;
    init_git_repo(test.cwd_path())?;

    submit_user_input(&test.codex, &test, "explain the current implementation").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(
        validation_events[0].status,
        ProjectValidationStatus::Skipped
    );
    assert_eq!(
        validation_events[0].skip_reason,
        Some(ProjectValidationSkipReason::UnchangedFingerprint)
    );
    assert!(!test.workspace_path("project-validation-ran").exists());
    assert_eq!(response_mock.requests().len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_skips_when_preexisting_dirty_worktree_is_unchanged() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_once(&server, sse_completed("resp-1")).await;
    let test = build_validation_codex(
        &server,
        shell_command("touch project-validation-ran", /*timeout_ms*/ 5_000),
    )
    .await?;
    init_git_repo(test.cwd_path())?;
    std::fs::write(test.workspace_path("preexisting-dirty.txt"), "dirty")?;

    submit_user_input(&test.codex, &test, "explain the current implementation").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(
        validation_events[0].status,
        ProjectValidationStatus::Skipped
    );
    assert_eq!(
        validation_events[0].skip_reason,
        Some(ProjectValidationSkipReason::UnchangedFingerprint)
    );
    assert!(!test.workspace_path("project-validation-ran").exists());
    assert_eq!(response_mock.requests().len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_retries_unchanged_skip_after_pending_input() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let (first_response_gate_tx, first_response_gate_rx) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            streaming_chunk(ev_response_created("resp-1")),
            gated_streaming_chunk(
                first_response_gate_rx,
                vec![
                    ev_assistant_message("msg-1", "initial answer"),
                    ev_completed("resp-1"),
                ],
            ),
        ],
        vec![
            streaming_chunk(ev_response_created("resp-2")),
            streaming_chunk(ev_shell_command_call("shell-1", "touch ignored-by-git.txt")),
            streaming_chunk(ev_completed("resp-2")),
        ],
        response_completed_chunks("resp-3"),
    ])
    .await;
    let test = build_streaming_validation_codex(
        &server,
        shell_command("printf validation-pass", /*timeout_ms*/ 5_000),
    )
    .await?;
    std::fs::write(test.workspace_path(".gitignore"), "ignored-by-git.txt\n")?;
    init_git_repo(test.cwd_path())?;

    submit_user_input(&test.codex, &test, "inspect the project").await?;
    server.wait_for_request_count(/*count*/ 1).await;

    let head_path = test.workspace_path(".git/HEAD");
    let saved_head_path = test.workspace_path(".git/HEAD.project-validation-test");
    let head_contents = std::fs::read(&head_path)?;
    std::fs::rename(&head_path, &saved_head_path)?;
    let mkfifo_status = Command::new("mkfifo").arg(&head_path).status()?;
    anyhow::ensure!(mkfifo_status.success(), "mkfifo failed");

    let (capture_started_tx, capture_started_rx) = oneshot::channel();
    let (capture_release_tx, capture_release_rx) = oneshot::channel();
    let capture_task = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut fifo = std::fs::OpenOptions::new().write(true).open(&head_path)?;
        std::fs::remove_file(&head_path)?;
        std::fs::rename(&saved_head_path, &head_path)?;
        let _ = capture_started_tx.send(());
        capture_release_rx
            .blocking_recv()
            .context("capture release sender dropped")?;
        fifo.write_all(&head_contents)?;
        Ok(())
    });

    let _ = first_response_gate_tx.send(());
    timeout(Duration::from_secs(5), capture_started_rx)
        .await
        .context("timed out waiting for validation fingerprint capture")??;
    let steering_result = steer_user_input(&test.codex, "make the requested change").await;
    let _ = capture_release_tx.send(());
    capture_task
        .await
        .context("validation fingerprint capture task panicked")??;
    steering_result?;

    let events = collect_events_until_terminal(&test.codex).await?;

    assert!(test.workspace_path("ignored-by-git.txt").exists());
    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 2);
    assert_eq!(
        validation_events[0].status,
        ProjectValidationStatus::Skipped
    );
    assert_eq!(
        validation_events[0].skip_reason,
        Some(ProjectValidationSkipReason::UnchangedFingerprint)
    );
    assert_eq!(validation_events[1].status, ProjectValidationStatus::Passed);
    assert_eq!(validation_events[1].output, "validation-pass");
    assert_eq!(server.requests().await.len(), 3);
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_retries_pass_after_pending_input() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let command = shell_command_with_args(
        "count=0; if [ -f \"$1\" ]; then count=$(cat \"$1\"); fi; count=$((count + 1)); printf '%s' \"$count\" > \"$1\"; if [ \"$count\" -eq 1 ]; then touch \"$2\"; while [ ! -f \"$3\" ]; do sleep 0.01; done; fi; printf 'validation-pass-%s' \"$count\"",
        &[
            "project-validation",
            "validation-count",
            "validation-started",
            "validation-release",
        ],
        /*timeout_ms*/ 5_000,
    );
    let first_patch =
        "*** Begin Patch\n*** Add File: first.rs\n+pub fn first() {}\n*** End Patch\n";
    let second_patch =
        "*** Begin Patch\n*** Add File: second.rs\n+pub fn second() {}\n*** End Patch\n";
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            streaming_chunk(ev_response_created("resp-pass-1")),
            streaming_chunk(ev_apply_patch_custom_tool_call("pass-first", first_patch)),
            streaming_chunk(ev_completed("resp-pass-1")),
        ],
        response_completed_chunks("resp-pass-2"),
        vec![
            streaming_chunk(ev_response_created("resp-pass-3")),
            streaming_chunk(ev_apply_patch_custom_tool_call("pass-second", second_patch)),
            streaming_chunk(ev_completed("resp-pass-3")),
        ],
        response_completed_chunks("resp-pass-4"),
    ])
    .await;
    let test = build_streaming_validation_codex(&server, command).await?;
    init_git_repo(test.cwd_path())?;
    let count_path = test.workspace_path("validation-count");
    let started_path = test.workspace_path("validation-started");
    let release_path = test.workspace_path("validation-release");

    submit_user_input(&test.codex, &test, "make the first change").await?;
    wait_for_path(&started_path).await?;
    steer_user_input(&test.codex, "also make the second change").await?;
    std::fs::write(&release_path, "release")?;

    let events = collect_events_until_terminal(&test.codex).await?;

    assert!(test.workspace_path("first.rs").exists());
    assert!(test.workspace_path("second.rs").exists());
    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 2);
    assert_eq!(validation_events[0].status, ProjectValidationStatus::Passed);
    assert_eq!(validation_events[0].output, "validation-pass-1");
    assert_eq!(validation_events[1].status, ProjectValidationStatus::Passed);
    assert_eq!(validation_events[1].output, "validation-pass-2");
    assert_eq!(std::fs::read_to_string(&count_path)?, "2");
    assert_eq!(server.requests().await.len(), 4);
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_runs_after_shell_tool_writes_ignored_file() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-1"),
                ev_shell_command_call("shell-1", "touch ignored-by-git.txt"),
                ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                ev_assistant_message("msg-1", "work complete"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let test = build_validation_codex(
        &server,
        shell_command("printf validation-pass", /*timeout_ms*/ 5_000),
    )
    .await?;
    std::fs::write(test.workspace_path(".gitignore"), "ignored-by-git.txt\n")?;
    init_git_repo(test.cwd_path())?;

    submit_user_input(&test.codex, &test, "make the requested change").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    assert!(test.workspace_path("ignored-by-git.txt").exists());
    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(validation_events[0].status, ProjectValidationStatus::Passed);
    assert_eq!(validation_events[0].output, "validation-pass");
    assert_eq!(response_mock.requests().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_runs_after_server_tool_activity() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            ev_response_created("resp-1"),
            serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "tool_search_call",
                    "call_id": null,
                    "status": "completed",
                    "execution": "server",
                    "arguments": {"query": "inspect the project"},
                }
            }),
            ev_assistant_message("msg-1", "work complete"),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let test = build_validation_codex(
        &server,
        shell_command("printf validation-pass", /*timeout_ms*/ 5_000),
    )
    .await?;
    init_git_repo(test.cwd_path())?;

    submit_user_input(&test.codex, &test, "make the requested change").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(validation_events[0].status, ProjectValidationStatus::Passed);
    assert_eq!(validation_events[0].output, "validation-pass");
    assert_eq!(response_mock.requests().len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_runs_after_untracked_change_outside_session_cwd() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let (response_gate_tx, response_gate_rx) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![vec![
        streaming_chunk(ev_response_created("resp-1")),
        gated_streaming_chunk(
            response_gate_rx,
            vec![
                ev_assistant_message("msg-1", "work complete"),
                ev_completed("resp-1"),
            ],
        ),
    ]])
    .await;
    let command = shell_command("printf validation-pass", /*timeout_ms*/ 5_000);
    let mut builder = test_codex()
        .with_config(move |config| {
            config.cwd = config.cwd.join("nested");
            config.validation.project_command = Some(command);
        })
        .with_workspace_setup(|cwd, fs| async move {
            let cwd = PathUri::from_host_native_path(cwd)?;
            fs.create_directory(
                &cwd,
                CreateDirectoryOptions { recursive: true },
                /*sandbox*/ None,
            )
            .await?;
            Ok::<(), anyhow::Error>(())
        });
    let test = builder.build_with_streaming_server(&server).await?;
    init_git_repo(test.cwd_path())?;

    submit_user_input(&test.codex, &test, "inspect the nested project").await?;
    server.wait_for_request_count(/*count*/ 1).await;
    std::fs::write(test.workspace_path("outside-session-cwd.txt"), "changed")?;
    let _ = response_gate_tx.send(());
    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(validation_events[0].status, ProjectValidationStatus::Passed);
    assert_eq!(validation_events[0].output, "validation-pass");
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_runs_after_head_changes_with_clean_worktree() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let (response_gate_tx, response_gate_rx) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![vec![
        streaming_chunk(ev_response_created("resp-1")),
        gated_streaming_chunk(
            response_gate_rx,
            vec![
                ev_assistant_message("msg-1", "work complete"),
                ev_completed("resp-1"),
            ],
        ),
    ]])
    .await;
    let test = build_streaming_validation_codex(
        &server,
        shell_command("printf validation-pass", /*timeout_ms*/ 5_000),
    )
    .await?;
    init_git_repo(test.cwd_path())?;

    submit_user_input(&test.codex, &test, "inspect the current branch").await?;
    server.wait_for_request_count(/*count*/ 1).await;
    run_git(
        test.cwd_path(),
        &["commit", "--quiet", "--allow-empty", "-m", "turn commit"],
    )?;
    let _ = response_gate_tx.send(());
    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(validation_events[0].status, ProjectValidationStatus::Passed);
    assert_eq!(validation_events[0].output, "validation-pass");
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_owned_rerun_ignores_unchanged_worktree_gate() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-1"),
                ev_shell_command_call("shell-1", "touch changed-by-shell.txt"),
                ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                ev_assistant_message("msg-1", "initial work complete"),
                ev_completed("resp-2"),
            ]),
            responses::sse(vec![
                ev_assistant_message("msg-2", "correction complete"),
                ev_completed("resp-3"),
            ]),
        ],
    )
    .await;
    let test = build_validation_codex(
        &server,
        shell_command(
            "printf validation-fail >&2; exit 7",
            /*timeout_ms*/ 5_000,
        ),
    )
    .await?;
    init_git_repo(test.cwd_path())?;

    submit_user_input(&test.codex, &test, "make the requested change").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 2);
    for event in validation_events {
        assert_eq!(event.status, ProjectValidationStatus::ActionableFailure);
        assert_eq!(event.output, "validation-fail");
    }
    assert_eq!(response_mock.requests().len(), 3);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_non_git_workspace_fails_open_before_turn_completion() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let events = run_validation_turn(shell_command("printf validation-pass", 5_000)).await?;
    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    let event = validation_events[0];
    assert_eq!(event.status, ProjectValidationStatus::Passed);
    assert_eq!(event.exit_code, Some(0));
    assert_eq!(event.output, "validation-pass");
    assert!(!event.output_truncated);

    let validation_index = events
        .iter()
        .position(|event| matches!(event, EventMsg::ProjectValidationCompleted(_)))
        .expect("validation event should be present");
    let completion_index = events
        .iter()
        .position(|event| matches!(event, EventMsg::TurnComplete(_)))
        .expect("turn completion should be present");
    assert!(validation_index < completion_index);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_unborn_git_repo_fails_open() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    responses::mount_sse_once(&server, sse_completed("resp-1")).await;
    let test = build_validation_codex(
        &server,
        shell_command("printf validation-pass", /*timeout_ms*/ 5_000),
    )
    .await?;
    run_git(test.cwd_path(), &["init", "--quiet"])?;

    submit_user_input(&test.codex, &test, "finish the work").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(validation_events[0].status, ProjectValidationStatus::Passed);
    assert_eq!(validation_events[0].output, "validation-pass");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_unreadable_git_fingerprint_fails_open() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    responses::mount_sse_once(&server, sse_completed("resp-1")).await;
    let test = build_validation_codex(
        &server,
        shell_command("printf validation-pass", /*timeout_ms*/ 5_000),
    )
    .await?;
    init_git_repo(test.cwd_path())?;
    std::fs::write(test.workspace_path(".git/index"), "invalid-index")?;

    submit_user_input(&test.codex, &test, "finish the work").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(validation_events[0].status, ProjectValidationStatus::Passed);
    assert_eq!(validation_events[0].output, "validation-pass");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_reports_bounded_actionable_failure() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let script = concat!(
        "printf 'validation-start\\n'; ",
        "i=0; while [ $i -lt 12000 ]; do printf x; i=$((i + 1)); done; ",
        "printf '\\nvalidation-end\\n' >&2; exit 7"
    );
    let (events, requests) = run_validation_correction_turn(shell_command(script, 5_000)).await?;
    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 2);
    for event in &validation_events {
        assert_eq!(event.status, ProjectValidationStatus::ActionableFailure);
        assert_eq!(event.exit_code, Some(7));
        assert!(event.output_truncated);
        assert!(event.output.len() <= 8 * 1024);
        assert!(event.output.contains("project validation output truncated"));
        assert!(event.output.starts_with("validation-start"));
        assert!(event.output.ends_with("validation-end\n"));
    }

    assert_eq!(requests.len(), 2);
    let correction_fragments = requests[1]
        .message_input_texts("user")
        .into_iter()
        .filter(|text| text.starts_with("<project_validation_failure>"))
        .collect::<Vec<_>>();
    assert_eq!(correction_fragments.len(), 1);
    let correction_fragment = &correction_fragments[0];
    assert!(correction_fragment.contains("Exit code: 7"));
    assert!(correction_fragment.contains("validation-start"));
    assert!(correction_fragment.contains("validation-end"));
    assert!(correction_fragment.len() <= 960);
    assert!(approx_token_count(correction_fragment) < 1_000);
    let initial_skills_fragments = requests[0]
        .message_input_texts("developer")
        .into_iter()
        .filter(|text| text.starts_with(SKILLS_INSTRUCTIONS_OPEN_TAG))
        .count();
    let correction_skills_fragments = requests[1]
        .message_input_texts("developer")
        .into_iter()
        .filter(|text| text.starts_with(SKILLS_INSTRUCTIONS_OPEN_TAG))
        .count();
    assert_eq!(initial_skills_fragments, 1);
    assert_eq!(correction_skills_fragments, 1);

    let validation_indices = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            matches!(event, EventMsg::ProjectValidationCompleted(_)).then_some(index)
        })
        .collect::<Vec<_>>();
    let completion_index = events
        .iter()
        .position(|event| matches!(event, EventMsg::TurnComplete(_)))
        .expect("turn completion should be present");
    assert!(validation_indices[0] < validation_indices[1]);
    assert!(validation_indices[1] < completion_index);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_correction_preserves_the_aggregated_turn_diff() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let command = shell_command_with_args(
        "if [ -f \"$1\" ]; then printf validation-pass; exit 0; fi; touch \"$1\"; printf validation-fail >&2; exit 7",
        &["project-validation", ".validation-ran"],
        /*timeout_ms*/ 5_000,
    );
    let initial_patch =
        "*** Begin Patch\n*** Add File: initial-change.txt\n+initial\n*** End Patch\n";
    let correction_patch =
        "*** Begin Patch\n*** Add File: correction-change.txt\n+correction\n*** End Patch\n";
    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-1"),
                ev_apply_patch_custom_tool_call("initial-patch", initial_patch),
                ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                ev_assistant_message("msg-1", "initial work complete"),
                ev_completed("resp-2"),
            ]),
            responses::sse(vec![
                ev_response_created("resp-3"),
                ev_apply_patch_custom_tool_call("correction-patch", correction_patch),
                ev_completed("resp-3"),
            ]),
            responses::sse(vec![
                ev_assistant_message("msg-2", "correction complete"),
                ev_completed("resp-4"),
            ]),
        ],
    )
    .await;
    let test = build_validation_codex(&server, command).await?;

    submit_user_input(&test.codex, &test, "finish the work").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let final_diff = events
        .iter()
        .filter_map(|event| match event {
            EventMsg::TurnDiff(event) => Some(event.unified_diff.as_str()),
            _ => None,
        })
        .next_back()
        .expect("correction should emit an aggregated turn diff");
    assert!(final_diff.contains("initial-change.txt"));
    assert!(final_diff.contains("correction-change.txt"));
    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 2);
    assert_eq!(
        validation_events[0].status,
        ProjectValidationStatus::ActionableFailure
    );
    assert_eq!(validation_events[0].output, "validation-fail");
    assert_eq!(validation_events[1].status, ProjectValidationStatus::Passed);
    assert_eq!(validation_events[1].output, "validation-pass");
    assert_eq!(response_mock.requests().len(), 4);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_shellcheck_provider_runs_one_correction_cycle() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = tempdir()?;
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .context("initialize shellcheck provider repository")?;
    std::fs::write(
        repo.path().join("fake-shellcheck"),
        concat!(
            "set -eu\n",
            "if [ \"$#\" -ne 3 ] || [ \"$1\" != \"-f\" ] || [ \"$2\" != \"gcc\" ] || [ \"$3\" != \"scripts/check.sh\" ]; then exit 64; fi\n",
            "if [ -f .shellcheck-ran ]; then printf shellcheck-pass; exit 0; fi\n",
            "touch .shellcheck-ran\n",
            "printf shellcheck-fail >&2\n",
            "exit 7\n",
        ),
    )?;
    let patch =
        "*** Begin Patch\n*** Add File: scripts/check.sh\n+#!/bin/sh\n+printf ok\n*** End Patch\n";
    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-shellcheck-1"),
                ev_apply_patch_custom_tool_call("shellcheck-patch", patch),
                ev_completed("resp-shellcheck-1"),
            ]),
            sse_completed("resp-shellcheck-2"),
            sse_completed("resp-shellcheck-3"),
        ],
    )
    .await;
    let test = build_shellcheck_provider_codex(
        &server,
        repo.path(),
        vec!["/bin/sh".to_string(), "fake-shellcheck".to_string()],
        None,
    )
    .await?;

    submit_user_input(&test.codex, &test, "finish the shell change").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 2);
    assert_eq!(
        validation_events[0].command,
        vec![
            "/bin/sh",
            "fake-shellcheck",
            "-f",
            "gcc",
            "scripts/check.sh",
        ]
    );
    assert_eq!(
        validation_events[0].status,
        ProjectValidationStatus::ActionableFailure
    );
    assert_eq!(validation_events[0].output, "shellcheck-fail");
    assert_eq!(validation_events[1].status, ProjectValidationStatus::Passed);
    assert_eq!(validation_events[1].output, "shellcheck-pass");
    let requests = response_mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2]
            .message_input_texts("user")
            .into_iter()
            .filter(|text| text.starts_with("<project_validation_failure>"))
            .count(),
        1
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_cargo_provider_runs_one_json_diagnostic_correction_cycle() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = tempdir()?;
    std::fs::write(
        repo.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    std::fs::write(
        repo.path().join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.95.0\"\n",
    )?;
    init_git_repo(repo.path())?;
    let provider = tempdir()?;
    let rustup_shim = provider.path().join("rustup");
    let fake_cargo = provider.path().join("cargo");
    let count = provider.path().join("count");
    let canonical_repo = dunce::canonicalize(repo.path())?;
    write_executable(
        &rustup_shim,
        &format!(
            concat!(
                "#!/bin/sh\n",
                "set -eu\n",
                "if [ \"$8\" != '--manifest-path' ] || [ \"$9\" != '{manifest}' ]; then exit 64; fi\n",
                "case \"$PWD\" in '{repo}'|'{repo}'/*) exit 64 ;; esac\n",
                "grep -Fq 'channel = \"1.95.0\"' \"$PWD/rust-toolchain.toml\" || exit 64\n",
                "count=0\n",
                "if [ -f '{count}' ]; then count=$(cat '{count}'); fi\n",
                "count=$((count + 1))\n",
                "printf '%s' \"$count\" > '{count}'\n",
                "if [ \"$count\" -eq 1 ]; then\n",
                "  printf '%s\\n' '{{\"reason\":\"compiler-message\",\"message\":{{\"level\":\"error\",\"message\":\"mismatched types\",\"code\":{{\"code\":\"E0308\"}},\"spans\":[{{\"file_name\":\"src/lib.rs\",\"line_start\":1,\"column_start\":25,\"is_primary\":true}}]}}}}'\n",
                "  exit 101\n",
                "fi\n",
                "exit 0\n",
            ),
            count = count.display(),
            manifest = canonical_repo.join("Cargo.toml").display(),
            repo = canonical_repo.display(),
        ),
    )?;
    symlink(&rustup_shim, &fake_cargo)?;
    let patch = "*** Begin Patch\n*** Add File: src/lib.rs\n+pub fn value() -> u8 { \"bad\" }\n*** End Patch\n";
    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-cargo-1"),
                ev_apply_patch_custom_tool_call("cargo-patch", patch),
                ev_completed("resp-cargo-1"),
            ]),
            sse_completed("resp-cargo-2"),
            sse_completed("resp-cargo-3"),
            responses::sse(vec![
                ev_response_created("resp-cargo-4"),
                ev_shell_command_call("cargo-cache-shell", "true"),
                ev_completed("resp-cargo-4"),
            ]),
            sse_completed("resp-cargo-5"),
        ],
    )
    .await;
    let test = build_cargo_provider_codex(
        &server,
        repo.path(),
        fake_cargo
            .to_str()
            .context("fake cargo path should be UTF-8")?
            .to_string(),
        /*timeout_ms*/ 5_000,
    )
    .await?;

    submit_user_input(&test.codex, &test, "finish the Rust change").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let initial_validation = validation_events(&events);
    assert_eq!(initial_validation.len(), 2);
    assert_eq!(
        initial_validation[0].status,
        ProjectValidationStatus::ActionableFailure
    );
    assert_eq!(
        initial_validation[0].output,
        "src/lib.rs:1:25: error[E0308]: mismatched types"
    );
    assert!(
        initial_validation[0]
            .command
            .windows(2)
            .any(|arguments| arguments == ["--jobs", "2"])
    );
    assert!(
        initial_validation[0]
            .command
            .contains(&"--message-format=json".to_string())
    );
    assert_eq!(
        initial_validation[1].status,
        ProjectValidationStatus::Passed
    );
    assert_eq!(initial_validation[1].output, "cargo check passed");
    for validation in &initial_validation {
        let target_dir = validation
            .command
            .iter()
            .position(|argument| argument == "--target-dir")
            .and_then(|index| validation.command.get(index + 1))
            .context("cargo validation command should include a target directory")?;
        assert!(!Path::new(target_dir).exists());
    }
    assert_eq!(std::fs::read_to_string(&count)?, "2");
    let requests = response_mock.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]
            .message_input_texts("user")
            .iter()
            .any(|text| text.contains("src/lib.rs:1:25: error[E0308]: mismatched types"))
    );

    submit_user_input(&test.codex, &test, "inspect the corrected Rust fixture").await?;
    let cached_events = collect_events_until_terminal(&test.codex).await?;
    let cached_validation = validation_events(&cached_events);
    assert_eq!(cached_validation.len(), 1);
    assert_eq!(
        cached_validation[0].status,
        ProjectValidationStatus::Skipped
    );
    assert_eq!(
        cached_validation[0].skip_reason,
        Some(ProjectValidationSkipReason::UnchangedFingerprint)
    );
    assert_eq!(std::fs::read_to_string(&count)?, "2");
    assert_eq!(response_mock.requests().len(), 5);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_cargo_provider_ignores_repository_rustc_wrapper() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = tempdir()?;
    std::fs::create_dir_all(repo.path().join(".cargo"))?;
    std::fs::create_dir_all(repo.path().join("src"))?;
    std::fs::write(
        repo.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    std::fs::write(
        repo.path().join("Cargo.lock"),
        "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )?;
    std::fs::write(
        repo.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )?;
    let wrapper_marker = repo.path().join("wrapper-ran");
    write_executable(
        &repo.path().join("wrapper.sh"),
        &format!(
            "#!/bin/sh\ntouch '{}'\nexec \"$@\"\n",
            wrapper_marker.display()
        ),
    )?;
    std::fs::write(
        repo.path().join(".cargo/config.toml"),
        "[build]\nrustc-wrapper = \"./wrapper.sh\"\n",
    )?;
    init_git_repo(repo.path())?;

    let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-pub fn value() -> u8 { 1 }\n+pub fn value() -> u8 { 2 }\n*** End Patch\n";
    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-cargo-wrapper-1"),
                ev_apply_patch_custom_tool_call("cargo-wrapper-patch", patch),
                ev_completed("resp-cargo-wrapper-1"),
            ]),
            sse_completed("resp-cargo-wrapper-2"),
        ],
    )
    .await;
    let test = build_cargo_provider_codex(
        &server,
        repo.path(),
        "cargo".to_string(),
        /*timeout_ms*/ 30_000,
    )
    .await?;

    submit_user_input(&test.codex, &test, "update the Rust fixture").await?;
    let events = collect_events_until_terminal(&test.codex).await?;
    let validation = validation_events(&events);

    assert_eq!(validation.len(), 1);
    assert_eq!(validation[0].status, ProjectValidationStatus::Passed);
    assert!(!wrapper_marker.exists());
    assert_eq!(response_mock.requests().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_cargo_provider_rejects_repository_cargo_symlink() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = tempdir()?;
    std::fs::create_dir_all(repo.path().join("bin"))?;
    std::fs::write(
        repo.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    let provider = tempdir()?;
    let marker = provider.path().join("cargo-ran");
    let external_rustup = provider.path().join("rustup");
    write_executable(
        &external_rustup,
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )?;
    let repository_cargo = repo.path().join("bin/cargo");
    symlink(&external_rustup, &repository_cargo)?;
    init_git_repo(repo.path())?;

    let patch = "*** Begin Patch\n*** Add File: src/lib.rs\n+pub fn value() {}\n*** End Patch\n";
    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-cargo-symlink-1"),
                ev_apply_patch_custom_tool_call("cargo-symlink-patch", patch),
                ev_completed("resp-cargo-symlink-1"),
            ]),
            sse_completed("resp-cargo-symlink-2"),
        ],
    )
    .await;
    let test = build_cargo_provider_codex(
        &server,
        repo.path(),
        repository_cargo
            .to_str()
            .context("repository cargo path should be UTF-8")?
            .to_string(),
        /*timeout_ms*/ 5_000,
    )
    .await?;

    submit_user_input(&test.codex, &test, "add the Rust fixture").await?;
    let events = collect_events_until_terminal(&test.codex).await?;
    let validation = validation_events(&events);

    assert_eq!(validation.len(), 1);
    assert_eq!(
        validation[0].status,
        ProjectValidationStatus::ConfigurationError
    );
    assert!(validation[0].output.contains("inside the repository"));
    assert!(!marker.exists());
    assert_eq!(response_mock.requests().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_cargo_provider_reports_timeout_without_correction() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = tempdir()?;
    std::fs::write(
        repo.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )?;
    init_git_repo(repo.path())?;
    let provider = tempdir()?;
    let fake_cargo = provider.path().join("cargo");
    write_executable(&fake_cargo, "#!/bin/sh\nsleep 2\n")?;
    let patch = "*** Begin Patch\n*** Add File: src/lib.rs\n+pub fn value() {}\n*** End Patch\n";
    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-cargo-timeout-1"),
                ev_apply_patch_custom_tool_call("cargo-timeout-patch", patch),
                ev_completed("resp-cargo-timeout-1"),
            ]),
            sse_completed("resp-cargo-timeout-2"),
        ],
    )
    .await;
    let test = build_cargo_provider_codex(
        &server,
        repo.path(),
        fake_cargo
            .to_str()
            .context("fake cargo path should be UTF-8")?
            .to_string(),
        /*timeout_ms*/ 50,
    )
    .await?;

    submit_user_input(&test.codex, &test, "finish the Rust change").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(
        validation_events[0].status,
        ProjectValidationStatus::TimedOut
    );
    assert_eq!(response_mock.requests().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_cargo_provider_deduplicates_successful_fingerprint() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = tempdir()?;
    std::fs::write(
        repo.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )?;
    init_git_repo(repo.path())?;
    let linked_worktree_parent = tempdir()?;
    let linked_worktree = linked_worktree_parent.path().join("linked-worktree");
    let linked_worktree_arg = linked_worktree
        .to_str()
        .context("linked worktree path should be UTF-8")?;
    run_git(
        repo.path(),
        &["worktree", "add", "--detach", linked_worktree_arg, "HEAD"],
    )?;
    let provider = tempdir()?;
    let fake_cargo = provider.path().join("cargo");
    let count = provider.path().join("count");
    write_executable(
        &fake_cargo,
        &format!(
            "#!/bin/sh\ncount=0\nif [ -f '{count}' ]; then count=$(cat '{count}'); fi\ncount=$((count + 1))\nprintf '%s' \"$count\" > '{count}'\n",
            count = count.display(),
        ),
    )?;
    let patch = "*** Begin Patch\n*** Add File: src/lib.rs\n+pub fn value() {}\n*** End Patch\n";
    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-cargo-dedupe-1"),
                ev_apply_patch_custom_tool_call("cargo-dedupe-patch", patch),
                ev_completed("resp-cargo-dedupe-1"),
            ]),
            sse_completed("resp-cargo-dedupe-2"),
            responses::sse(vec![
                ev_response_created("resp-cargo-dedupe-3"),
                ev_apply_patch_custom_tool_call("cargo-dedupe-linked-patch", patch),
                ev_completed("resp-cargo-dedupe-3"),
            ]),
            sse_completed("resp-cargo-dedupe-4"),
        ],
    )
    .await;
    let test = build_cargo_provider_codex(
        &server,
        repo.path(),
        fake_cargo
            .to_str()
            .context("fake cargo path should be UTF-8")?
            .to_string(),
        /*timeout_ms*/ 5_000,
    )
    .await?;

    submit_user_input(&test.codex, &test, "add the Rust fixture").await?;
    let first_events = collect_events_until_terminal(&test.codex).await?;
    assert_eq!(
        validation_events(&first_events)[0].status,
        ProjectValidationStatus::Passed
    );

    let linked = start_root_thread_with_cwd(&test, &linked_worktree).await?;
    let linked_cwd = AbsolutePathBuf::from_absolute_path(&linked_worktree)
        .context("linked worktree path should be absolute")?;
    submit_user_input_at_cwd(
        &linked,
        &test,
        "add the equivalent Rust fixture",
        linked_cwd,
    )
    .await?;
    let second_events = collect_events_until_terminal(&linked).await?;
    let second_validation = validation_events(&second_events);
    assert_eq!(second_validation.len(), 1);
    assert_eq!(
        second_validation[0].status,
        ProjectValidationStatus::Skipped
    );
    assert_eq!(
        second_validation[0].skip_reason,
        Some(ProjectValidationSkipReason::UnchangedFingerprint)
    );
    assert_eq!(std::fs::read_to_string(count)?, "1");
    assert_eq!(response_mock.requests().len(), 4);
    run_git(
        repo.path(),
        &["worktree", "remove", "--force", linked_worktree_arg],
    )?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_validation_reports_disabled_disposition() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let patch = "*** Begin Patch\n*** Add File: changed.txt\n+changed\n*** End Patch\n";
    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-disabled-1"),
                ev_apply_patch_custom_tool_call("disabled-patch", patch),
                ev_completed("resp-disabled-1"),
            ]),
            sse_completed("resp-disabled-2"),
        ],
    )
    .await;
    let mut builder = test_codex().with_config(|config| {
        config.validation.groups.functional = false;
        config.validation.project_command = None;
    });
    let test = builder.build(&server).await?;
    init_git_repo(test.cwd_path())?;

    submit_user_input(&test.codex, &test, "make the requested change").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let event =
        assert_single_validation_skip(&events, ProjectValidationSkipReason::ValidationDisabled);
    assert_eq!(event.changed_file_count, None);
    assert_eq!(response_mock.requests().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_validation_disabled_precedes_unsupported_environment() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    responses::mount_sse_once(&server, sse_completed("resp-disabled-no-environment")).await;
    let mut builder = test_codex().with_config(|config| {
        config.validation.groups.functional = false;
        config.validation.project_command = None;
    });
    let test = builder.build(&server).await?;

    submit_user_input_without_environments(&test.codex, &test, "finish the work").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let event =
        assert_single_validation_skip(&events, ProjectValidationSkipReason::ValidationDisabled);
    assert_eq!(event.cwd, None);
    assert_eq!(event.changed_file_count, None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_validation_reports_no_changed_files() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = tempdir()?;
    init_git_repo(repo.path())?;
    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-no-changes-1"),
                ev_shell_command_call("no-changes-shell", "true"),
                ev_completed("resp-no-changes-1"),
            ]),
            sse_completed("resp-no-changes-2"),
        ],
    )
    .await;
    let test = build_shellcheck_provider_codex(
        &server,
        repo.path(),
        vec!["/usr/bin/true".to_string()],
        None,
    )
    .await?;

    submit_user_input(&test.codex, &test, "inspect without changing files").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let event = assert_single_validation_skip(&events, ProjectValidationSkipReason::NoChangedFiles);
    assert!(event.command.is_empty());
    assert_eq!(event.changed_file_count, Some(0));
    assert_eq!(response_mock.requests().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_validation_reports_no_applicable_provider() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = tempdir()?;
    init_git_repo(repo.path())?;
    let patch =
        "*** Begin Patch\n*** Add File: lib.rs\n+pub fn value() -> u64 { 1 }\n*** End Patch\n";
    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-no-provider-1"),
                ev_apply_patch_custom_tool_call("no-provider-patch", patch),
                ev_completed("resp-no-provider-1"),
            ]),
            sse_completed("resp-no-provider-2"),
        ],
    )
    .await;
    let test = build_shellcheck_provider_codex(
        &server,
        repo.path(),
        vec!["/usr/bin/true".to_string()],
        None,
    )
    .await?;

    submit_user_input(&test.codex, &test, "add the Rust fixture").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let event =
        assert_single_validation_skip(&events, ProjectValidationSkipReason::NoApplicableProvider);
    assert!(event.command.is_empty());
    assert_eq!(event.changed_file_count, Some(1));
    assert_eq!(response_mock.requests().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_validation_reports_unsupported_environment() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let workspace = tempdir()?;
    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-unsupported-1"),
                ev_shell_command_call("unsupported-shell", "true"),
                ev_completed("resp-unsupported-1"),
            ]),
            sse_completed("resp-unsupported-2"),
        ],
    )
    .await;
    let test = build_shellcheck_provider_codex(
        &server,
        workspace.path(),
        vec!["/usr/bin/true".to_string()],
        None,
    )
    .await?;

    submit_user_input(&test.codex, &test, "inspect this workspace").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let event =
        assert_single_validation_skip(&events, ProjectValidationSkipReason::UnsupportedEnvironment);
    assert!(event.command.is_empty());
    assert_eq!(event.changed_file_count, None);
    assert_eq!(response_mock.requests().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_project_command_preserves_unsupported_environment_failure() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    responses::mount_sse_once(&server, sse_completed("resp-unsupported-command")).await;
    let test = build_validation_codex(&server, shell_command("true", 5_000)).await?;

    submit_user_input_without_environments(&test.codex, &test, "finish the work").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(
        validation_events[0].status,
        ProjectValidationStatus::InfrastructureFailure
    );
    assert_eq!(validation_events[0].skip_reason, None);
    assert_eq!(
        validation_events[0].output,
        "project validation requires exactly one local turn environment"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_project_command_takes_precedence_over_shellcheck_provider() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = tempdir()?;
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .context("initialize validation precedence repository")?;
    std::fs::write(
        repo.path().join("fake-shellcheck"),
        "touch provider-ran\nprintf provider-pass\n",
    )?;
    let patch =
        "*** Begin Patch\n*** Add File: scripts/check.sh\n+#!/bin/sh\n+printf ok\n*** End Patch\n";
    let server = start_mock_server().await;
    responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("resp-precedence-1"),
                ev_apply_patch_custom_tool_call("precedence-patch", patch),
                ev_completed("resp-precedence-1"),
            ]),
            sse_completed("resp-precedence-2"),
        ],
    )
    .await;
    let project_command = shell_command(
        "touch project-command-ran; printf project-command-pass",
        /*timeout_ms*/ 5_000,
    );
    let test = build_shellcheck_provider_codex(
        &server,
        repo.path(),
        vec!["/bin/sh".to_string(), "fake-shellcheck".to_string()],
        Some(project_command),
    )
    .await?;

    submit_user_input(&test.codex, &test, "finish the shell change").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(validation_events[0].output, "project-command-pass");
    assert!(repo.path().join("project-command-ran").is_file());
    assert!(!repo.path().join("provider-ran").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_steering_joins_the_single_correction_cycle() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let command = shell_command_with_args(
        "count=0; if [ -f \"$1\" ]; then count=$(cat \"$1\"); fi; count=$((count + 1)); printf '%s' \"$count\" > \"$1\"; if [ \"$count\" -eq 1 ]; then touch \"$2\"; while [ ! -f \"$3\" ]; do sleep 0.01; done; printf validation-fail >&2; exit 7; fi; printf validation-pass",
        &[
            "project-validation",
            "validation-count",
            "validation-started",
            "validation-release",
        ],
        /*timeout_ms*/ 5_000,
    );
    let (server, _completions) = start_streaming_sse_server(vec![
        response_completed_chunks("resp-1"),
        response_completed_chunks("resp-2"),
    ])
    .await;
    let test = build_streaming_validation_codex(&server, command).await?;
    let count_path = test.workspace_path("validation-count");
    let started_path = test.workspace_path("validation-started");
    let release_path = test.workspace_path("validation-release");

    submit_user_input(&test.codex, &test, "finish the work").await?;
    wait_for_path(&started_path).await?;
    steer_user_input(&test.codex, "also preserve the public API").await?;
    std::fs::write(&release_path, "release")?;

    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 2);
    assert_eq!(
        validation_events[0].status,
        ProjectValidationStatus::ActionableFailure
    );
    assert_eq!(validation_events[1].status, ProjectValidationStatus::Passed);
    assert_eq!(std::fs::read_to_string(&count_path)?, "2");

    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    let correction_request: Value = serde_json::from_slice(&requests[1])?;
    let user_texts = request_message_input_texts(&correction_request, "user");
    let correction_index = user_texts
        .iter()
        .position(|text| text.starts_with("<project_validation_failure>"))
        .expect("correction request should include validation failure context");
    let steering_index = user_texts
        .iter()
        .position(|text| text == "also preserve the public API")
        .expect("correction request should include pending steering");
    assert!(correction_index < steering_index);

    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_steering_during_rerun_does_not_reopen_correction_cycle() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let command = shell_command_with_args(
        "count=0; if [ -f \"$1\" ]; then count=$(cat \"$1\"); fi; count=$((count + 1)); printf '%s' \"$count\" > \"$1\"; case \"$count\" in 1) printf validation-fail-1 >&2; exit 7 ;; 2) touch \"$2\"; while [ ! -f \"$3\" ]; do sleep 0.01; done; printf validation-pass-2 ;; 3) printf validation-fail-3 >&2; exit 9 ;; *) printf 'validation-pass-%s' \"$count\" ;; esac",
        &[
            "project-validation",
            "validation-count",
            "validation-rerun-started",
            "validation-rerun-release",
        ],
        /*timeout_ms*/ 5_000,
    );
    let first_patch =
        "*** Begin Patch\n*** Add File: first.rs\n+pub fn first() {}\n*** End Patch\n";
    let second_patch =
        "*** Begin Patch\n*** Add File: second.rs\n+pub fn second() {}\n*** End Patch\n";
    let (server, _completions) = start_streaming_sse_server(vec![
        vec![
            streaming_chunk(ev_response_created("resp-rerun-steer-1")),
            streaming_chunk(ev_apply_patch_custom_tool_call(
                "rerun-steer-first",
                first_patch,
            )),
            streaming_chunk(ev_completed("resp-rerun-steer-1")),
        ],
        response_completed_chunks("resp-rerun-steer-2"),
        response_completed_chunks("resp-rerun-steer-3"),
        vec![
            streaming_chunk(ev_response_created("resp-rerun-steer-4")),
            streaming_chunk(ev_apply_patch_custom_tool_call(
                "rerun-steer-second",
                second_patch,
            )),
            streaming_chunk(ev_completed("resp-rerun-steer-4")),
        ],
        response_completed_chunks("resp-rerun-steer-5"),
        response_completed_chunks("resp-rerun-steer-6"),
    ])
    .await;
    let test = build_streaming_validation_codex(&server, command).await?;
    init_git_repo(test.cwd_path())?;
    let count_path = test.workspace_path("validation-count");
    let rerun_started_path = test.workspace_path("validation-rerun-started");
    let rerun_release_path = test.workspace_path("validation-rerun-release");

    submit_user_input(&test.codex, &test, "make the first change").await?;
    wait_for_path(&rerun_started_path).await?;
    steer_user_input(&test.codex, "also make the second change").await?;
    std::fs::write(&rerun_release_path, "release")?;

    let events = collect_events_until_terminal(&test.codex).await?;

    assert!(test.workspace_path("first.rs").exists());
    assert!(test.workspace_path("second.rs").exists());
    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 2);
    assert_eq!(
        validation_events[0].status,
        ProjectValidationStatus::ActionableFailure
    );
    assert_eq!(validation_events[0].output, "validation-fail-1");
    assert_eq!(validation_events[1].status, ProjectValidationStatus::Passed);
    assert_eq!(validation_events[1].output, "validation-pass-2");
    let first_item_id = validation_events[0]
        .item_id
        .as_deref()
        .expect("first validation should have an item id");
    let second_item_id = validation_events[1]
        .item_id
        .as_deref()
        .expect("second validation should have an item id");
    assert_ne!(first_item_id, second_item_id);
    assert_eq!(std::fs::read_to_string(&count_path)?, "2");
    assert_eq!(server.requests().await.len(), 5);
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_interrupt_during_correction_prevents_rerun() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let command = shell_command_with_args(
        "count=0; if [ -f \"$1\" ]; then count=$(cat \"$1\"); fi; count=$((count + 1)); printf '%s' \"$count\" > \"$1\"; printf validation-fail >&2; exit 7",
        &["project-validation", "validation-count"],
        /*timeout_ms*/ 5_000,
    );
    let (correction_gate_tx, correction_gate_rx) = oneshot::channel();
    let (server, _completions) = start_streaming_sse_server(vec![
        response_completed_chunks("resp-1"),
        vec![
            streaming_chunk(ev_response_created("resp-2")),
            gated_streaming_chunk(correction_gate_rx, vec![ev_completed("resp-2")]),
        ],
    ])
    .await;
    let test = build_streaming_validation_codex(&server, command).await?;
    let count_path = test.workspace_path("validation-count");

    submit_user_input(&test.codex, &test, "finish the work").await?;
    let first_validation = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::ProjectValidationCompleted(event)
                if event.status == ProjectValidationStatus::ActionableFailure
        )
    })
    .await;
    assert!(matches!(
        first_validation,
        EventMsg::ProjectValidationCompleted(_)
    ));
    server.wait_for_request_count(/*count*/ 2).await;

    test.codex.submit(Op::Interrupt).await?;
    let _ = correction_gate_tx.send(());
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    sleep(Duration::from_millis(100)).await;
    assert_eq!(server.requests().await.len(), 2);
    assert_eq!(std::fs::read_to_string(&count_path)?, "1");

    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_interrupt_during_rerun_cancels_command() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let command = shell_command_with_args(
        "count=0; if [ -f \"$1\" ]; then count=$(cat \"$1\"); fi; count=$((count + 1)); printf '%s' \"$count\" > \"$1\"; if [ \"$count\" -eq 1 ]; then printf validation-fail >&2; exit 7; fi; touch \"$2\"; while :; do sleep 0.05; done",
        &[
            "project-validation",
            "validation-count",
            "validation-rerun-started",
        ],
        /*timeout_ms*/ 5_000,
    );
    let server = start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![sse_completed("resp-1"), sse_completed("resp-2")],
    )
    .await;
    let test = build_validation_codex(&server, command).await?;
    let count_path = test.workspace_path("validation-count");
    let rerun_started_path = test.workspace_path("validation-rerun-started");

    submit_user_input(&test.codex, &test, "finish the work").await?;
    wait_for_path(&rerun_started_path).await?;
    test.codex.submit(Op::Interrupt).await?;

    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 2);
    assert_eq!(
        validation_events[0].status,
        ProjectValidationStatus::ActionableFailure
    );
    assert_eq!(
        validation_events[1].status,
        ProjectValidationStatus::Cancelled
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, EventMsg::TurnAborted(_)))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, EventMsg::TurnComplete(_)))
    );
    assert_eq!(std::fs::read_to_string(&count_path)?, "2");
    assert_eq!(response_mock.requests().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_reports_empty_command_as_configuration_error() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    responses::mount_sse_once(&server, sse_completed("resp-1")).await;
    let test = build_validation_codex(&server, ProjectValidationCommand::default()).await?;
    init_git_repo(test.cwd_path())?;

    submit_user_input(&test.codex, &test, "finish the work").await?;
    let events = collect_events_until_terminal(&test.codex).await?;
    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    let event = validation_events[0];
    assert_eq!(event.status, ProjectValidationStatus::ConfigurationError);
    assert_eq!(event.exit_code, None);
    assert!(event.output.contains("non-empty executable"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_reports_missing_executable_for_unchanged_worktree() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    responses::mount_sse_once(&server, sse_completed("resp-1")).await;
    let test = build_validation_codex(
        &server,
        ProjectValidationCommand {
            command: vec!["missing-project-validation-executable".to_string()],
            timeout_ms: 5_000,
        },
    )
    .await?;
    init_git_repo(test.cwd_path())?;

    submit_user_input(&test.codex, &test, "finish the work").await?;
    let events = collect_events_until_terminal(&test.codex).await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(
        validation_events[0].status,
        ProjectValidationStatus::ConfigurationError
    );
    assert!(validation_events[0].output.contains("not found"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_reports_oversized_command_as_configuration_error() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let events = run_validation_turn(ProjectValidationCommand {
        command: vec!["/bin/echo".to_string(), "x".repeat(9_000)],
        timeout_ms: 5_000,
    })
    .await?;
    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    let event = validation_events[0];
    assert_eq!(event.status, ProjectValidationStatus::ConfigurationError);
    assert!(event.output.contains("8192 bytes"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_reports_invalid_timeout_as_configuration_error() -> Result<()> {
    skip_if_no_network!(Ok(()));

    for timeout_ms in [0, MAX_PROJECT_VALIDATION_TIMEOUT_MS + 1] {
        let events = run_validation_turn(ProjectValidationCommand {
            command: vec!["/bin/echo".to_string(), "ok".to_string()],
            timeout_ms,
        })
        .await?;
        let validation_events = validation_events(&events);
        assert_eq!(validation_events.len(), 1);
        let event = validation_events[0];
        assert_eq!(event.status, ProjectValidationStatus::ConfigurationError);
        assert!(event.output.contains("timeout_ms"));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_is_skipped_for_non_root_agents() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let temp_dir = tempdir()?;
    let marker = temp_dir.path().join("project-validation-ran");
    let events = run_validation_turn_with_source(
        ProjectValidationCommand {
            command: vec![
                "/usr/bin/touch".to_string(),
                marker.to_string_lossy().into_owned(),
            ],
            timeout_ms: 5_000,
        },
        Some(SessionSource::SubAgent(SubAgentSource::Review)),
    )
    .await?;

    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(
        validation_events[0].status,
        ProjectValidationStatus::Skipped
    );
    assert_eq!(
        validation_events[0].skip_reason,
        Some(ProjectValidationSkipReason::NonRootAgent)
    );
    assert!(!marker.exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_reports_timeout_and_completes_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let events = run_validation_turn(shell_command("sleep 5", 50)).await?;
    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    let event = validation_events[0];
    assert_eq!(event.status, ProjectValidationStatus::TimedOut);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, EventMsg::TurnComplete(_)))
    );
    Ok(())
}
