use std::sync::Arc;

use codex_features::Feature;
use codex_protocol::models::PermissionProfile;
use core_test_support::responses;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_disabled_omits_tool_fields_from_responses_request() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let response = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            responses::ev_completed("resp-1"),
        ]),
    )
    .await;

    let home = Arc::new(tempfile::TempDir::new().expect("create codex home"));
    std::fs::write(
        home.path().join("config.toml"),
        "[tools]\nenabled = false\n",
    )
    .expect("write config.toml");

    let mut builder = test_codex()
        .with_model("gpt-5.2")
        .with_home(home)
        .with_code_mode_host_program("unused-code-mode-host".into())
        .with_config(|config| {
            config
                .features
                .disable(Feature::CodeModeHost)
                .expect("disable code mode host for request-shape test");
        });
    let test = builder
        .build(&server)
        .await
        .expect("create test Codex conversation");

    test.submit_turn_with_permission_profile("hello", PermissionProfile::Disabled)
        .await
        .expect("submit turn");

    let body = response.single_request().body_json();
    assert_eq!(body.get("tools"), None);
    assert_eq!(body.get("tool_choice"), None);
    assert_eq!(body["parallel_tool_calls"], false);
}
