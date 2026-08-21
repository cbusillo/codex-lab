use anyhow::Result;
use codex_features::Feature;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::EventMsg;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;

const FIRST_PROMPT: &str = "spawn the first worker";
const FIRST_TASK: &str = "first worker task";
const SECOND_TASK: &str = "second worker task";
const MULTI_AGENT_V2_NAMESPACE: &str = "agents";

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(&request.body)
        .is_ok_and(|body| body.to_string().contains(text))
}

fn has_function_call_output(request: &wiremock::Request, call_id: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(&request.body).is_ok_and(|body| {
        body.get("input")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("type").and_then(serde_json::Value::as_str)
                        == Some("function_call_output")
                        && item.get("call_id").and_then(serde_json::Value::as_str) == Some(call_id)
                })
            })
    })
}

async fn mount_root_collaboration_call(
    server: &wiremock::MockServer,
    prompt: &'static str,
    call_id: &'static str,
    tool_name: &'static str,
    arguments: serde_json::Value,
) {
    let response_id = format!("resp-{call_id}");
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| body_contains(request, prompt),
        sse(vec![
            ev_response_created(&response_id),
            ev_function_call_with_namespace(
                call_id,
                MULTI_AGENT_V2_NAMESPACE,
                tool_name,
                &arguments.to_string(),
            ),
            ev_completed(&response_id),
        ]),
    )
    .await;

    let completion_id = format!("resp-{call_id}-complete");
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| has_function_call_output(request, call_id),
        sse(vec![
            ev_response_created(&completion_id),
            ev_assistant_message(&format!("msg-{call_id}"), "collaboration completed"),
            ev_completed(&completion_id),
        ]),
    )
    .await;
}

