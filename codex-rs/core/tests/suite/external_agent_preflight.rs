use anyhow::Result;
use codex_config::config_toml::AgentSelectorToml;
use codex_core::config::AgentRoleBackendConfig;
use codex_core::config::AgentRoleConfig;
use codex_core::config::ExternalCommandAgentBackendConfig;
use codex_core::config::ExternalCommandProtocol;
use codex_features::Feature;
use codex_utils_output_truncation::approx_token_count;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace as raw_ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex_with_agents as test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

const PROMPT: &str = "probe the configured external agent";
const AGENT_MESSAGE: &str = "reply without changing files";
const SPAWN_CALL_ID: &str = "spawn-external-probe";
const WAIT_CALL_ID: &str = "wait-external-probe";
const LIST_CALL_ID: &str = "list-external-probe";
const MESSAGE_CALL_ID: &str = "message-external-probe";
const FOLLOWUP_CALL_ID: &str = "followup-external-probe";
const ROLE: &str = "external_probe";
const COLLABORATION_NAMESPACE: &str = "collaboration";
const FOLLOW_UP_PROMPT: &str = "summarize what the external agent reported";
/// The repeating unit of the stub external agent's ~200 KB final message.
const EXTERNAL_AGENT_OUTPUT_CHUNK: &str = "0123456789";

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    String::from_utf8_lossy(&request.body).contains(text)
}

fn ev_function_call_with_namespace(
    call_id: &str,
    namespace: &str,
    name: &str,
    arguments: &str,
) -> Value {
    let mut event = raw_ev_function_call_with_namespace(call_id, namespace, name, arguments);
    if namespace == COLLABORATION_NAMESPACE
        && matches!(name, "spawn_agent" | "send_message" | "followup_task")
    {
        event["item"]["encrypted_function_args"] = json!([]);
    }
    event
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
    spawn_agent_arguments_with_selector(Some(ROLE), /*model*/ None)
}

fn spawn_agent_arguments_with_selector(
    agent_type: Option<&str>,
    model: Option<&str>,
) -> Result<String> {
    spawn_agent_arguments_with_selector_and_effort(
        agent_type, model, /*reasoning_effort*/ None,
    )
}

fn spawn_agent_arguments_with_selector_and_effort(
    agent_type: Option<&str>,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
) -> Result<String> {
    Ok(serde_json::to_string(&json!({
        "message": AGENT_MESSAGE,
        "task_name": "external_probe",
        "task_kind": "other",
        "task_size": "normal",
        "agent_type": agent_type,
        "model": model,
        "reasoning_effort": reasoning_effort,
        "fork_turns": "none",
    }))?)
}

/// Configure a single explicit external role and the timeouts the spawn tools need.
fn builder_with_external_role(backend: ExternalCommandAgentBackendConfig) -> TestCodexBuilder {
    builder_with_named_external_role(ROLE, backend)
}

fn builder_with_multi_agent_v2() -> TestCodexBuilder {
    test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::Collab)
            .expect("test config should enable Collab");
        config
            .features
            .enable(Feature::MultiAgentV2)
            .expect("test config should enable MultiAgentV2");
    })
}

fn builder_with_named_external_role(
    role: &str,
    backend: ExternalCommandAgentBackendConfig,
) -> TestCodexBuilder {
    let role = role.to_string();
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
            role.clone(),
            AgentRoleConfig {
                description: Some("External agent preflight probe".to_string()),
                backend: Some(AgentRoleBackendConfig::ExternalCommand(backend)),
                ..Default::default()
            },
        );
    })
}

fn builder_for_dynamic_external_selector(
    backend: ExternalCommandAgentBackendConfig,
) -> TestCodexBuilder {
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
            "antigravity".to_string(),
            AgentRoleConfig {
                description: Some("Antigravity test backend".to_string()),
                backend: Some(AgentRoleBackendConfig::ExternalCommand(backend)),
                ..Default::default()
            },
        );
    })
}

