use anyhow::Result;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_models_manager::bundled_models_response;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_compact_json_once;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::namespace_child_tool;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::responses::strip_metadata_from_json;
use core_test_support::responses::strip_response_item_ids_from_json;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use test_case::test_case;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::header_regex;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::query_param;

#[test_case("gpt-6-astra", "0.153.0"; "astra")]
#[test_case("gpt-5.6-sol", "0.144.0"; "sol")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_and_repeated_turns_preserve_wire_contract(
    model_slug: &'static str,
    inference_version: &str,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let mut model = bundled_models_response()?
        .models
        .into_iter()
        .find(|model| model.slug == model_slug)
        .expect("bundled model metadata");
    model.visibility = ModelVisibility::List;
    model.display_name = "Model from discovery".to_string();
    let remote = ModelsResponse {
        models: vec![model],
    };
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(query_param("client_version", "0.153.0"))
        .and(header("version", "0.153.0"))
        .and(header_regex("user-agent", r"/0\.153\.0 "))
        .respond_with(ResponseTemplate::new(200).set_body_json(remote))
        .expect(1..)
        .mount(&server)
        .await;

    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_model(model_slug)
        .with_config(|config| {
            let _ = config.features.disable(Feature::RemoteCompactionV2);
        });
    let test = builder.build_with_auto_env(&server).await?;
    let models = test
        .thread_manager
        .get_models_manager()
        .list_models(
            RefreshStrategy::Online,
            codex_core::test_support::default_http_client_factory(),
        )
        .await;
    assert!(models.iter().any(|model| {
        model.model == model_slug && model.display_name == "Model from discovery"
    }));

    let mut bodies = Vec::new();
    for (response_id, message_id, prompt) in [
        ("resp-1", "msg-1", "Reply ready"),
        ("resp-2", "msg-2", "Reply ready again"),
    ] {
        let response = mount_sse_once(
            &server,
            sse(vec![
                ev_response_created(response_id),
                ev_assistant_message(message_id, "ready"),
                ev_completed(response_id),
            ]),
        )
        .await;
        test.submit_turn(prompt).await?;
        let request = response.single_request();
        assert_eq!(
            request.header("version"),
            Some(inference_version.to_string())
        );
        assert!(
            request
                .header("user-agent")
                .expect("request user-agent")
                .contains(&format!("/{inference_version} "))
        );
        let body = request.body_json();
        assert_eq!(body["model"], json!(model_slug));
        assert_eq!(body["reasoning"]["context"], json!("all_turns"));
        assert_eq!(body["parallel_tool_calls"], json!(false));
        assert!(body.get("tools").is_none());
        let input_tools = body["input"]
            .as_array()
            .expect("input array")
            .iter()
            .find(|item| item["type"] == "additional_tools")
            .expect("Responses Lite tool definitions");
        assert!(namespace_child_tool(input_tools, "functions", "exec").is_some());
        assert!(namespace_child_tool(input_tools, "functions", "exec_command").is_none());
        for name in [
            "spawn_agent",
            "send_message",
            "followup_task",
            "wait_agent",
            "interrupt_agent",
            "list_agents",
        ] {
            assert!(namespace_child_tool(input_tools, "agents", name).is_some());
        }
        let exec =
            namespace_child_tool(input_tools, "functions", "exec").expect("code-mode executor");
        assert!(
            exec["description"]
                .as_str()
                .expect("executor description")
                .contains("exec_command")
        );
        bodies.push(strip_response_item_ids_from_json(strip_metadata_from_json(
            body,
        )));
    }
    assert_eq!(bodies[0]["prompt_cache_key"], bodies[1]["prompt_cache_key"]);
    assert_eq!(bodies[0]["reasoning"], bodies[1]["reasoning"]);
    let first_input = bodies[0]["input"].as_array().expect("first input array");
    let next_input = bodies[1]["input"].as_array().expect("next input array");
    assert_eq!(first_input, &next_input[..first_input.len()]);
    let compact = mount_compact_json_once(
        &server,
        json!({"output": [{
            "type": "compaction", "encrypted_content": "compacted"
        }]}),
    )
    .await;
    test.codex.submit(Op::Compact).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let request = compact.single_request();
    assert_eq!(
        request.header("version"),
        Some(inference_version.to_string())
    );
    assert!(
        request
            .header("user-agent")
            .expect("compaction user-agent")
            .contains(&format!("/{inference_version} "))
    );
    Ok(())
}