async fn mount_completed_worker(
    server: &wiremock::MockServer,
    task: &'static str,
    parent_call_id: &'static str,
) -> ResponseMock {
    let response_id = format!("resp-worker-{parent_call_id}");
    mount_sse_once_match(
        server,
        move |request: &wiremock::Request| {
            body_contains(request, task) && !has_function_call_output(request, parent_call_id)
        },
        sse(vec![
            ev_response_created(&response_id),
            ev_assistant_message(&format!("msg-worker-{parent_call_id}"), "worker completed"),
            ev_completed(&response_id),
        ]),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_nested_spawn_checks_shared_active_execution_capacity() -> Result<()> {
    let server = start_mock_server().await;
    let first_args = serde_json::to_string(&json!({
        "message": FIRST_TASK,
        "task_name": "first",
    }))?;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, FIRST_PROMPT),
        sse(vec![
            ev_response_created("first-response"),
            ev_function_call_with_namespace(
                "first-call",
                MULTI_AGENT_V2_NAMESPACE,
                "spawn_agent",
                &first_args,
            ),
            ev_completed("first-response"),
        ]),
    )
    .await;
    let second_args = serde_json::to_string(&json!({
        "message": SECOND_TASK,
        "task_name": "second",
    }))?;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, FIRST_TASK) && !has_function_call_output(request, "first-call")
        },
        sse(vec![
            ev_response_created("first-worker-response"),
            ev_function_call_with_namespace(
                "second-call",
                MULTI_AGENT_V2_NAMESPACE,
                "spawn_agent",
                &second_args,
            ),
            ev_completed("first-worker-response"),
        ]),
    )
    .await;
    let second_followup = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, "second-call"),
        sse(vec![
            ev_response_created("second-followup-response"),
            ev_assistant_message("second-followup-message", "blocked"),
            ev_completed("second-followup-response"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, "first-call"),
        sse(vec![
            ev_response_created("first-followup-response"),
            ev_assistant_message("first-followup-message", "spawned"),
            ev_completed("first-followup-response"),
        ]),
    )
    .await;

    let mut builder = test_codex()
        .with_model("gpt-5.6-sol")
        .with_config(|config| {
            config
                .features
                .enable(Feature::Collab)
                .expect("test config should allow feature update");
            config
                .features
                .enable(Feature::MultiAgentV2)
                .expect("test config should allow feature update");
            config.multi_agent_v2.max_concurrent_threads_per_session = 2;
        });
    let test = builder.build(&server).await?;
    test.submit_turn(FIRST_PROMPT).await?;

    let second_output = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(output) = second_followup.function_call_output_text("second-call") {
                return output;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert_eq!(
        second_output,
        "collab spawn failed: agent thread limit reached"
    );
    assert_eq!(test.thread_manager.list_thread_ids().await.len(), 2);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_residency_reload_preserves_inherited_environment_and_tools() -> Result<()> {
    const EVICT_PROMPT: &str = "spawn the replacement worker";
    const FOLLOWUP_PROMPT: &str = "continue the original worker";
    const FOLLOWUP_TASK: &str = "continue work in the original environment";
    const RESIDENT_MODEL: &str = "gpt-5.6-terra";

    let server = start_mock_server().await;
    mount_root_collaboration_call(
        &server,
        FIRST_PROMPT,
        "first-call",
        "spawn_agent",
        json!({
            "message": FIRST_TASK,
            "task_name": "first",
            "model": RESIDENT_MODEL,
            "reasoning_effort": "high",
            "fork_turns": "none"
        }),
    )
    .await;
    mount_completed_worker(&server, FIRST_TASK, "first-call").await;

    mount_root_collaboration_call(
        &server,
        EVICT_PROMPT,
        "replacement-call",
        "spawn_agent",
        json!({ "message": SECOND_TASK, "task_name": "replacement", "fork_turns": "none" }),
    )
    .await;
    mount_completed_worker(&server, SECOND_TASK, "replacement-call").await;

    mount_root_collaboration_call(
        &server,
        FOLLOWUP_PROMPT,
        "followup-call",
        "followup_task",
        json!({ "target": "first", "message": FOLLOWUP_TASK }),
    )
    .await;

    let mut builder = test_codex()
        .with_model("gpt-5.6-sol")
        .with_exec_server_url("none")
        .with_config(|config| {
            for feature in [
                Feature::Collab,
                Feature::MultiAgentV2,
                Feature::UnifiedExec,
                Feature::DeferredExecutor,
            ] {
                config
                    .features
                    .enable(feature)
                    .expect("test config should allow feature update");
            }
            config.use_experimental_unified_exec_tool = true;
            config.model_reasoning_effort = Some(ReasoningEffort::Low);
            config.multi_agent_v2.max_concurrent_threads_per_session = 2;
            config.multi_agent_v2.non_code_mode_only = false;
            config.multi_agent_v2.expose_spawn_agent_model_overrides = true;
        });
    let test = builder.build_with_remote_and_local_env(&server).await?;
    let child_environment = test.executor_environment().selection().clone();
    if let Some(exec_server_url) = test.executor_environment().exec_server_url() {
        test.thread_manager
            .environment_manager()
            .upsert_environment(
                child_environment.environment_id.clone(),
                exec_server_url.to_string(),
                /*connect_timeout*/ None,
            )?;
    }
    let mut created_threads = test.thread_manager.subscribe_thread_created();

    test.submit_turn_with_environments(FIRST_PROMPT, Some(vec![child_environment.clone()]))
        .await?;
    let first_thread_id = created_threads.recv().await?;
    let first_thread = test.thread_manager.get_thread(first_thread_id).await?;
    wait_for_event(first_thread.as_ref(), |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let first_config = first_thread.config_snapshot().await;
    assert_eq!(first_config.model, RESIDENT_MODEL);
    assert_eq!(first_config.reasoning_effort, Some(ReasoningEffort::High));

    test.submit_text_turn(EVICT_PROMPT).await?;
    let replacement_thread_id = created_threads.recv().await?;
    let replacement_thread = test
        .thread_manager
        .get_thread(replacement_thread_id)
        .await?;
    wait_for_event(replacement_thread.as_ref(), |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert!(
        test.thread_manager
            .get_thread(first_thread_id)
            .await
            .is_err()
    );

    let first_thread_id_string = first_thread_id.to_string();
    let expected_thread_id = first_thread_id_string.clone();
    let reloaded_worker_request = mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            request
                .headers
                .get("x-codex-turn-metadata")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|metadata| {
                    serde_json::from_str::<serde_json::Value>(metadata).is_ok_and(|metadata| {
                        metadata["thread_id"].as_str() == Some(expected_thread_id.as_str())
                    })
                })
        },
        sse(vec![
            ev_response_created("resp-worker-followup-call"),
            ev_assistant_message("msg-worker-followup-call", "worker completed"),
            ev_completed("resp-worker-followup-call"),
        ]),
    )
    .await;

    test.submit_text_turn(FOLLOWUP_PROMPT).await?;
    let reloaded_worker = test.thread_manager.get_thread(first_thread_id).await?;
    let reloaded_config = reloaded_worker.config_snapshot().await;
    assert_eq!(
        reloaded_config.environments.environments,
        vec![child_environment]
    );
    assert_eq!(reloaded_config.model, RESIDENT_MODEL);
    assert_eq!(
        reloaded_config.reasoning_effort,
        Some(ReasoningEffort::High)
    );

    let reloaded_request = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        loop {
            if let Some(request) = reloaded_worker_request.last_request() {
                return request;
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 10)).await;
        }
    })
    .await?;
    let turn_metadata: serde_json::Value = serde_json::from_str(
        &reloaded_request
            .header("x-codex-turn-metadata")
            .expect("reloaded worker request turn metadata"),
    )?;
    let followup_turn_id = turn_metadata["turn_id"]
        .as_str()
        .expect("reloaded worker request turn id")
        .to_string();
    assert_eq!(
        turn_metadata["thread_id"].as_str(),
        Some(first_thread_id_string.as_str())
    );
    let body = reloaded_request.body_json();
    assert_eq!(body["model"].as_str(), Some(RESIDENT_MODEL));
    assert_eq!(body["reasoning"]["effort"].as_str(), Some("high"));
    let reloaded_tools = body
        .get("tools")
        .or_else(|| {
            body["input"]
                .as_array()?
                .iter()
                .find(|item| item["type"] == "additional_tools")?
                .get("tools")
        })
        .expect("expected tools in the reloaded worker request");
    assert!(reloaded_tools.to_string().contains("### `exec_command`"));
    wait_for_event(
        reloaded_worker.as_ref(),
        |event| matches!(event, EventMsg::TurnComplete(event) if event.turn_id == followup_turn_id),
    )
    .await;

    Ok(())
}
