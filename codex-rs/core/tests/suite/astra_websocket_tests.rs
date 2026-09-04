use super::USER_AGENT_HEADER;
use super::message_item;
use super::prewarm_metadata;
use super::prompt_with_input;
use super::stream_until_complete;
use super::websocket_harness_for_codex_backend;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::start_websocket_server;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_wire_identity_survives_prewarm_reuse_and_model_switch() {
    skip_if_no_network!();
    let server = start_websocket_server(vec![
        vec![
            vec![ev_response_created("warmup"), ev_completed("warmup")],
            vec![ev_response_created("astra-1"), ev_completed("astra-1")],
            vec![ev_response_created("astra-2"), ev_completed("astra-2")],
        ],
        vec![vec![ev_response_created("sol-1"), ev_completed("sol-1")]],
    ])
    .await;
    let mut harness = websocket_harness_for_codex_backend(&server).await;
    let catalog = codex_models_manager::bundled_models_response().unwrap();
    harness.model_info = catalog
        .models
        .iter()
        .find(|model| model.slug == "gpt-6-astra")
        .unwrap()
        .clone();
    let mut session = harness.client.new_session();
    let mut input = vec![message_item("hello")];
    session
        .prewarm_websocket(
            &prompt_with_input(input.clone()),
            &harness.model_info,
            &harness.session_telemetry,
            harness.effort.clone(),
            harness.summary,
            /*service_tier*/ None,
            &prewarm_metadata(&harness, /*turn_id*/ None),
        )
        .await
        .unwrap();
    stream_until_complete(&mut session, &harness, &prompt_with_input(input.clone())).await;
    input.push(message_item("continue"));
    stream_until_complete(&mut session, &harness, &prompt_with_input(input.clone())).await;
    assert_eq!(server.handshakes().len(), 1);

    harness.model_info = catalog
        .models
        .iter()
        .find(|model| model.slug == "gpt-5.6-sol")
        .unwrap()
        .clone();
    stream_until_complete(&mut session, &harness, &prompt_with_input(input)).await;
    let identities: Vec<_> = server
        .handshakes()
        .iter()
        .map(|handshake| {
            (
                handshake.header("version"),
                handshake.header(USER_AGENT_HEADER),
            )
        })
        .collect();
    assert_eq!(
        identities,
        vec![
            (
                Some("0.153.0".to_string()),
                Some(codex_login::default_client::get_codex_user_agent_for_model(
                    "gpt-6-astra"
                ))
            ),
            (
                Some("0.144.0".to_string()),
                Some(codex_login::default_client::get_codex_user_agent_for_model(
                    "gpt-5.6-sol"
                ))
            ),
        ]
    );
    assert_eq!(
        server
            .connections()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![3, 1]
    );
    server.shutdown().await;
}
