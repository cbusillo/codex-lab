use super::auth_change_tests::auth_manager_for_account;
use super::auth_change_tests::respond_with_remote_control_enrollment;
use super::*;
use crate::transport::remote_control::auth::REMOTE_CONTROL_ACCOUNT_ID_HEADER;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::time::Duration;
use tokio::time::timeout;

struct WorkerHandles {
    shutdown_token: CancellationToken,
    // Must be kept alive: dropping it closes the enabled channel and makes
    // connect() return Shutdown immediately.
    _desired_state_tx: Arc<watch::Sender<RemoteControlDesiredState>>,
    // Must be kept alive: dropping it closes the reconnect channel and makes
    // the backoff select return Shutdown immediately.
    _reconnect_tx: mpsc::UnboundedSender<u64>,
    transport_event_rx: mpsc::Receiver<TransportEvent>,
}

// Builds a RemoteControlWebsocket. When `pre_seeded_account` is Some the
// initial enrollment is pre-seeded so the first connection goes directly to
// the WebSocket upgrade without an HTTP enrollment round-trip. Pass None to
// let the worker perform the real HTTP enrollment on first connection.
async fn worker_with_listener(
    listener: &TcpListener,
    codex_home: &TempDir,
    auth_manager: Arc<AuthManager>,
    pre_seeded_account: Option<(&str, &str)>, // (account_id, server_token)
) -> (RemoteControlWebsocket, WorkerHandles) {
    let remote_control_url = remote_control_url_for_listener(listener);
    let remote_control_target =
        normalize_remote_control_url(&remote_control_url).expect("target should normalize");
    let (transport_event_tx, transport_event_rx) = mpsc::channel(/*buffer*/ 16);
    let (status_publisher, _status_rx) = remote_control_status_channel();
    let shutdown_token = CancellationToken::new();
    let desired_state_tx = Arc::new(enabled_desired_state_sender());
    let (reconnect_tx, reconnect_rx) = mpsc::unbounded_channel();
    let initial_enrollment = pre_seeded_account.map(|(account_id, server_token)| {
        let mut enrollment = remote_control_enrollment(Some(server_token));
        enrollment.remote_control_target = remote_control_target.clone();
        enrollment.account_id = account_id.to_string();
        enrollment
    });
    let websocket = RemoteControlWebsocket::new(
        RemoteControlWebsocketConfig {
            remote_control_url,
            installation_id: TEST_INSTALLATION_ID.to_string(),
            remote_control_target: Some(remote_control_target),
            server_name: "test-server".to_string(),
        },
        Some(remote_control_state_runtime(codex_home).await),
        auth_manager,
        RemoteControlChannels {
            transport_event_tx,
            status_publisher,
            current_enrollment: test_current_enrollment(initial_enrollment),
            pairing_persistence_key: watch::channel(None).0,
            desired_state_persistence_lock: Arc::new(Semaphore::new(1)),
        },
        shutdown_token.clone(),
        Arc::clone(&desired_state_tx),
        reconnect_rx,
    );
    let handles = WorkerHandles {
        shutdown_token,
        _desired_state_tx: desired_state_tx,
        _reconnect_tx: reconnect_tx,
        transport_event_rx,
    };
    (websocket, handles)
}

async fn accept_enroll_request(listener: &TcpListener) -> (TcpStream, String) {
    let (stream, _) = timeout(TEST_HTTP_ACCEPT_TIMEOUT, listener.accept())
        .await
        .expect("enroll request should arrive in time")
        .expect("listener should accept");
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .await
        .expect("request line should read");
    let mut account_id_header = String::new();
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("header line should read");
        if line == "\r\n" {
            break;
        }
        let line = line.trim_end_matches("\r\n");
        if let Some((name, value)) = line.split_once(':') {
            if name.to_ascii_lowercase() == REMOTE_CONTROL_ACCOUNT_ID_HEADER {
                account_id_header = value.trim().to_string();
            }
        }
    }
    (reader.into_inner(), account_id_header)
}

async fn accept_websocket(listener: &TcpListener) -> tokio_tungstenite::WebSocketStream<TcpStream> {
    let (stream, _) = timeout(TEST_HTTP_ACCEPT_TIMEOUT, listener.accept())
        .await
        .expect("websocket handshake should arrive in time")
        .expect("listener should accept");
    tokio_tungstenite::accept_async(stream)
        .await
        .expect("websocket handshake should succeed")
}

