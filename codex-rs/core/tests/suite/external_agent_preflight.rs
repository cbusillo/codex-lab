use anyhow::Result;
use codex_core::config::AgentRoleBackendConfig;
use codex_core::config::AgentRoleConfig;
use codex_core::config::ExternalCommandAgentBackendConfig;
use codex_core::config::ExternalCommandProtocol;
use codex_features::Feature;
use codex_utils_output_truncation::approx_token_count;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;

const PROMPT: &str = "probe the configured external agent";
const AGENT_MESSAGE: &str = "reply without changing files";
const SPAWN_CALL_ID: &str = "spawn-external-probe";
const WAIT_CALL_ID: &str = "wait-external-probe";
const LIST_CALL_ID: &str = "list-external-probe";
const ROLE: &str = "external_probe";
const COLLABORATION_NAMESPACE: &str = "collaboration";
const FOLLOW_UP_PROMPT: &str = "summarize what the external agent reported";
/// The repeating unit of the stub external agent's ~200 KB final message.
const EXTERNAL_AGENT_OUTPUT_CHUNK: &str = "0123456789";

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    String::from_utf8_lossy(&request.body).contains(text)
}

/// Every text span an input item carries, whatever its role or content type.
fn input_item_texts(item: &Value) -> Vec<String> {
    item.get("content")
        .and_then(Value::as_array)
        .map(|content| {
            content
                .iter()
                .filter_map(|span| span.get("text").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn spawn_agent_arguments() -> Result<String> {
    Ok(serde_json::to_string(&json!({
        "message": AGENT_MESSAGE,
        "task_name": "external_probe",
        "task_kind": "other",
        "task_size": "normal",
        "agent_type": ROLE,
        "fork_turns": "none",
    }))?)
}

/// Configure a single explicit external role and the timeouts the spawn tools need.
fn builder_with_external_role(backend: ExternalCommandAgentBackendConfig) -> TestCodexBuilder {
    test_codex().with_config(move |config| {
        config
            .features
            .enable(Feature::Collab)
            .expect("test config should enable Collab");
        config
            .features
            .enable(Feature::MultiAgentV2)
            .expect("test config should enable MultiAgentV2");
        config.multi_agent_v2.hide_spawn_agent_metadata = false;
        config.multi_agent_v2.min_wait_timeout_ms = 10;
        config.multi_agent_v2.max_wait_timeout_ms = 5_000;
        config.multi_agent_v2.default_wait_timeout_ms = 2_000;
        config.agent_roles.insert(
            ROLE.to_string(),
            AgentRoleConfig {
                description: Some("External agent preflight probe".to_string()),
                backend: Some(AgentRoleBackendConfig::ExternalCommand(backend)),
                ..Default::default()
            },
        );
    })
}

/// Drive one turn that spawns the explicit external role and return the
/// `spawn_agent` output the model saw.
async fn spawn_agent_output(backend: ExternalCommandAgentBackendConfig) -> Result<String> {
    let server = start_mock_server().await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, PROMPT) && !body_contains(request, SPAWN_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-spawn"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                COLLABORATION_NAMESPACE,
                "spawn_agent",
                &spawn_agent_arguments()?,
            ),
            ev_completed("resp-spawn"),
        ]),
    )
    .await;
    let final_response = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, SPAWN_CALL_ID),
        sse(vec![
            ev_response_created("resp-complete"),
            ev_assistant_message("msg-complete", "probe complete"),
            ev_completed("resp-complete"),
        ]),
    )
    .await;

    let mut builder = builder_with_external_role(backend);
    let test = builder.build(&server).await?;

    test.submit_turn(PROMPT).await?;

    let output_item = final_response
        .single_request()
        .function_call_output(SPAWN_CALL_ID);
    let output = output_item
        .get("output")
        .and_then(Value::as_str)
        .expect("spawn_agent output string");
    Ok(output.to_string())
}

