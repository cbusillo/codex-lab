use anyhow::Result;
use codex_features::Feature;
use codex_model_provider_info::LMSTUDIO_OSS_PROVIDER_ID;
use codex_model_provider_info::built_in_model_providers;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use serde_json::Value;
use std::time::Duration;

const LOCAL_PROMPT: &str = "use the local tool surface";
const LOCAL_TASK: &str = "local worker task";
const MULTI_AGENT_V2_TOOL_NAMES: [&str; 6] = [
    "spawn_agent",
    "send_message",
    "followup_task",
    "wait_agent",
    "interrupt_agent",
    "list_agents",
];

fn request_body(request: &wiremock::Request) -> Option<Value> {
    serde_json::from_slice::<Value>(&request.body).ok()
}

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    request_body(request).is_some_and(|body| body.to_string().contains(text))
}

fn has_function_call_output(request: &wiremock::Request, call_id: &str) -> bool {
    request_body(request).is_some_and(|body| {
        body.get("input")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("type").and_then(Value::as_str) == Some("function_call_output")
                        && item.get("call_id").and_then(Value::as_str) == Some(call_id)
                })
            })
    })
}

fn request_tools(body: &Value) -> Vec<Value> {
    body.get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn top_level_tool_names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn tool_type_names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .map(|tool| {
            let tool_type = tool
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("function");
            let tool_name = tool
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed>");
            format!("{tool_type}/{tool_name}")
        })
        .collect()
}

fn assert_no_exotic_tool_types(tools: &[Value]) {
    for tool in tools {
        let tool_type = tool
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function");
        let tool_name = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unnamed>");
        assert!(
            !matches!(tool_type, "namespace" | "custom" | "web_search"),
            "local provider request must not use unsupported tool shapes, found {tool_type:?}/{tool_name:?}"
        );
    }
}

fn enable_multi_agent_v2(config: &mut codex_core::config::Config) {
    config
        .features
        .enable(Feature::Collab)
        .expect("test config should allow feature update");
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("test config should allow feature update");
}

fn local_provider_builder() -> core_test_support::test_codex::TestCodexBuilder {
    test_codex()
        .with_model_provider(
            built_in_model_providers(/*openai_base_url*/ None)[LMSTUDIO_OSS_PROVIDER_ID].clone(),
        )
        .with_config(enable_multi_agent_v2)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_provider_request_uses_flat_function_tools() -> Result<()> {
    let server = start_mock_server().await;
    let mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-local-flat"),
            ev_assistant_message("msg-local-flat", "done"),
            ev_completed("resp-local-flat"),
        ]),
    )
    .await;

    let test = local_provider_builder().build(&server).await?;
    test.submit_turn(LOCAL_PROMPT).await?;

    let tools = request_tools(&mock.single_request().body_json());
    assert_no_exotic_tool_types(&tools);
    let function_names = top_level_tool_names(&tools);
    for expected in MULTI_AGENT_V2_TOOL_NAMES {
        assert!(
            function_names.iter().any(|name| name == expected),
            "expected flat function tool {expected:?} in local provider request: {function_names:?}"
        );
    }
    assert!(
        !function_names.iter().any(|name| name == "apply_patch"),
        "apply_patch should be dropped for providers without custom tool support: {function_names:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_provider_can_spawn_subagent_through_flat_tools() -> Result<()> {
    let server = start_mock_server().await;
    const CALL_ID: &str = "local-spawn-call";

    let spawn_args = serde_json::to_string(&serde_json::json!({
        "message": LOCAL_TASK,
        "task_name": "local_worker",
        "fork_turns": "none",
    }))?;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, LOCAL_PROMPT) && !has_function_call_output(request, CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-local-spawn"),
            ev_function_call(CALL_ID, "spawn_agent", &spawn_args),
            ev_completed("resp-local-spawn"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, LOCAL_TASK) && !has_function_call_output(request, CALL_ID)
        },
        sse(vec![
            ev_response_created("resp-local-worker"),
            ev_assistant_message("msg-local-worker", "worker completed"),
            ev_completed("resp-local-worker"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, CALL_ID),
        sse(vec![
            ev_response_created("resp-local-spawn-done"),
            ev_assistant_message("msg-local-spawn-done", "spawned"),
            ev_completed("resp-local-spawn-done"),
        ]),
    )
    .await;

    let test = local_provider_builder()
        .with_config(|config| {
            config.multi_agent_v2.max_concurrent_threads_per_session = 2;
        })
        .build(&server)
        .await?;
    test.submit_turn(LOCAL_PROMPT).await?;

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if test.thread_manager.list_thread_ids().await.len() == 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("spawn_agent should start a subagent through the flat tool surface");
    assert_eq!(test.thread_manager.list_thread_ids().await.len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_provider_request_keeps_namespaced_tool_surface() -> Result<()> {
    let server = start_mock_server().await;
    let mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-openai"),
            ev_assistant_message("msg-openai", "done"),
            ev_completed("resp-openai"),
        ]),
    )
    .await;

    let test = test_codex()
        .with_config(enable_multi_agent_v2)
        .build(&server)
        .await?;
    test.submit_turn("hello openai").await?;

    let tools = request_tools(&mock.single_request().body_json());
    let agents_namespace = tools
        .iter()
        .find(|tool| {
            tool.get("type").and_then(Value::as_str) == Some("namespace")
                && tool.get("name").and_then(Value::as_str) == Some("agents")
        })
        .expect("OpenAI-style request should keep the agents namespace");
    let nested_tools = agents_namespace
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let nested_names = top_level_tool_names(&nested_tools);
    for expected in MULTI_AGENT_V2_TOOL_NAMES {
        assert!(
            nested_names.iter().any(|name| name == expected),
            "expected {expected:?} inside the agents namespace: {nested_names:?}"
        );
    }
    let top_level_names = top_level_tool_names(&tools);
    assert!(
        !top_level_names.iter().any(|name| name == "spawn_agent"),
        "multi-agent tools should stay namespaced for OpenAI-style providers: {top_level_names:?}"
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool.get("type").and_then(Value::as_str) == Some("web_search")),
        "OpenAI-style request should keep the hosted web_search tool: {:?}",
        tool_type_names(&tools)
    );
    assert!(
        tools.iter().any(|tool| {
            tool.get("type").and_then(Value::as_str) == Some("custom")
                && tool.get("name").and_then(Value::as_str) == Some("apply_patch")
        }),
        "OpenAI-style request should keep the freeform apply_patch tool: {:?}",
        tool_type_names(&tools)
    );
    Ok(())
}