/// Verify that the full `run()` worker loop survives an account A→B change:
/// the same worker closes the account-A relay, re-enrolls under account B, and
/// opens a fresh relay connection — all without stopping the worker task.
#[tokio::test]
async fn worker_outer_loop_reconnects_under_new_account() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let codex_home = TempDir::new().expect("temp dir should create");
    let auth_manager = auth_manager_for_account(&codex_home, "account-a", "access-a").await;

    let (websocket, handles) =
        worker_with_listener(&listener, &codex_home, auth_manager.clone(), None).await;

    let run_task = tokio::spawn(websocket.run(/*app_server_client_name_rx*/ None));

    // Phase 1 — account A enrolls and connects.
    let (stream_a, account_id_a) = accept_enroll_request(&listener).await;
    assert_eq!(account_id_a, "account-a");
    respond_with_remote_control_enrollment(stream_a, "srv_e_a", "env_a", "token-a").await;
    let mut ws_a = accept_websocket(&listener).await;

    // Switch control auth to account B while account A is connected.
    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json_for_account("account-b", "access-b"),
        TEST_AUTH_STORE_MODE,
        TEST_AUTH_KEYRING_BACKEND,
    )
    .expect("replacement auth should save");
    auth_manager.reload().await;

    // Phase 2 — account A websocket closes when the worker detects the identity
    // change, then the worker re-enrolls and opens a new relay under account B.
    timeout(Duration::from_secs(5), ws_a.next())
        .await
        .expect("account-A relay should close after identity change");

    let (stream_b, account_id_b) = accept_enroll_request(&listener).await;
    assert_eq!(account_id_b, "account-b");
    respond_with_remote_control_enrollment(stream_b, "srv_e_b", "env_b", "token-b").await;
    let _ws_b = accept_websocket(&listener).await;

    // Worker is still alive; stop it cleanly.
    handles.shutdown_token.cancel();
    timeout(Duration::from_secs(5), run_task)
        .await
        .expect("run task should stop after shutdown")
        .expect("run task should not panic");
}

/// When account identity changes, any virtual (remote-control) clients that
/// were open under the old identity must receive a ConnectionClosed event so
/// the app-server can tear them down and not forward stale traffic.
#[tokio::test]
async fn worker_outer_loop_closes_virtual_clients_on_account_change() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let codex_home = TempDir::new().expect("temp dir should create");
    let auth_manager = auth_manager_for_account(&codex_home, "account-a", "access-a").await;

    let (websocket, mut handles) =
        worker_with_listener(&listener, &codex_home, auth_manager.clone(), None).await;

    let run_task = tokio::spawn(websocket.run(/*app_server_client_name_rx*/ None));

    // Enroll and connect account A.
    let (stream_a, _) = accept_enroll_request(&listener).await;
    respond_with_remote_control_enrollment(stream_a, "srv_e_a", "env_a", "token-a").await;
    let mut ws_a = accept_websocket(&listener).await;

    // Inject a fake ClientEnvelope initialize message through the server-side
    // websocket so the relay creates a virtual client.
    // ClientEnvelope uses #[serde(flatten)] on ClientEvent, which itself uses
    // #[serde(tag = "type", rename_all = "snake_case")], so the type discriminant
    // and variant fields appear at the top level.
    let init_envelope = json!({
        "type": "client_message",
        "message": {
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        },
        "client_id": "test-client-1",
        "stream_id": "test-stream-1",
        "seq_id": 0
    });
    ws_a.send(tungstenite::Message::Text(
        serde_json::to_string(&init_envelope)
            .expect("init envelope should serialize")
            .into(),
    ))
    .await
    .expect("init message should send");

    // Wait for ConnectionOpened to confirm a virtual client is live.
    let connection_id = timeout(Duration::from_secs(5), handles.transport_event_rx.recv())
        .await
        .expect("ConnectionOpened should arrive in time")
        .and_then(|event| match event {
            TransportEvent::ConnectionOpened { connection_id, .. } => Some(connection_id),
            _ => None,
        })
        .expect("first transport event should be ConnectionOpened");
    // Drain the IncomingMessage for the initialize.
    let _ = timeout(
        Duration::from_millis(500),
        handles.transport_event_rx.recv(),
    )
    .await;

    // Switch to account B — triggers ControlAuthChanged in the worker.
    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json_for_account("account-b", "access-b"),
        TEST_AUTH_STORE_MODE,
        TEST_AUTH_KEYRING_BACKEND,
    )
    .expect("replacement auth should save");
    auth_manager.reload().await;

    // The virtual client for account A should be closed.
    let closed_id = timeout(Duration::from_secs(5), async {
        loop {
            match handles.transport_event_rx.recv().await? {
                TransportEvent::ConnectionClosed { connection_id } => return Some(connection_id),
                _ => continue,
            }
        }
    })
    .await
    .expect("ConnectionClosed should arrive in time")
    .expect("transport channel should stay open");
    assert_eq!(closed_id, connection_id);

    // Let account B enroll and connect so the worker can proceed cleanly.
    let (stream_b, _) = accept_enroll_request(&listener).await;
    respond_with_remote_control_enrollment(stream_b, "srv_e_b", "env_b", "token-b").await;
    let _ws_b = accept_websocket(&listener).await;

    handles.shutdown_token.cancel();
    timeout(Duration::from_secs(5), run_task)
        .await
        .expect("run task should stop")
        .expect("run task should not panic");
}