/// Write a `/bin/sh` stub the external backend can run without touching a real
/// provider CLI.
fn stub_cli(dir: &TempDir, name: &str, script: &str) -> ExternalCommandAgentBackendConfig {
    let path = dir.path().join(name);
    std::fs::write(&path, script).expect("stub CLI should be written");
    ExternalCommandAgentBackendConfig {
        command: format!("/bin/sh {}", path.display()),
        protocol: ExternalCommandProtocol::RawCli,
        timeout_ms: 5_000,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_copilot_executable_fails_explicit_preflight() -> Result<()> {
    let command = std::env::current_exe()?;
    let resolved_command = which::which(&command)?;
    let expected_error = format!(
        "GitHub Copilot CLI command `{}` resolved to a different `copilot` executable. Install GitHub Copilot CLI and ensure its `copilot` command appears first on PATH.",
        resolved_command.display()
    );

    let output = spawn_agent_output(ExternalCommandAgentBackendConfig {
        command: command.display().to_string(),
        protocol: ExternalCommandProtocol::RawCli,
        launch_family: Some("copilot".to_string()),
        timeout_ms: 5_000,
        ..Default::default()
    })
    .await?;

    assert_eq!(
        output,
        format!(
            "Explicit external agent `{ROLE}` failed `launch_failed` preflight: {expected_error}"
        )
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_external_agent_command_fails_explicit_preflight_with_install_hint() -> Result<()> {
    let output = spawn_agent_output(ExternalCommandAgentBackendConfig {
        command: "definitely-missing-claude-code-test-command".to_string(),
        protocol: ExternalCommandProtocol::RawCli,
        launch_family: Some("claude".to_string()),
        timeout_ms: 5_000,
        ..Default::default()
    })
    .await?;

    assert_eq!(
        output,
        format!(
            "Explicit external agent `{ROLE}` failed `command_missing` preflight: Claude Code command `definitely-missing-claude-code-test-command` was not found or is not executable. Install claude-code and make sure `definitely-missing-claude-code-test-command` is on PATH."
        )
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logged_out_claude_agent_fails_explicit_preflight_as_authentication_required() -> Result<()>
{
    let stub_dir = TempDir::new()?;
    let mut backend = stub_cli(
        &stub_dir,
        "fake-claude.sh",
        r#"if [ "$1" = "--version" ]; then
  echo "Claude Code 2.1.212"
  exit 0
fi
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  echo '{"loggedIn":false}'
  exit 0
fi
exit 2
"#,
    );
    backend.launch_family = Some("claude".to_string());

    let output = spawn_agent_output(backend).await?;

    assert_eq!(
        output,
        format!(
            "Explicit external agent `{ROLE}` failed `authentication_required` preflight: Claude Code authentication preflight failed: {{\"loggedIn\":false}}"
        )
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_command_agent_routes_through_spawn_agent_with_provider_provenance() -> Result<()>
{
    let stub_dir = TempDir::new()?;
    let backend = stub_cli(
        &stub_dir,
        "fake-provider.sh",
        "printf 'external provider replied\\n'\n",
    );

    let server = start_mock_server().await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, PROMPT) && !body_contains(request, SPAWN_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-spawn"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                COLLABORATION_NAMESPACE,
                "spawn_agent",
                &spawn_agent_arguments()?,
            ),
            ev_completed("resp-spawn"),
        ]),
    )
    .await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, SPAWN_CALL_ID) && !body_contains(request, WAIT_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-wait"),
            ev_function_call_with_namespace(
                WAIT_CALL_ID,
                COLLABORATION_NAMESPACE,
                "wait_agent",
                &serde_json::to_string(&json!({ "timeout_ms": 5_000 }))?,
            ),
            ev_completed("resp-wait"),
        ]),
    )
    .await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, WAIT_CALL_ID) && !body_contains(request, LIST_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-list"),
            ev_function_call_with_namespace(
                LIST_CALL_ID,
                COLLABORATION_NAMESPACE,
                "list_agents",
                "{}",
            ),
            ev_completed("resp-list"),
        ]),
    )
    .await;
    let final_response = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, LIST_CALL_ID),
        sse(vec![
            ev_response_created("resp-complete"),
            ev_assistant_message("msg-complete", "probe complete"),
            ev_completed("resp-complete"),
        ]),
    )
    .await;

    let mut builder = builder_with_external_role(backend);
    let test = builder.build(&server).await?;
    let workspace = test.cwd_path().display().to_string();

    test.submit_turn(PROMPT).await?;

    let output_item = final_response
        .single_request()
        .function_call_output(LIST_CALL_ID);
    let output = output_item
        .get("output")
        .and_then(Value::as_str)
        .expect("list_agents output string");
    let listed: Value = serde_json::from_str(output)?;

    let mut agent = listed
        .get("agents")
        .and_then(Value::as_array)
        .and_then(|agents| {
            agents
                .iter()
                .find(|agent| agent.get("agent_name") == Some(&json!("/root/external_probe")))
        })
        .cloned()
        .unwrap_or_else(|| panic!("external agent should be listed: {listed}"));
    // Wall-clock duration is not reproducible; the rest of the record is.
    agent
        .as_object_mut()
        .expect("listed agent object")
        .remove("duration_ms")
        .expect("completed external agent should report a duration");

    assert_eq!(
        agent,
        json!({
            "agent_name": "/root/external_probe",
            "agent_status": { "completed": "external provider replied" },
            "provider": {
                "agent_type": ROLE,
                "command": "sh",
                "protocol": "raw_cli",
                "mode": "write",
                "workspace": workspace,
            },
        })
    );

    Ok(())
}

/// Hard cap for a whole completion envelope, mirrored from
/// `codex_core::session_prefix::COMPLETION_MESSAGE_MAX_TOKENS`. The agent-authored payload is
/// truncated to a 900-token budget; the shared truncator's marker can push the rendered result a
/// few tokens past that, which is what the 100-token envelope reserve exists for.
const COMPLETION_MESSAGE_MAX_TOKENS: usize = 1_000;

