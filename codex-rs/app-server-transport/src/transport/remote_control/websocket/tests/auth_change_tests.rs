use super::*;
use crate::transport::remote_control::enroll::REMOTE_CONTROL_ACCOUNT_ID_HEADER;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;

pub(super) async fn auth_manager_for_account(
    codex_home: &TempDir,
    account_id: &str,
    access_token: &str,
) -> Arc<AuthManager> {
    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json_for_account(account_id, access_token),
        AuthCredentialsStoreMode::File,
    )
    .expect("test auth should save");
    AuthManager::shared(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*chatgpt_base_url*/ None,
    )
    .await
}

async fn active_remote_control_websocket(
    listener: &TcpListener,
    codex_home: &TempDir,
    control_auth_manager: Arc<AuthManager>,
    account_id: &str,
) -> (
    RemoteControlWebsocket,
    CancellationToken,
    watch::Sender<bool>,
    mpsc::UnboundedSender<u64>,
) {
    let remote_control_url = remote_control_url_for_listener(listener);
    let remote_control_target =
        normalize_remote_control_url(&remote_control_url).expect("target should normalize");
    let mut enrollment = remote_control_enrollment(Some("server-token-a"));
    enrollment.remote_control_target = remote_control_target.clone();
    enrollment.account_id = account_id.to_string();
    let (transport_event_tx, _transport_event_rx) = mpsc::channel(/*buffer*/ 1);
    let (status_publisher, _status_rx) = remote_control_status_channel();
    let shutdown_token = CancellationToken::new();
    let (enabled_tx, enabled_rx) = watch::channel(/*init*/ true);
    let (reconnect_tx, reconnect_rx) = mpsc::unbounded_channel();
    let websocket = RemoteControlWebsocket::new(
        RemoteControlWebsocketConfig {
            remote_control_url,
            installation_id: TEST_INSTALLATION_ID.to_string(),
            remote_control_target: Some(remote_control_target),
            server_name: "test-server".to_string(),
        },
        Some(remote_control_state_runtime(codex_home).await),
        control_auth_manager,
        RemoteControlChannels {
            transport_event_tx,
            status_publisher,
            current_enrollment: test_current_enrollment(Some(enrollment)),
            pairing_persistence_key: watch::channel(None).0,
        },
        shutdown_token.clone(),
        enabled_rx,
        reconnect_rx,
    );
    (websocket, shutdown_token, enabled_tx, reconnect_tx)
}

async fn reconnect_after_control_auth_change(
    websocket: &mut RemoteControlWebsocket,
    shutdown_token: &CancellationToken,
    active_control_auth: ActiveControlAuth,
) {
    let (client_websocket, mut server_websocket) = connected_websocket_pair().await;
    let reason = websocket
        .run_connection(
            client_websocket,
            shutdown_token.child_token(),
            active_control_auth,
        )
        .await;

    assert!(matches!(reason, ConnectionEndReason::ControlAuthChanged));
    timeout(Duration::from_secs(5), server_websocket.next())
        .await
        .expect("control auth change should close the active relay websocket");
}

async fn accept_http_request_with_headers(
    listener: &TcpListener,
) -> (TcpStream, String, BTreeMap<String, String>) {
    let (stream, _) = timeout(TEST_HTTP_ACCEPT_TIMEOUT, listener.accept())
        .await
        .expect("HTTP request should arrive in time")
        .expect("listener should accept request");
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .await
        .expect("request line should read");
    let mut headers = BTreeMap::new();
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
        let (name, value) = line.split_once(':').expect("header should contain colon");
        headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
    }
    (
        reader.into_inner(),
        request_line.trim_end_matches("\r\n").to_string(),
        headers,
    )
}