/// After an account switch the subscribe cursor accumulated for the old account
/// must NOT be forwarded to the new account's relay connection — it belongs to
/// a different identity's event stream.
#[tokio::test]
async fn worker_outer_loop_clears_subscribe_cursor_on_account_change() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let codex_home = TempDir::new().expect("temp dir should create");
    let auth_manager = auth_manager_for_account(&codex_home, "account-a", "access-a").await;

    let (websocket, handles) =
        worker_with_listener(&listener, &codex_home, auth_manager.clone(), None).await;

    let run_task = tokio::spawn(websocket.run(/*app_server_client_name_rx*/ None));

    // Enroll and connect account A.
    let (stream_a, _) = accept_enroll_request(&listener).await;
    respond_with_remote_control_enrollment(stream_a, "srv_e_a", "env_a", "token-a").await;
    let mut ws_a = accept_websocket(&listener).await;

    // Push a subscribe-cursor via a fake Ack from the server-side websocket.
    // Same flattened format as ClientMessage: type discriminant at top level.
    let ack_envelope = json!({
        "type": "ack",
        "client_id": "test-client-cursor",
        "stream_id": "test-stream-cursor",
        "seq_id": 1,
        "cursor": "cursor-for-account-a"
    });
    ws_a.send(tungstenite::Message::Text(
        serde_json::to_string(&ack_envelope)
            .expect("ack should serialize")
            .into(),
    ))
    .await
    .expect("ack should send");
    // Small pause to let the reader process the cursor before we change auth.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Switch to account B.
    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json_for_account("account-b", "access-b"),
        TEST_AUTH_STORE_MODE,
        TEST_AUTH_KEYRING_BACKEND,
    )
    .expect("replacement auth should save");
    auth_manager.reload().await;

    // Account A relay closes.
    timeout(Duration::from_secs(5), ws_a.next())
        .await
        .expect("account-A relay should close");

    // Account B enrolls.
    let (stream_b, _) = accept_enroll_request(&listener).await;
    respond_with_remote_control_enrollment(stream_b, "srv_e_b", "env_b", "token-b").await;

    // Accept the websocket upgrade for account B and inspect its headers to
    // confirm the subscribe cursor was not carried over.
    let (tcp_stream_b, _) = timeout(TEST_HTTP_ACCEPT_TIMEOUT, listener.accept())
        .await
        .expect("account-B websocket request should arrive")
        .expect("listener should accept");
    let mut reader = BufReader::new(tcp_stream_b);
    let mut has_subscribe_cursor_header = false;
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("header line should read");
        if line == "\r\n" {
            break;
        }
        if line
            .to_ascii_lowercase()
            .starts_with("x-codex-subscribe-cursor:")
        {
            has_subscribe_cursor_header = true;
        }
    }
    assert!(
        !has_subscribe_cursor_header,
        "account B's relay connection must not carry account A's subscribe cursor"
    );

    handles.shutdown_token.cancel();
    timeout(Duration::from_secs(5), run_task)
        .await
        .expect("run task should stop")
        .expect("run task should not panic");
}

