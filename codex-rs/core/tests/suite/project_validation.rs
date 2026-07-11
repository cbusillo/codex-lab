#![cfg(unix)]

use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use codex_config::MAX_PROJECT_VALIDATION_TIMEOUT_MS;
use codex_config::ProjectValidationCommand;
use codex_core::CodexThread;
use codex_core::StartThreadOptions;
use codex_exec_server::CreateDirectoryOptions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ProjectValidationCompletedEvent;
use codex_protocol::protocol::ProjectValidationStatus;
use codex_protocol::protocol::SKILLS_INSTRUCTIONS_OPEN_TAG;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
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
            .start_thread_with_options(StartThreadOptions {
                config: test.config.clone(),
                initial_history: InitialHistory::New,
                session_source: Some(session_source),
                session_provenance: None,
                thread_source: None,
                dynamic_tools: Vec::new(),
                metrics_service_name: None,
                parent_trace: None,
                environments: Vec::new(),
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
        config.validation.project_command = Some(command.clone());
    });
    builder.build_with_streaming_server(server).await
}

async fn build_validation_codex(
    server: &MockServer,
    command: ProjectValidationCommand,
) -> Result<TestCodex> {
    let mut builder = test_codex().with_config(move |config| {
        config.validation.project_command = Some(command.clone());
    });
    builder.build(server).await
}

async fn submit_user_input(codex: &CodexThread, test: &TestCodex, text: &str) -> Result<()> {
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.config.cwd.as_path());
    let session_model = test.session_configured.model.clone();
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            environments: None,
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: codex_protocol::protocol::ThreadSettingsOverrides {
                cwd: Some(test.config.cwd.clone()),
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

    assert!(validation_events(&events).is_empty());
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

    assert!(validation_events(&events).is_empty());
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
    let steering_result = submit_user_input(&test.codex, &test, "make the requested change").await;
    let _ = capture_release_tx.send(());
    capture_task
        .await
        .context("validation fingerprint capture task panicked")??;
    steering_result?;

    let events = collect_events_until_terminal(&test.codex).await?;

    assert!(test.workspace_path("ignored-by-git.txt").exists());
    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    assert_eq!(validation_events[0].status, ProjectValidationStatus::Passed);
    assert_eq!(validation_events[0].output, "validation-pass");
    assert_eq!(server.requests().await.len(), 3);
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
            config.validation.project_command = Some(command.clone());
        })
        .with_workspace_setup(|cwd, fs| async move {
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
    submit_user_input(&test.codex, &test, "also preserve the public API").await?;
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
    server
        .assert_request_count_stays(/*count*/ 2, Duration::from_millis(100))
        .await;
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
    assert_eq!(validation_events.len(), 1);
    assert_eq!(
        validation_events[0].status,
        ProjectValidationStatus::ActionableFailure
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

    assert!(validation_events(&events).is_empty());
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