async fn spawn_agent_output_with_selector(
    backend: ExternalCommandAgentBackendConfig,
    role: &str,
    agent_type: Option<&str>,
    model: Option<&str>,
) -> Result<String> {
    let server = start_mock_server().await;
    let arguments = spawn_agent_arguments_with_selector(agent_type, model)?;
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
                &arguments,
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

    let mut builder = builder_with_named_external_role(role, backend);
    let test = builder.build(&server).await?;
    test.submit_turn(PROMPT).await?;

    let output_item = final_response
        .single_request()
        .function_call_output(SPAWN_CALL_ID);
    Ok(output_item
        .get("output")
        .and_then(Value::as_str)
        .expect("spawn_agent output string")
        .to_string())
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
async fn external_aliases_in_model_field_route_to_external_agent_type() -> Result<()> {
    for (selector, canonical_agent_type) in [
        ("opus", "claude-opus-5"),
        ("claude-opus", "claude-opus-5"),
        ("gemini", "antigravity"),
        ("antigravity", "antigravity"),
    ] {
        let stub_dir = TempDir::new()?;
        let backend = stub_cli(
            &stub_dir,
            &format!("{selector}-provider.sh"),
            "printf 'external provider replied\\n'\n",
        );
        let output = spawn_agent_output_with_selector(
            backend,
            canonical_agent_type,
            /*agent_type*/ None,
            Some(selector),
        )
        .await?;
        let output: Value = serde_json::from_str(&output)?;

        assert_eq!(output.get("agent_type"), Some(&json!(canonical_agent_type)));
        assert_eq!(output.pointer("/routing/kind"), Some(&json!("explicit")));
        assert_eq!(
            output.pointer("/routing/requested"),
            Some(&json!(canonical_agent_type))
        );
        assert_eq!(
            output.pointer("/routing/effective"),
            Some(&json!(canonical_agent_type))
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_user_agent_requires_explicit_retry_and_receives_plaintext() -> Result<()> {
    const USER_PROMPT: &str = "Ask Opus to review this and do not substitute another agent.";
    const RETRY_CALL_ID: &str = "spawn-opus-explicit-retry";

    let stub_dir = TempDir::new()?;
    let captured_args = stub_dir.path().join("captured-args.txt");
    let script = format!(
        "if [ \"${{1:-}}\" = \"--version\" ]; then printf 'test-provider 1.0\\n'; exit 0; fi\nprintf '%s\\n' \"$@\" > '{}'\nprintf 'external provider replied\\n'\n",
        captured_args.display()
    );
    let backend = stub_cli(&stub_dir, "opus-provider.sh", &script);
    let server = start_mock_server().await;
    let omitted_selector_args = serde_json::to_string(&json!({
        "message": AGENT_MESSAGE,
        "task_name": "opus_review",
        "task_kind": "independent_review",
        "task_size": "large",
        "fork_turns": "none",
    }))?;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, USER_PROMPT) && !body_contains(request, SPAWN_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-omitted-selector"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                COLLABORATION_NAMESPACE,
                "spawn_agent",
                &omitted_selector_args,
            ),
            ev_completed("resp-omitted-selector"),
        ]),
    )
    .await;
    let retry_response = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, SPAWN_CALL_ID),
        sse(vec![
            ev_response_created("resp-explicit-retry"),
            ev_function_call_with_namespace(
                RETRY_CALL_ID,
                COLLABORATION_NAMESPACE,
                "spawn_agent",
                &spawn_agent_arguments_with_selector(/*agent_type*/ None, Some("opus"))?,
            ),
            ev_completed("resp-explicit-retry"),
        ]),
    )
    .await;
    let final_response = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, RETRY_CALL_ID),
        sse(vec![
            ev_response_created("resp-complete"),
            ev_assistant_message("msg-complete", "probe complete"),
            ev_completed("resp-complete"),
        ]),
    )
    .await;

    let mut builder = builder_with_named_external_role("claude-opus-5", backend);
    let test = builder.build(&server).await?;
    test.submit_turn(USER_PROMPT).await?;

    let omitted_output_item = retry_response
        .single_request()
        .function_call_output(SPAWN_CALL_ID);
    let omitted_output = omitted_output_item["output"]
        .as_str()
        .expect("omitted-selector output");
    assert!(omitted_output.contains("current user turn explicitly requests specific agents"));
    assert!(omitted_output.contains("claude-opus-5"));
    assert!(omitted_output.contains("Set `agent_type`"));

    let retry_output: Value = serde_json::from_str(
        final_response
            .single_request()
            .function_call_output(RETRY_CALL_ID)["output"]
            .as_str()
            .expect("explicit retry output"),
    )?;
    assert_eq!(retry_output["agent_type"], json!("claude-opus-5"));
    assert_eq!(retry_output["routing"]["kind"], json!("explicit"));
    assert_eq!(retry_output["routing"]["requested"], json!("claude-opus-5"));
    assert_eq!(retry_output["routing"]["effective"], json!("claude-opus-5"));

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !captured_args.exists() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let captured = std::fs::read_to_string(&captured_args)?;
    assert!(captured.contains(AGENT_MESSAGE));
    assert!(!captured.contains("gAAAA"));
    Ok(())
}