fn completion_payload(agent_status: &Value) -> &str {
    agent_status
        .get("completed")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected a completed agent status: {agent_status}"))
}

/// An external agent's final message is process-authored and unbounded. It reaches the model
/// through the `list_agents` status payload and through the inter-agent completion notification,
/// so both paths carry the completion-payload budget. (The v2 `wait_agent` output is a fixed
/// string and carries no payload; the v1 `wait_agent` output is bounded through the same
/// `bounded_status` helper.)
///
/// The notification is queued for the parent rather than injected into the turn that spawned the
/// agent, so proving that path needs a second turn; asserting inside the first one only ever
/// proves that a message which has not been delivered yet is absent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_external_agent_completion_is_bounded_in_wait_and_list_outputs() -> Result<()> {
    let stub_dir = TempDir::new()?;
    // ~200 KB of final message, far past any per-item model-context budget.
    let backend = stub_cli(
        &stub_dir,
        "fake-verbose-provider.sh",
        "i=0\nwhile [ $i -lt 20000 ]; do printf '0123456789'; i=$((i+1)); done\nprintf '\\n'\n",
    );

    let server = start_mock_server().await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, PROMPT) && !body_contains(request, SPAWN_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-spawn"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                COLLABORATION_NAMESPACE,
                "spawn_agent",
                &spawn_agent_arguments()?,
            ),
            ev_completed("resp-spawn"),
        ]),
    )
    .await;
    let _wait_response = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, SPAWN_CALL_ID) && !body_contains(request, WAIT_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-wait"),
            ev_function_call_with_namespace(
                WAIT_CALL_ID,
                COLLABORATION_NAMESPACE,
                "wait_agent",
                &serde_json::to_string(&json!({ "timeout_ms": 5_000 }))?,
            ),
            ev_completed("resp-wait"),
        ]),
    )
    .await;
    let list_response = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, WAIT_CALL_ID) && !body_contains(request, LIST_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-list"),
            ev_function_call_with_namespace(
                LIST_CALL_ID,
                COLLABORATION_NAMESPACE,
                "list_agents",
                "{}",
            ),
            ev_completed("resp-list"),
        ]),
    )
    .await;
    let final_response = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, LIST_CALL_ID),
        sse(vec![
            ev_response_created("resp-complete"),
            ev_assistant_message("msg-complete", "probe complete"),
            ev_completed("resp-complete"),
        ]),
    )
    .await;

    let mut builder = builder_with_external_role(backend);
    let test = Box::pin(builder.build(&server)).await?;

    test.submit_turn(PROMPT).await?;

    let wait_output: Value = serde_json::from_str(
        list_response
            .single_request()
            .function_call_output(WAIT_CALL_ID)
            .get("output")
            .and_then(Value::as_str)
            .expect("wait_agent output string"),
    )?;
    assert_eq!(
        wait_output,
        json!({ "message": "Wait completed.", "timed_out": false })
    );

    let listed: Value = serde_json::from_str(
        final_response
            .single_request()
            .function_call_output(LIST_CALL_ID)
            .get("output")
            .and_then(Value::as_str)
            .expect("list_agents output string"),
    )?;
    let listed_agent = listed
        .get("agents")
        .and_then(Value::as_array)
        .and_then(|agents| {
            agents
                .iter()
                .find(|agent| agent.get("agent_name") == Some(&json!("/root/external_probe")))
        })
        .unwrap_or_else(|| panic!("external agent should be listed: {listed}"));
    let listed_payload = completion_payload(
        listed_agent
            .get("agent_status")
            .expect("listed agent status"),
    );
    assert!(
        approx_token_count(listed_payload) <= COMPLETION_MESSAGE_MAX_TOKENS,
        "list_agents completion payload was {} tokens",
        approx_token_count(listed_payload)
    );

    // The second path the unbounded final message reaches the model: the external agent's
    // completion is queued for the parent and delivered on its *next* turn. It has to be driven,
    // because any assertion made inside this turn is vacuous -- the notification is not there yet.
    let follow_up = responses::mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-follow-up"),
            ev_assistant_message("msg-follow-up", "follow-up complete"),
            ev_completed("resp-follow-up"),
        ]),
    )
    .await;

    test.submit_turn(FOLLOW_UP_PROMPT).await?;

    let delivered = follow_up
        .single_request()
        .input()
        .iter()
        .flat_map(input_item_texts)
        .find(|text| text.contains(EXTERNAL_AGENT_OUTPUT_CHUNK))
        .expect("the parent thread should receive the external agent's completion notification");
    assert!(
        approx_token_count(&delivered) <= COMPLETION_MESSAGE_MAX_TOKENS,
        "inter-agent completion notification was {} tokens",
        approx_token_count(&delivered)
    );

    Ok(())
}