pub(super) async fn respond_with_remote_control_enrollment(
    mut stream: TcpStream,
    server_id: &str,
    environment_id: &str,
    remote_control_token: &str,
) {
    let body = serde_json::json!({
        "server_id": server_id,
        "environment_id": environment_id,
        "remote_control_token": remote_control_token,
        "expires_at": "2999-01-01T00:00:00Z",
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("response should write");
    stream.flush().await.expect("response should flush");
}

#[tokio::test]
async fn active_relay_reconnects_under_new_control_account() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let codex_home = TempDir::new().expect("temp dir should create");
    let control_auth_manager = auth_manager_for_account(&codex_home, "account-a", "access-a").await;
    let (mut websocket, shutdown_token, _enabled_tx, _reconnect_tx) =
        active_remote_control_websocket(
            &listener,
            &codex_home,
            control_auth_manager.clone(),
            "account-a",
        )
        .await;
    let active_control_auth = ActiveControlAuth {
        account_id: "account-a".to_string(),
        revision: control_auth_manager.auth_revision(),
    };
    let revision_before_account_change = control_auth_manager.auth_revision();

    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json_for_account("account-b", "access-b"),
        AuthCredentialsStoreMode::File,
    )
    .expect("replacement control auth should save");
    control_auth_manager.reload().await;
    assert_eq!(
        control_auth_manager.auth_revision(),
        revision_before_account_change + 1
    );
    assert_eq!(
        control_auth_manager
            .auth_cached()
            .and_then(|auth| auth.get_account_id()),
        Some("account-b".to_string())
    );

    reconnect_after_control_auth_change(&mut websocket, &shutdown_token, active_control_auth).await;

    let connect_shutdown_token = shutdown_token.child_token();
    let backend = async {
        let (stream, request_line, headers) = accept_http_request_with_headers(&listener).await;
        assert_eq!(
            request_line,
            "POST /backend-api/wham/remote/control/server/enroll HTTP/1.1"
        );
        assert_eq!(
            headers.get(REMOTE_CONTROL_ACCOUNT_ID_HEADER),
            Some(&"account-b".to_string())
        );
        respond_with_remote_control_enrollment(
            stream,
            "srv_e_account_b",
            "env_account_b",
            "server-token-b",
        )
        .await;
        let (stream, _) = timeout(TEST_HTTP_ACCEPT_TIMEOUT, listener.accept())
            .await
            .expect("websocket handshake should arrive in time")
            .expect("listener should accept websocket handshake");
        let _websocket = accept_async(stream)
            .await
            .expect("websocket handshake should succeed");
    };
    let (connect_outcome, ()) = tokio::join!(
        websocket.connect(
            &connect_shutdown_token,
            /*app_server_client_name*/ None
        ),
        backend,
    );
    let ConnectOutcome::Connected {
        active_control_auth,
        ..
    } = connect_outcome
    else {
        panic!("control auth change should reconnect the relay");
    };
    assert_eq!(active_control_auth.account_id, "account-b");
    assert_eq!(
        active_control_auth.revision,
        control_auth_manager.auth_revision()
    );
}

#[tokio::test]
async fn active_relay_stays_connected_after_same_control_identity_token_refresh() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let codex_home = TempDir::new().expect("temp dir should create");
    let control_auth_manager =
        auth_manager_for_account(&codex_home, "account-a", "access-old").await;
    let (mut websocket, shutdown_token, _enabled_tx, _reconnect_tx) =
        active_remote_control_websocket(
            &listener,
            &codex_home,
            control_auth_manager.clone(),
            "account-a",
        )
        .await;
    let active_control_auth = ActiveControlAuth {
        account_id: "account-a".to_string(),
        revision: control_auth_manager.auth_revision(),
    };
    let revision_before_token_refresh = control_auth_manager.auth_revision();

    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json_for_account("account-a", "access-fresh"),
        AuthCredentialsStoreMode::File,
    )
    .expect("refreshed control auth should save");
    control_auth_manager.reload().await;
    assert_eq!(
        control_auth_manager.auth_revision(),
        revision_before_token_refresh + 1
    );

    let (client_websocket, _server_websocket) = connected_websocket_pair().await;
    assert!(
        timeout(
            Duration::from_millis(100),
            websocket.run_connection(
                client_websocket,
                shutdown_token.child_token(),
                active_control_auth,
            ),
        )
        .await
        .is_err(),
        "same-account token refresh must not interrupt the active relay"
    );
}

#[tokio::test]
async fn active_relay_ignores_execution_auth_manager_changes() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let control_home = TempDir::new().expect("control temp dir should create");
    let execution_home = TempDir::new().expect("execution temp dir should create");
    let control_auth_manager =
        auth_manager_for_account(&control_home, "account-a", "access-a").await;
    let execution_auth_manager =
        auth_manager_for_account(&execution_home, "account-execution", "access-old").await;
    let (mut websocket, shutdown_token, _enabled_tx, _reconnect_tx) =
        active_remote_control_websocket(
            &listener,
            &control_home,
            control_auth_manager.clone(),
            "account-a",
        )
        .await;
    let active_control_auth = ActiveControlAuth {
        account_id: "account-a".to_string(),
        revision: control_auth_manager.auth_revision(),
    };

    let control_revision = control_auth_manager.auth_revision();
    let execution_revision = execution_auth_manager.auth_revision();
    save_auth(
        execution_home.path(),
        &remote_control_auth_dot_json_for_account("account-execution-failover", "access-failover"),
        AuthCredentialsStoreMode::File,
    )
    .expect("replacement execution auth should save");
    execution_auth_manager.reload().await;
    assert_eq!(control_auth_manager.auth_revision(), control_revision);
    assert_eq!(
        execution_auth_manager.auth_revision(),
        execution_revision + 1
    );

    let (client_websocket, _server_websocket) = connected_websocket_pair().await;
    assert!(
        timeout(
            Duration::from_millis(100),
            websocket.run_connection(
                client_websocket,
                shutdown_token.child_token(),
                active_control_auth,
            ),
        )
        .await
        .is_err(),
        "execution auth changes must not interrupt the active relay"
    );
}