async fn assert_claude_effort_reaches_launch(
    configured_effort: &str,
    explicit_effort: Option<&str>,
    expected_effort: &str,
) -> Result<()> {
    let stub_dir = TempDir::new()?;
    let captured_args = stub_dir.path().join("captured-args.txt");
    let role_config = stub_dir.path().join("claude-role.toml");
    std::fs::write(
        &role_config,
        "developer_instructions = \"Run the bounded provenance probe.\"\n",
    )?;
    let script = format!(
        "if [ \"${{1:-}}\" = \"--version\" ]; then printf 'Claude Code 2.1.220\\n'; exit 0; fi\nif [ \"${{1:-}}\" = \"auth\" ] && [ \"${{2:-}}\" = \"status\" ]; then printf '{{\"loggedIn\":true}}\\n'; exit 0; fi\nif [ \"${{1:-}}\" = \"--help\" ]; then printf '%s\\n' '--effort <level>'; exit 0; fi\nprintf '%s\\n' \"$@\" > '{}'\nprintf 'CLAUDE_PROVENANCE_OK\\n'\n",
        captured_args.display()
    );
    let mut backend = stub_cli(&stub_dir, "claude-provider.sh", &script);
    backend.args = vec!["--effort".to_string(), "low".to_string()];
    backend.launch_family = Some("claude".to_string());

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
                &spawn_agent_arguments_with_selector_and_effort(
                    Some("claude-sonnet-4.6"),
                    /*model*/ None,
                    explicit_effort,
                )?,
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

    let mut builder = builder_with_named_external_role("claude-sonnet-4.6", backend);
    let configured_effort = configured_effort.to_string();
    builder = builder.with_config(move |config| {
        config.agent_selector_overrides.insert(
            "claude-sonnet-4.6".to_string(),
            AgentSelectorToml {
                effort: Some(configured_effort.clone()),
                ..Default::default()
            },
        );
        config
            .agent_roles
            .get_mut("claude-sonnet-4.6")
            .expect("test role should exist")
            .config_file = Some(role_config);
    });
    let test = builder.build(&server).await?;
    test.submit_turn(PROMPT).await?;

    let response = final_response.single_request();
    let output_item = response.function_call_output(SPAWN_CALL_ID);
    let output_text = output_item["output"].as_str().expect("spawn output");
    let output: Value = serde_json::from_str(output_text)
        .unwrap_or_else(|err| panic!("spawn output should be JSON ({err}): {output_text}"));
    assert_eq!(output["routing"]["effective"], "claude-sonnet-4.6");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !captured_args.exists() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let captured = std::fs::read_to_string(captured_args)?;
    let arguments = captured.lines().collect::<Vec<_>>();
    assert!(
        arguments
            .windows(2)
            .any(|args| args == ["--effort", expected_effort])
    );
    assert_eq!(
        arguments.iter().filter(|arg| **arg == "--effort").count(),
        1
    );
    assert!(!arguments.contains(&"low"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_claude_effort_reaches_launch_after_role_reload() -> Result<()> {
    assert_claude_effort_reaches_launch("medium", /*explicit_effort*/ None, "medium").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_claude_effort_overrides_default_and_reaches_launch() -> Result<()> {
    assert_claude_effort_reaches_launch("medium", Some("high"), "high").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_user_agent_rejects_explicit_substitution() -> Result<()> {
    const USER_PROMPT: &str = "Ask Opus to review this and do not substitute another agent.";

    let server = start_mock_server().await;
    let arguments = serde_json::to_string(&json!({
        "message": AGENT_MESSAGE,
        "task_name": "wrong_claude_review",
        "task_kind": "independent_review",
        "task_size": "large",
        "agent_type": "claude-sonnet-4.6",
        "fork_turns": "none",
    }))?;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, USER_PROMPT) && !body_contains(request, SPAWN_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-substitution"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                COLLABORATION_NAMESPACE,
                "spawn_agent",
                &arguments,
            ),
            ev_completed("resp-substitution"),
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

    let mut builder = builder_with_multi_agent_v2();
    let test = builder.build(&server).await?;
    test.submit_turn(USER_PROMPT).await?;

    let output_item = final_response
        .single_request()
        .function_call_output(SPAWN_CALL_ID);
    let output = output_item["output"].as_str().expect("substitution output");
    assert!(output.contains("explicitly requests specific agents: `claude-opus-5`"));
    assert!(output.contains("spawn selected `claude-sonnet-4.6`"));
    assert!(output.contains("do not substitute another agent"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_user_agent_cannot_be_selected_explicitly() -> Result<()> {
    const USER_PROMPT: &str = "Do not use Opus; use Sonnet for this review.";

    let server = start_mock_server().await;
    let arguments = spawn_agent_arguments_with_selector(Some("opus"), /*model*/ None)?;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, USER_PROMPT) && !body_contains(request, SPAWN_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-rejected-agent"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                COLLABORATION_NAMESPACE,
                "spawn_agent",
                &arguments,
            ),
            ev_completed("resp-rejected-agent"),
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

    let mut builder = builder_with_multi_agent_v2();
    let test = builder.build(&server).await?;
    test.submit_turn(USER_PROMPT).await?;

    let output_item = final_response
        .single_request()
        .function_call_output(SPAWN_CALL_ID);
    let output = output_item["output"]
        .as_str()
        .expect("rejected-agent output");
    assert!(output.contains("explicitly rejects `claude-opus-5`"));
    assert!(output.contains("Do not spawn that agent"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encrypted_external_agent_message_is_rejected_before_launch() -> Result<()> {
    let stub_dir = TempDir::new()?;
    let launched = stub_dir.path().join("launched.txt");
    let script = format!(
        "if [ \"${{1:-}}\" = \"--version\" ]; then printf 'test-provider 1.0\\n'; exit 0; fi\ntouch '{}'\nprintf 'must not run\\n'\n",
        launched.display()
    );
    let backend = stub_cli(&stub_dir, "encrypted-provider.sh", &script);
    let server = start_mock_server().await;
    let mut spawn_event = ev_function_call_with_namespace(
        SPAWN_CALL_ID,
        COLLABORATION_NAMESPACE,
        "spawn_agent",
        &spawn_agent_arguments()?,
    );
    spawn_event["item"]["encrypted_function_args"] = json!(["message"]);
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, PROMPT) && !body_contains(request, SPAWN_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-encrypted"),
            spawn_event,
            ev_completed("resp-encrypted"),
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
    let output = output_item["output"]
        .as_str()
        .expect("encrypted spawn output");
    assert!(output.contains("require plaintext task content"));
    assert!(!output.contains(AGENT_MESSAGE));
    assert!(!launched.exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conflicting_external_selectors_fail_without_substitution() -> Result<()> {
    let stub_dir = TempDir::new()?;
    let backend = stub_cli(
        &stub_dir,
        "antigravity-provider.sh",
        "printf 'must not run\\n'\n",
    );
    let output =
        spawn_agent_output_with_selector(backend, "antigravity", Some("antigravity"), Some("opus"))
            .await?;

    assert!(output.contains("use one explicit agent selector"));
    assert!(output.contains("claude-opus-5"));
    assert!(output.contains("antigravity"));

    let native_conflict = spawn_agent_output_with_selector(
        stub_cli(&stub_dir, "opus-provider.sh", "printf 'must not run\\n'\n"),
        "opus",
        Some("opus"),
        Some("gpt-5.6-sol"),
    )
    .await?;
    assert!(native_conflict.contains("cannot be combined with native model override"));
    Ok(())
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
async fn unsupported_antigravity_model_fails_explicit_preflight() -> Result<()> {
    let stub_dir = TempDir::new()?;
    let mut backend = stub_cli(
        &stub_dir,
        "fake-agy.sh",
        r#"if [ "$1" = "--version" ]; then
  echo "1.1.9"
  exit 0
fi
if [ "$1" = "models" ]; then
  echo "gemini-3.6-flash-high"
  exit 0
fi
if [ "$1" = "--help" ]; then
  echo '--model Model'
  echo '--effort low|medium|high'
  exit 0
fi
exit 2
"#,
    );
    backend.launch_family = Some("antigravity".to_string());
    backend.args = vec!["--model".to_string(), "gemini-missing".to_string()];

    let output = spawn_agent_output(backend).await?;

    assert_eq!(
        output,
        format!(
            "Explicit external agent `{ROLE}` failed `unsupported_mode` preflight: antigravity model `gemini-missing` was not reported by the installed CLI. Available models: gemini-3.6-flash-high"
        )
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_antigravity_selector_is_rejected_before_launch_without_substitution() -> Result<()> {
    let stub_dir = TempDir::new()?;
    let launched = stub_dir.path().join("launched");
    let mut backend = stub_cli(
        &stub_dir,
        "fake-agy.sh",
        &format!(
            r#"if [ "$1" = "--version" ]; then
  echo "1.1.9"
  exit 0
fi
if [ "$1" = "models" ]; then
  echo "gemini-3.6-flash-high"
  exit 0
fi
if [ "$1" = "--help" ]; then
  echo '--model Model'
  echo '--effort low|medium|high'
  exit 0
fi
touch '{}'
exit 0
"#,
            launched.display()
        ),
    );
    backend.launch_family = Some("antigravity".to_string());

    let output = spawn_agent_output_with_selector(
        backend,
        "antigravity",
        Some("antigravity-gemini-missing"),
        /*model*/ None,
    )
    .await?;

    assert!(output.contains("Antigravity selector `antigravity-gemini-missing`"));
    assert!(output.contains("Rule:"));
    assert!(output.contains("Remediation:"));
    assert!(output.contains("no provider or model was substituted"));
    assert!(!launched.exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovered_antigravity_selector_routes_before_preflight_and_launches_exactly() -> Result<()>
{
    let stub_dir = TempDir::new()?;
    let agy = stub_dir.path().join("agy");
    let args_log = stub_dir.path().join("agy-args.log");
    std::fs::write(
        &agy,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if [ "$1" = "--version" ]; then
  echo "1.1.10"
  exit 0
fi
if [ "$1" = "models" ]; then
  echo "gemini-3.6-flash-high"
  exit 0
fi
if [ "$1" = "--help" ]; then
  echo '--model Model'
  echo '--effort low|medium|high'
  exit 0
fi
printf 'external provider replied\n'
"#,
            args_log.display()
        ),
    )?;
    let mut permissions = std::fs::metadata(&agy)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&agy, permissions)?;
    let backend = ExternalCommandAgentBackendConfig {
        command: agy.display().to_string(),
        protocol: ExternalCommandProtocol::RawCli,
        launch_family: Some("antigravity".to_string()),
        ..Default::default()
    };

    let server = start_mock_server().await;
    let selector = "antigravity-gemini-3.6-flash-high";
    let spawn_arguments = serde_json::to_string(&json!({
        "message": AGENT_MESSAGE,
        "task_name": "external_probe",
        "task_kind": "other",
        "task_size": "normal",
        "model": selector,
        "reasoning_effort": "high",
        "fork_turns": "none",
    }))?;
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
                &spawn_arguments,
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
    let final_response = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, WAIT_CALL_ID),
        sse(vec![
            ev_response_created("resp-complete"),
            ev_assistant_message("msg-complete", "probe complete"),
            ev_completed("resp-complete"),
        ]),
    )
    .await;

    let mut builder = builder_for_dynamic_external_selector(backend);
    let test = builder.build(&server).await?;
    test.submit_turn(PROMPT).await?;

    let request = final_response.single_request();
    let spawn_output = request.function_call_output(SPAWN_CALL_ID)["output"]
        .as_str()
        .expect("spawn output string")
        .to_string();
    let spawn_output: Value = serde_json::from_str(&spawn_output)
        .unwrap_or_else(|err| panic!("spawn output should be JSON ({err}): {spawn_output}"));
    assert_eq!(spawn_output["agent_type"], selector);
    assert_eq!(spawn_output["routing"]["effective"], selector);
    assert_eq!(spawn_output["routing"]["kind"], "explicit");
    let wait_output_item = request.function_call_output(WAIT_CALL_ID);
    let wait_output = wait_output_item["output"]
        .as_str()
        .expect("wait output string");
    let wait_output = serde_json::from_str::<Value>(wait_output)?;
    assert_eq!(wait_output["timed_out"], false);
    assert!(matches!(
        wait_output["message"].as_str(),
        Some("Wait completed." | "All child agents are terminal.")
    ));

    let logged_args = std::fs::read_to_string(args_log)?;
    assert!(logged_args.contains("--model gemini-3.6-flash-high"));
    assert!(logged_args.contains("--effort high"));
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
            "supports_followup_messages": false,
            "provider": {
                "agent_type": ROLE,
                "command": "sh",
                "protocol": "raw_cli",
                "mode": "write",
                "workspace": workspace,
                "capability_source": "not_probed",
                "capability_freshness": "fresh",
            },
        })
    );

    Ok(())
}

#[derive(Clone, Copy)]
enum QuotaMetadataExposure {
    Visible,
    Hidden,
}

async fn claude_stream_quota_agent_status(exposure: QuotaMetadataExposure) -> Result<Value> {
    let stub_dir = TempDir::new()?;
    let fixture = include_str!("../../src/agent/fixtures/claude_stream/five_hour_rejected.jsonl");
    let script = format!(
        r#"if [ "${{1:-}}" = "--version" ]; then printf 'Claude Code 2.1.220\n'; exit 0; fi
if [ "${{1:-}}" = "auth" ] && [ "${{2:-}}" = "status" ]; then printf '{{"loggedIn":true}}\n'; exit 0; fi
if [ "${{1:-}}" = "--help" ]; then printf '%s\n' '--model <model>' '--verbose' '--output-format <format>'; exit 0; fi
case " $* " in
  *" --verbose --output-format stream-json "*) ;;
  *) printf 'missing stream-json flags\n' >&2; exit 2 ;;
esac
cat <<'EOF'
{fixture}
EOF
exit 1
"#
    );
    let mut backend = stub_cli(&stub_dir, "fake-claude.sh", &script);
    backend.launch_family = Some("claude".to_string());

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

    let mut builder = match exposure {
        QuotaMetadataExposure::Visible => builder_with_external_role(backend),
        QuotaMetadataExposure::Hidden => {
            builder_with_external_role(backend).with_config(|config| {
                config.multi_agent_v2.hide_spawn_agent_metadata = true;
            })
        }
    };
    let test = builder.build(&server).await?;
    test.submit_turn(PROMPT).await?;

    let output_item = final_response
        .single_request()
        .function_call_output(LIST_CALL_ID);
    let listed = serde_json::from_str::<Value>(
        output_item["output"]
            .as_str()
            .expect("list_agents output string"),
    )?;
    let mut agent = listed["agents"]
        .as_array()
        .and_then(|agents| {
            agents
                .iter()
                .find(|agent| agent["agent_name"] == "/root/external_probe")
        })
        .expect("external agent should be listed")
        .clone();
    agent
        .as_object_mut()
        .expect("listed agent object")
        .remove("duration_ms")
        .expect("completed external agent should report a duration");

    Ok(agent)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_stream_quota_diagnostic_is_authoritative_in_agent_status() -> Result<()> {
    let agent = claude_stream_quota_agent_status(QuotaMetadataExposure::Visible).await?;

    assert_eq!(
        agent["agent_status"]["errored"].as_str(),
        Some(
            "External agent rate limit status rejected: five_hour window; resets at 2026-07-12T04:20:00+00:00; paid overage rejected (reason: org_level_disabled_until); using paid overage: false."
        )
    );
    assert_eq!(agent["failure"]["kind"], "quota_or_rate_limited");
    assert_eq!(
        agent["quota_diagnostic"],
        json!({
            "status": "rejected",
            "window": "five_hour",
            "resets_at": 1_783_830_000,
            "overage_state": "rejected",
            "overage_reason": "org_level_disabled_until",
            "is_using_overage": false,
        })
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_stream_quota_diagnostic_is_redacted_with_provider_metadata() -> Result<()> {
    let agent = claude_stream_quota_agent_status(QuotaMetadataExposure::Hidden).await?;

    assert_eq!(
        agent,
        json!({
            "agent_name": "/root/external_probe",
            "agent_status": { "errored": "external agent failed: quota_or_rate_limited" },
            "supports_followup_messages": false,
            "failure": { "kind": "quota_or_rate_limited" },
        })
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_external_agent_rejects_followup_without_cancellation() -> Result<()> {
    let stub_dir = TempDir::new()?;
    let release = stub_dir.path().join("release.txt");
    let backend = stub_cli(
        &stub_dir,
        "long-running-provider.sh",
        &format!(
            "while [ ! -f '{}' ]; do sleep 0.05; done\nprintf 'external provider replied\\n'\n",
            release.display()
        ),
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
            body_contains(request, SPAWN_CALL_ID) && !body_contains(request, MESSAGE_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-message"),
            ev_function_call_with_namespace(
                MESSAGE_CALL_ID,
                COLLABORATION_NAMESPACE,
                "send_message",
                &serde_json::to_string(&json!({
                    "target": "/root/external_probe",
                    "message": "please follow up",
                }))?,
            ),
            ev_completed("resp-message"),
        ]),
    )
    .await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, MESSAGE_CALL_ID) && !body_contains(request, FOLLOWUP_CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-followup"),
            ev_function_call_with_namespace(
                FOLLOWUP_CALL_ID,
                COLLABORATION_NAMESPACE,
                "followup_task",
                &serde_json::to_string(&json!({
                    "target": "/root/external_probe",
                    "message": "perform another task",
                }))?,
            ),
            ev_completed("resp-followup"),
        ]),
    )
    .await;
    responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, FOLLOWUP_CALL_ID) && !body_contains(request, LIST_CALL_ID)
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
    test.submit_turn(PROMPT).await?;

    let request = final_response.single_request();
    let spawn_output_item = request.function_call_output(SPAWN_CALL_ID);
    let spawn_output = spawn_output_item["output"]
        .as_str()
        .expect("spawn output string");
    let spawn_output: Value = serde_json::from_str(spawn_output)?;
    assert_eq!(spawn_output["supports_followup_messages"], false);

    let message_output_item = request.function_call_output(MESSAGE_CALL_ID);
    let message_output = message_output_item["output"]
        .as_str()
        .expect("message output string");
    assert!(message_output.contains("do not support follow-up messages"));
    assert!(!message_output.contains("not found"));
    assert!(!message_output.contains("collab manager unavailable"));

    let followup_output_item = request.function_call_output(FOLLOWUP_CALL_ID);
    let followup_output = followup_output_item["output"]
        .as_str()
        .expect("follow-up output string");
    assert!(followup_output.contains("do not support follow-up messages"));
    assert!(!followup_output.contains("not found"));
    assert!(!followup_output.contains("collab manager unavailable"));

    let list_output_item = request.function_call_output(LIST_CALL_ID);
    let list_output = list_output_item["output"]
        .as_str()
        .expect("list output string");
    let listed: Value = serde_json::from_str(list_output)?;
    let external_agent = listed["agents"]
        .as_array()
        .and_then(|agents| {
            agents
                .iter()
                .find(|agent| agent["agent_name"] == "/root/external_probe")
        })
        .unwrap_or_else(|| panic!("external agent should remain listed: {listed}"));
    assert_eq!(external_agent["agent_status"], "running");
    assert_eq!(external_agent["supports_followup_messages"], false);

    std::fs::write(release, "release")?;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
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
