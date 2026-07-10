#![cfg(unix)]

use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use codex_config::MAX_PROJECT_VALIDATION_TIMEOUT_MS;
use codex_config::ProjectValidationCommand;
use codex_core::StartThreadOptions;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ProjectValidationCompletedEvent;
use codex_protocol::protocol::ProjectValidationStatus;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse_completed;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use tempfile::tempdir;
use tokio::time::timeout;

async fn run_validation_turn(command: ProjectValidationCommand) -> Result<Vec<EventMsg>> {
    run_validation_turn_with_source(command, None).await
}

async fn run_validation_turn_with_source(
    command: ProjectValidationCommand,
    session_source: Option<SessionSource>,
) -> Result<Vec<EventMsg>> {
    let server = start_mock_server().await;
    let _response_mock = mount_sse_once(&server, sse_completed("resp-1")).await;
    let mut builder = test_codex().with_config(move |config| {
        config.validation.project_command = Some(command.clone());
    });
    let test = builder.build(&server).await?;
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

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "finish the work".to_string(),
                text_elements: Vec::new(),
            }],
            environments: None,
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;

    let mut events = Vec::new();
    loop {
        let event = timeout(Duration::from_secs(10), codex.next_event())
            .await
            .context("timed out waiting for validation turn events")??;
        let turn_complete = matches!(event.msg, EventMsg::TurnComplete(_));
        events.push(event.msg);
        if turn_complete {
            break;
        }
    }
    Ok(events)
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
    ProjectValidationCommand {
        command: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
        timeout_ms,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_passes_once_before_turn_completion() -> Result<()> {
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
async fn project_validation_reports_bounded_actionable_failure() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let script = concat!(
        "printf 'validation-start\\n'; ",
        "i=0; while [ $i -lt 12000 ]; do printf x; i=$((i + 1)); done; ",
        "printf '\\nvalidation-end\\n' >&2; exit 7"
    );
    let events = run_validation_turn(shell_command(script, 5_000)).await?;
    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    let event = validation_events[0];
    assert_eq!(event.status, ProjectValidationStatus::ActionableFailure);
    assert_eq!(event.exit_code, Some(7));
    assert!(event.output_truncated);
    assert!(event.output.len() <= 8 * 1024);
    assert!(event.output.contains("project validation output truncated"));
    assert!(event.output.starts_with("validation-start"));
    assert!(event.output.ends_with("validation-end\n"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_validation_reports_empty_command_as_configuration_error() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let events = run_validation_turn(ProjectValidationCommand::default()).await?;
    let validation_events = validation_events(&events);
    assert_eq!(validation_events.len(), 1);
    let event = validation_events[0];
    assert_eq!(event.status, ProjectValidationStatus::ConfigurationError);
    assert_eq!(event.exit_code, None);
    assert!(event.output.contains("non-empty executable"));
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