/// After logging out (no auth available), the worker must keep retrying without
/// panicking. Once auth is restored for a new account the worker must connect.
#[tokio::test]
async fn worker_outer_loop_survives_logout_then_reconnects_on_relogin() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let codex_home = TempDir::new().expect("temp dir should create");
    let auth_manager = auth_manager_for_account(&codex_home, "account-a", "access-a").await;

    // Pre-seed the account-A enrollment so the first connection goes directly
    // to the WebSocket upgrade without an HTTP enrollment round-trip.
    let (websocket, handles) = worker_with_listener(
        &listener,
        &codex_home,
        auth_manager.clone(),
        Some(("account-a", "token-a")),
    )
    .await;

    let run_task = tokio::spawn(websocket.run(/*app_server_client_name_rx*/ None));

    // Initial account-A session — no HTTP enrollment (pre-seeded).
    let mut ws_a = accept_websocket(&listener).await;

    // Log out — remove auth entirely (no tokens, no API key).
    save_auth(
        codex_home.path(),
        &AuthDotJson {
            auth_mode: None,
            openai_api_key: None,
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
        },
        TEST_AUTH_STORE_MODE,
        TEST_AUTH_KEYRING_BACKEND,
    )
    .expect("logout auth should save");
    auth_manager.reload().await;

    // Account A relay should close.
    timeout(Duration::from_secs(5), ws_a.next())
        .await
        .expect("account-A relay should close after logout");

    // Worker keeps retrying (PermissionDenied → backoff). Restore auth for
    // account B to let the worker reconnect.
    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json_for_account("account-b", "access-b"),
        TEST_AUTH_STORE_MODE,
        TEST_AUTH_KEYRING_BACKEND,
    )
    .expect("relogin auth should save");
    auth_manager.reload().await;

    // Worker should enroll and connect under account B.
    let (stream_b, account_id_b) = accept_enroll_request(&listener).await;
    assert_eq!(account_id_b, "account-b");
    respond_with_remote_control_enrollment(stream_b, "srv_e_b", "env_b", "token-b").await;
    let _ws_b = accept_websocket(&listener).await;

    handles.shutdown_token.cancel();
    timeout(Duration::from_secs(5), run_task)
        .await
        .expect("run task should stop")
        .expect("run task should not panic");
}

/// Switching from a ChatGPT account to an API-key account must not crash the
/// worker. API-key auth is rejected as PermissionDenied by
/// `load_remote_control_auth`, so the worker will error and keep retrying until
/// a compatible auth arrives.
#[tokio::test]
async fn worker_outer_loop_handles_api_key_transition_safely() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let codex_home = TempDir::new().expect("temp dir should create");
    let auth_manager = auth_manager_for_account(&codex_home, "account-a", "access-a").await;

    // Pre-seed the account-A enrollment so the first connection goes directly
    // to the WebSocket upgrade without an HTTP enrollment round-trip.
    let (websocket, handles) = worker_with_listener(
        &listener,
        &codex_home,
        auth_manager.clone(),
        Some(("account-a", "token-a")),
    )
    .await;

    let run_task = tokio::spawn(websocket.run(/*app_server_client_name_rx*/ None));

    // Initial account-A session — no HTTP enrollment (pre-seeded).
    let mut ws_a = accept_websocket(&listener).await;

    // Switch to API-key auth (remote control requires ChatGPT, not API key).
    save_auth(
        codex_home.path(),
        &AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some("sk-test-api-key".to_string()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
        },
        TEST_AUTH_STORE_MODE,
        TEST_AUTH_KEYRING_BACKEND,
    )
    .expect("api-key auth should save");
    auth_manager.reload().await;

    // Account A relay closes when the identity change is detected.
    timeout(Duration::from_secs(5), ws_a.next())
        .await
        .expect("account-A relay should close on API-key switch");

    // Worker is alive but stuck (PermissionDenied from load_remote_control_auth).
    // Restore ChatGPT auth to unblock the worker.
    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json_for_account("account-b", "access-b"),
        TEST_AUTH_STORE_MODE,
        TEST_AUTH_KEYRING_BACKEND,
    )
    .expect("chatgpt relogin auth should save");
    auth_manager.reload().await;

    let (stream_b, account_id_b) = accept_enroll_request(&listener).await;
    assert_eq!(account_id_b, "account-b");
    respond_with_remote_control_enrollment(stream_b, "srv_e_b", "env_b", "token-b").await;
    let _ws_b = accept_websocket(&listener).await;

    handles.shutdown_token.cancel();
    timeout(Duration::from_secs(5), run_task)
        .await
        .expect("run task should stop")
        .expect("run task should not panic");
}
