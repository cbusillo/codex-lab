use anyhow::Result;
use codex_core::config::AgentRoleBackendConfig;
use codex_core::config::AgentRoleConfig;
use codex_core::config::ExternalCommandAgentBackendConfig;
use codex_core::config::ExternalCommandProtocol;
use codex_features::Feature;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

const PROMPT: &str = "probe the configured Copilot agent";
const AGENT_MESSAGE: &str = "reply without changing files";
const SPAWN_CALL_ID: &str = "spawn-copilot-probe";
const WAIT_CALL_ID: &str = "wait-copilot-probe";
const LIST_CALL_ID: &str = "list-copilot-probe";

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    String::from_utf8_lossy(&request.body).contains(text)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_copilot_executable_surfaces_actionable_agent_status() -> Result<()> {
    let server = start_mock_server().await;
    let spawn_args = serde_json::to_string(&json!({
        "message": AGENT_MESSAGE,
        "task_name": "copilot_probe",
        "task_kind": "other",
        "task_size": "normal",
        "agent_type": "copilot_probe",
        "fork_turns": "none",
    }))?;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, PROMPT) && !body_contains(request, SPAWN_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-copilot-spawn"),
            ev_function_call(SPAWN_CALL_ID, "spawn_agent", &spawn_args),
            ev_completed("resp-copilot-spawn"),
        ]),
    )
    .await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, SPAWN_CALL_ID) && !body_contains(request, WAIT_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-copilot-wait"),
            ev_function_call(WAIT_CALL_ID, "wait_agent", r#"{"timeout_ms":100}"#),
            ev_completed("resp-copilot-wait"),
        ]),
    )
    .await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, WAIT_CALL_ID) && !body_contains(request, LIST_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-copilot-list"),
            ev_function_call(LIST_CALL_ID, "list_agents", "{}"),
            ev_completed("resp-copilot-list"),
        ]),
    )
    .await;
    let final_response = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, LIST_CALL_ID),
        sse(vec![
            ev_response_created("resp-copilot-complete"),
            ev_assistant_message("msg-copilot-complete", "probe complete"),
            ev_completed("resp-copilot-complete"),
        ]),
    )
    .await;

    let command = std::env::current_exe()?;
    let resolved_command = which::which(&command)?;
    let expected_error = format!(
        "GitHub Copilot CLI command `{}` resolved to a different `copilot` executable. Install GitHub Copilot CLI and ensure its `copilot` command appears first on PATH.",
        resolved_command.display()
    );
    let command = command.display().to_string();
    let mut builder = test_codex().with_config(move |config| {
        config
            .features
            .enable(Feature::MultiAgentV2)
            .expect("test config should enable MultiAgentV2");
        config.multi_agent_v2.hide_spawn_agent_metadata = false;
        config.multi_agent_v2.min_wait_timeout_ms = 10;
        config.multi_agent_v2.max_wait_timeout_ms = 1_000;
        config.multi_agent_v2.default_wait_timeout_ms = 100;
        config.agent_roles.insert(
            "copilot_probe".to_string(),
            AgentRoleConfig {
                description: Some("Wrong Copilot executable probe".to_string()),
                backend: Some(AgentRoleBackendConfig::ExternalCommand(
                    ExternalCommandAgentBackendConfig {
                        command: command,
                        protocol: ExternalCommandProtocol::RawCli,
                        launch_family: Some("copilot".to_string()),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            },
        );
    });
    let test = builder.build(&server).await?;

    test.submit_turn(PROMPT).await?;

    let output_item = final_response
        .single_request()
        .function_call_output(LIST_CALL_ID);
    let output = output_item
        .get("output")
        .and_then(Value::as_str)
        .expect("list_agents output string");
    let output: Value = serde_json::from_str(output)?;
    let agent = output["agents"]
        .as_array()
        .and_then(|agents| {
            agents
                .iter()
                .find(|agent| agent["agent_name"] == "/root/copilot_probe")
        })
        .expect("Copilot probe agent");
    assert_eq!(
        agent,
        &json!({
            "agent_name": "/root/copilot_probe",
            "agent_status": {"errored": expected_error},
            "last_task_message": AGENT_MESSAGE,
        })
    );

    Ok(())
}
