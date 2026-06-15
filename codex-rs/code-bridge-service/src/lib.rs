use axum::Router;
use axum::body::Bytes;
use axum::extract::ConnectInfo;
use axum::extract::DefaultBodyLimit;
use axum::extract::Request;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::middleware;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use codex_code_bridge_protocol::AckMessage;
use codex_code_bridge_protocol::AuthProof;
use codex_code_bridge_protocol::BridgeDescriptor;
use codex_code_bridge_protocol::BridgeEndpoint;
use codex_code_bridge_protocol::BridgeEnvelope;
use codex_code_bridge_protocol::BridgeLimits;
use codex_code_bridge_protocol::BridgePayload;
use codex_code_bridge_protocol::ClientRole;
use codex_code_bridge_protocol::ErrorCode;
use codex_code_bridge_protocol::ErrorMessage;
use codex_code_bridge_protocol::HelloResponseMessage;
use codex_code_bridge_protocol::MAX_SCREENSHOT_MESSAGE_BYTES;
use codex_code_bridge_protocol::PROTOCOL_VERSION;
use codex_code_bridge_protocol::ValidationError;
use codex_code_bridge_protocol::validate_descriptor;
use codex_code_bridge_protocol::validate_envelope;
use constant_time_eq::constant_time_eq;
use rand::RngCore;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::error;

const DEFAULT_STALE_CLIENT_TIMEOUT: Duration = Duration::from_secs(45);
const DEFAULT_STALE_CLIENT_SWEEP_INTERVAL: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct BridgeServiceConfig {
    pub codex_home: PathBuf,
    pub bind_addr: SocketAddr,
    pub stale_client_timeout: Duration,
    pub stale_client_sweep_interval: Duration,
}

impl BridgeServiceConfig {
    pub fn new(codex_home: PathBuf) -> Self {
        Self {
            codex_home,
            bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            stale_client_timeout: DEFAULT_STALE_CLIENT_TIMEOUT,
            stale_client_sweep_interval: DEFAULT_STALE_CLIENT_SWEEP_INTERVAL,
        }
    }

    pub fn from_codex_home() -> io::Result<Self> {
        Ok(Self::new(codex_utils_home_dir::find_codex_home()?.into()))
    }
}

pub struct BridgeServiceHandle {
    local_addr: SocketAddr,
    descriptor_path: PathBuf,
    auth_secret: String,
    state: SharedState,
    shutdown_token: CancellationToken,
    server_task: JoinHandle<()>,
    stale_task: JoinHandle<()>,
}

impl BridgeServiceHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn endpoint_url(&self) -> String {
        format!("http://{}", self.local_addr)
    }

    pub fn descriptor_path(&self) -> &Path {
        &self.descriptor_path
    }

    pub fn auth_secret(&self) -> &str {
        &self.auth_secret
    }

    pub async fn status(&self) -> BridgeServiceStatus {
        self.state.status().await
    }

    pub async fn shutdown(self) {
        self.shutdown_token.cancel();
        let mut server_task = self.server_task;
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut server_task).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => error!("Code Bridge service task failed during shutdown: {err}"),
            Err(_) => {
                server_task.abort();
                let _ = server_task.await;
            }
        }
        self.stale_task.abort();
        let _ = self.stale_task.await;
        remove_descriptor_if_current(&self.descriptor_path, &self.auth_secret);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeServiceStatus {
    pub protocol_version: String,
    pub connected_producer_count: usize,
    pub connected_subscriber_count: usize,
    pub uptime_ms: u64,
    pub last_event_time_unix_ms: Option<u64>,
}

#[derive(Debug, Error)]
pub enum BridgeServiceError {
    #[error("Code Bridge service only supports loopback listeners, got {0}")]
    NonLoopbackBind(SocketAddr),
    #[error("invalid Code Bridge descriptor")]
    InvalidDescriptor,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub async fn start(config: BridgeServiceConfig) -> Result<BridgeServiceHandle, BridgeServiceError> {
    if !config.bind_addr.ip().is_loopback() {
        return Err(BridgeServiceError::NonLoopbackBind(config.bind_addr));
    }

    let listener = TcpListener::bind(config.bind_addr).await?;
    let local_addr = listener.local_addr()?;
    let auth_secret = generate_auth_secret();
    let descriptor_path = config
        .codex_home
        .join(codex_code_bridge_protocol::DESCRIPTOR_RELATIVE_PATH);
    let descriptor = BridgeDescriptor {
        protocol_version: PROTOCOL_VERSION.to_string(),
        endpoint: BridgeEndpoint::LoopbackHttp {
            url: format!("http://{local_addr}"),
        },
        auth_secret: auth_secret.clone(),
        pid: Some(std::process::id()),
    };
    validate_descriptor(&descriptor).map_err(|_| BridgeServiceError::InvalidDescriptor)?;
    write_descriptor(&descriptor_path, &descriptor)?;

    let state = SharedState::new(config.stale_client_timeout);
    let shutdown_token = CancellationToken::new();
    let app_state = AppState {
        auth_secret: Arc::new(auth_secret.clone()),
        state: state.clone(),
    };
    let router = Router::new()
        .route("/readyz", get(readyz_handler))
        .route("/status", get(status_handler))
        .route("/message", post(message_handler))
        .layer(middleware::from_fn_with_state(
            app_state.clone(),
            require_auth,
        ))
        .layer(DefaultBodyLimit::max(MAX_SCREENSHOT_MESSAGE_BYTES))
        .with_state(app_state);
    let server_shutdown = shutdown_token.clone();
    let server = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        server_shutdown.cancelled().await;
    });
    let server_task = tokio::spawn(async move {
        if let Err(err) = server.await {
            error!("Code Bridge service failed: {err}");
        }
    });

    let stale_state = state.clone();
    let stale_shutdown = shutdown_token.clone();
    let stale_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(config.stale_client_sweep_interval);
        loop {
            tokio::select! {
                _ = stale_shutdown.cancelled() => break,
                _ = interval.tick() => stale_state.expire_stale_clients().await,
            }
        }
    });

    Ok(BridgeServiceHandle {
        local_addr,
        descriptor_path,
        auth_secret,
        state,
        shutdown_token,
        server_task,
        stale_task,
    })
}

#[derive(Clone)]
struct AppState {
    auth_secret: Arc<String>,
    state: SharedState,
}

#[derive(Clone)]
struct SharedState {
    inner: Arc<Mutex<ServiceState>>,
}

impl SharedState {
    fn new(stale_client_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ServiceState {
                started_at: Instant::now(),
                clients: HashMap::new(),
                stale_client_timeout,
                last_event_time_unix_ms: None,
            })),
        }
    }

    async fn handle_envelope(&self, envelope: &BridgeEnvelope) -> BridgePayload {
        let mut state = self.inner.lock().await;
        match &envelope.payload {
            BridgePayload::Hello(message) => {
                state.clients.insert(
                    message.client_id.clone(),
                    ClientState {
                        role: message.role,
                        last_seen: Instant::now(),
                    },
                );
                BridgePayload::HelloResponse(HelloResponseMessage {
                    accepted: true,
                    protocol_version: PROTOCOL_VERSION.to_string(),
                    granted_capabilities: message.requested_capabilities.clone(),
                    limits: BridgeLimits::default(),
                    error: None,
                })
            }
            BridgePayload::Heartbeat(message) => {
                state.touch_client(&message.client_id);
                ack_for(envelope)
            }
            BridgePayload::Event(message) => {
                state.touch_client(&message.client_id);
                state.last_event_time_unix_ms = Some(now_unix_ms());
                ack_for(envelope)
            }
            BridgePayload::Subscribe(message) => {
                state.clients.insert(
                    message.subscriber_id.clone(),
                    ClientState {
                        role: ClientRole::Subscriber,
                        last_seen: Instant::now(),
                    },
                );
                ack_for(envelope)
            }
            BridgePayload::ScreenshotResponse(message) => {
                state.last_event_time_unix_ms = Some(now_unix_ms());
                BridgePayload::Ack(AckMessage {
                    message_id: message.request_id.clone(),
                })
            }
            BridgePayload::ControlResponse(message) => {
                state.last_event_time_unix_ms = Some(now_unix_ms());
                BridgePayload::Ack(AckMessage {
                    message_id: message.request_id.clone(),
                })
            }
            BridgePayload::HelloResponse(_)
            | BridgePayload::Ack(_)
            | BridgePayload::Error(_)
            | BridgePayload::ScreenshotRequest(_)
            | BridgePayload::ControlRequest(_) => ack_for(envelope),
        }
    }

    async fn status(&self) -> BridgeServiceStatus {
        let state = self.inner.lock().await;
        state.status()
    }

    async fn expire_stale_clients(&self) {
        let mut state = self.inner.lock().await;
        state.expire_stale_clients();
    }
}

struct ServiceState {
    started_at: Instant,
    clients: HashMap<String, ClientState>,
    stale_client_timeout: Duration,
    last_event_time_unix_ms: Option<u64>,
}

impl ServiceState {
    fn status(&self) -> BridgeServiceStatus {
        BridgeServiceStatus {
            protocol_version: PROTOCOL_VERSION.to_string(),
            connected_producer_count: self
                .clients
                .values()
                .filter(|client| client.role == ClientRole::Producer)
                .count(),
            connected_subscriber_count: self
                .clients
                .values()
                .filter(|client| client.role == ClientRole::Subscriber)
                .count(),
            uptime_ms: self
                .started_at
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            last_event_time_unix_ms: self.last_event_time_unix_ms,
        }
    }

    fn touch_client(&mut self, client_id: &str) {
        if let Some(client) = self.clients.get_mut(client_id) {
            client.last_seen = Instant::now();
        }
    }

    fn expire_stale_clients(&mut self) {
        let now = Instant::now();
        self.clients
            .retain(|_, client| now.duration_since(client.last_seen) <= self.stale_client_timeout);
    }
}

struct ClientState {
    role: ClientRole,
    last_seen: Instant,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeMessageResponse {
    pub payload: BridgePayload,
}

async fn readyz_handler() -> StatusCode {
    StatusCode::OK
}

async fn status_handler(State(state): State<AppState>) -> axum::Json<BridgeServiceStatus> {
    axum::Json(state.state.status().await)
}

async fn message_handler(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<axum::Json<BridgeMessageResponse>, BridgeHttpError> {
    if body.len() > MAX_SCREENSHOT_MESSAGE_BYTES {
        return Err(BridgeHttpError::PayloadTooLarge {
            limit: MAX_SCREENSHOT_MESSAGE_BYTES,
            actual: body.len(),
        });
    }

    let envelope: BridgeEnvelope = serde_json::from_slice(&body).map_err(|err| {
        BridgeHttpError::InvalidPayload(format!("invalid Code Bridge envelope JSON: {err}"))
    })?;
    validate_envelope(&envelope, body.len()).map_err(BridgeHttpError::Validation)?;

    if let BridgePayload::Hello(message) = &envelope.payload {
        validate_payload_auth(&message.auth, state.auth_secret.as_str())?;
    }

    let payload = state.state.handle_envelope(&envelope).await;
    Ok(axum::Json(BridgeMessageResponse { payload }))
}

async fn require_auth(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Result<Response, BridgeHttpError> {
    if !peer_addr.ip().is_loopback() {
        return Err(BridgeHttpError::Forbidden(
            "Code Bridge service only accepts loopback clients".to_string(),
        ));
    }
    let token = bearer_token_from_headers(request.headers())?;
    if !constant_time_eq(token.as_bytes(), state.auth_secret.as_bytes()) {
        return Err(BridgeHttpError::Unauthorized(
            "invalid Code Bridge bearer token".to_string(),
        ));
    }
    if let Some(content_length) = request
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        && content_length > MAX_SCREENSHOT_MESSAGE_BYTES
    {
        return Err(BridgeHttpError::PayloadTooLarge {
            limit: MAX_SCREENSHOT_MESSAGE_BYTES,
            actual: content_length,
        });
    }
    Ok(next.run(request).await)
}

fn bearer_token_from_headers(headers: &HeaderMap) -> Result<&str, BridgeHttpError> {
    let header = headers
        .get(AUTHORIZATION)
        .ok_or_else(|| {
            BridgeHttpError::Unauthorized("missing Code Bridge bearer token".to_string())
        })?
        .to_str()
        .map_err(|_| BridgeHttpError::Unauthorized("invalid authorization header".to_string()))?;
    let Some((scheme, token)) = header.split_once(' ') else {
        return Err(BridgeHttpError::Unauthorized(
            "invalid authorization header".to_string(),
        ));
    };
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return Err(BridgeHttpError::Unauthorized(
            "invalid authorization header".to_string(),
        ));
    }
    let token = token.trim();
    if token.is_empty() {
        return Err(BridgeHttpError::Unauthorized(
            "invalid authorization header".to_string(),
        ));
    }
    Ok(token)
}

fn validate_payload_auth(auth: &AuthProof, expected_secret: &str) -> Result<(), BridgeHttpError> {
    match auth {
        AuthProof::LocalSecret { secret }
            if constant_time_eq(secret.as_bytes(), expected_secret.as_bytes()) =>
        {
            Ok(())
        }
        AuthProof::LocalSecret { .. } => Err(BridgeHttpError::Unauthorized(
            "invalid Code Bridge payload secret".to_string(),
        )),
    }
}

#[derive(Debug)]
enum BridgeHttpError {
    Unauthorized(String),
    Forbidden(String),
    InvalidPayload(String),
    PayloadTooLarge { limit: usize, actual: usize },
    Validation(ValidationError),
}

impl IntoResponse for BridgeHttpError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            BridgeHttpError::Unauthorized(message) => (
                StatusCode::UNAUTHORIZED,
                ErrorMessage {
                    code: ErrorCode::AuthRejected,
                    message,
                },
            ),
            BridgeHttpError::Forbidden(message) => (
                StatusCode::FORBIDDEN,
                ErrorMessage {
                    code: ErrorCode::AuthRejected,
                    message,
                },
            ),
            BridgeHttpError::InvalidPayload(message) => (
                StatusCode::BAD_REQUEST,
                ErrorMessage {
                    code: ErrorCode::InvalidPayload,
                    message,
                },
            ),
            BridgeHttpError::PayloadTooLarge { limit, actual } => (
                StatusCode::PAYLOAD_TOO_LARGE,
                ErrorMessage {
                    code: ErrorCode::PayloadTooLarge,
                    message: format!(
                        "Code Bridge payload is {actual} bytes, over the {limit} byte limit"
                    ),
                },
            ),
            BridgeHttpError::Validation(error) => validation_http_error(error),
        };
        (status, axum::Json(error)).into_response()
    }
}

fn validation_http_error(error: ValidationError) -> (StatusCode, ErrorMessage) {
    match error {
        ValidationError::AuthRequired => (
            StatusCode::UNAUTHORIZED,
            ErrorMessage {
                code: ErrorCode::AuthRequired,
                message: "Code Bridge payload auth is required".to_string(),
            },
        ),
        ValidationError::UnsupportedProtocolVersion => (
            StatusCode::BAD_REQUEST,
            ErrorMessage {
                code: ErrorCode::UnsupportedProtocolVersion,
                message: "unsupported Code Bridge protocol version".to_string(),
            },
        ),
        ValidationError::CapabilityDenied => (
            StatusCode::FORBIDDEN,
            ErrorMessage {
                code: ErrorCode::CapabilityDenied,
                message: "Code Bridge capability denied".to_string(),
            },
        ),
        ValidationError::PayloadTooLarge { limit, actual } => (
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorMessage {
                code: ErrorCode::PayloadTooLarge,
                message: format!(
                    "Code Bridge payload is {actual} bytes, over the {limit} byte limit"
                ),
            },
        ),
        ValidationError::TimeoutTooLarge {
            limit_ms,
            actual_ms,
        } => (
            StatusCode::BAD_REQUEST,
            ErrorMessage {
                code: ErrorCode::Timeout,
                message: format!(
                    "Code Bridge timeout is {actual_ms}ms, over the {limit_ms}ms limit"
                ),
            },
        ),
        ValidationError::InvalidEndpoint | ValidationError::InvalidDimensions => (
            StatusCode::BAD_REQUEST,
            ErrorMessage {
                code: ErrorCode::InvalidPayload,
                message: "invalid Code Bridge payload".to_string(),
            },
        ),
    }
}

fn ack_for(envelope: &BridgeEnvelope) -> BridgePayload {
    BridgePayload::Ack(AckMessage {
        message_id: envelope.message_id.clone(),
    })
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn generate_auth_secret() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_descriptor(path: &Path, descriptor: &BridgeDescriptor) -> Result<(), BridgeServiceError> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Code Bridge descriptor path {} has no parent",
                path.display()
            ),
        )
    })?;
    create_owner_only_dir_all(parent)?;
    set_owner_only_dir(parent)?;

    let json = serde_json::to_vec_pretty(descriptor)?;
    write_owner_only_file(path, &json)?;
    Ok(())
}

#[cfg(unix)]
fn create_owner_only_dir_all(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_owner_only_dir_all(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)
}

fn write_owner_only_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Code Bridge descriptor path {} has no parent",
                path.display()
            ),
        )
    })?;
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("descriptor"),
        generate_auth_secret()
    ));
    let result = (|| {
        let mut file = open_owner_only_new_file(&temp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp_path, path)?;
        set_owner_only_file(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

#[cfg(unix)]
fn open_owner_only_new_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_owner_only_new_file(path: &Path) -> io::Result<std::fs::File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
fn set_owner_only_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_owner_only_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn remove_descriptor_if_current(path: &Path, auth_secret: &str) {
    let Ok(raw) = std::fs::read(path) else {
        return;
    };
    let Ok(descriptor) = serde_json::from_slice::<BridgeDescriptor>(&raw) else {
        return;
    };
    if constant_time_eq(descriptor.auth_secret.as_bytes(), auth_secret.as_bytes())
        && descriptor.pid == Some(std::process::id())
    {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_code_bridge_protocol::BridgeEvent;
    use codex_code_bridge_protocol::CapabilitySet;
    use codex_code_bridge_protocol::ClientMetadata;
    use codex_code_bridge_protocol::ConsoleEvent;
    use codex_code_bridge_protocol::ConsoleLevel;
    use codex_code_bridge_protocol::ControlStatus;
    use codex_code_bridge_protocol::EventPublishMessage;
    use codex_code_bridge_protocol::HeartbeatMessage;
    use codex_code_bridge_protocol::HelloMessage;
    use codex_code_bridge_protocol::ScreenshotMediaType;
    use codex_code_bridge_protocol::ScreenshotPayload;
    use codex_code_bridge_protocol::ScreenshotResponseMessage;
    use codex_code_bridge_protocol::SourceKind;
    use http::header::CONTENT_LENGTH;
    use reqwest::Client;
    use tempfile::TempDir;

    #[tokio::test]
    async fn start_writes_loopback_descriptor_with_owner_only_permissions() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let descriptor_path = service.handle.descriptor_path().to_path_buf();
        let descriptor = read_descriptor(&descriptor_path);

        assert_eq!(descriptor.protocol_version, PROTOCOL_VERSION);
        assert_eq!(descriptor.auth_secret, service.handle.auth_secret());
        assert_eq!(descriptor.pid, Some(std::process::id()));
        assert_eq!(validate_descriptor(&descriptor), Ok(()));
        match descriptor.endpoint {
            BridgeEndpoint::LoopbackHttp { url } => {
                assert_eq!(url, service.handle.endpoint_url());
            }
            _ => panic!("expected loopback HTTP descriptor"),
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let dir_mode = std::fs::metadata(descriptor_path.parent().expect("descriptor parent"))
                .expect("descriptor dir metadata")
                .permissions()
                .mode()
                & 0o777;
            let file_mode = std::fs::metadata(&descriptor_path)
                .expect("descriptor file metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
            assert_eq!(file_mode, 0o600);
        }

        service.handle.shutdown().await;
        assert!(!descriptor_path.exists());
    }

    #[tokio::test]
    async fn rejects_missing_and_invalid_auth_before_json_deserialization() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let url = format!("{}/message", service.handle.endpoint_url());

        let missing = client
            .post(&url)
            .body("not json")
            .send()
            .await
            .expect("missing auth response");
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let invalid = client
            .post(&url)
            .bearer_auth("wrong")
            .body("not json")
            .send()
            .await
            .expect("invalid auth response");
        assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);

        let valid_auth_invalid_json = client
            .post(&url)
            .bearer_auth(service.handle.auth_secret())
            .body("not json")
            .send()
            .await
            .expect("invalid json response");
        assert_eq!(valid_auth_invalid_json.status(), StatusCode::BAD_REQUEST);

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn status_tracks_clients_heartbeats_events_and_stale_timeout() {
        let service =
            start_test_service(Duration::from_millis(80), Duration::from_millis(20)).await;
        let client = Client::new();

        post_envelope(
            &client,
            &service.handle,
            hello_envelope(
                "producer-1",
                service.handle.auth_secret(),
                ClientRole::Producer,
                CapabilitySet {
                    publish_events: true,
                    ..CapabilitySet::default()
                },
            ),
        )
        .await;
        post_envelope(
            &client,
            &service.handle,
            hello_envelope(
                "subscriber-1",
                service.handle.auth_secret(),
                ClientRole::Subscriber,
                CapabilitySet {
                    subscribe_events: true,
                    ..CapabilitySet::default()
                },
            ),
        )
        .await;

        let status = service.handle.status().await;
        assert_eq!(status.protocol_version, PROTOCOL_VERSION);
        assert_eq!(status.connected_producer_count, 1);
        assert_eq!(status.connected_subscriber_count, 1);
        assert!(status.uptime_ms < u64::MAX);
        assert_eq!(status.last_event_time_unix_ms, None);

        post_envelope(
            &client,
            &service.handle,
            envelope(
                "event-1",
                BridgePayload::Event(EventPublishMessage {
                    client_id: "producer-1".to_string(),
                    event_id: "console-1".to_string(),
                    event: BridgeEvent::Console(ConsoleEvent {
                        level: ConsoleLevel::Info,
                        text: "ready".to_string(),
                    }),
                }),
            ),
        )
        .await;
        assert!(
            service
                .handle
                .status()
                .await
                .last_event_time_unix_ms
                .is_some()
        );

        tokio::time::sleep(Duration::from_millis(50)).await;
        post_envelope(
            &client,
            &service.handle,
            envelope(
                "heartbeat-1",
                BridgePayload::Heartbeat(HeartbeatMessage {
                    client_id: "subscriber-1".to_string(),
                    sequence: 1,
                }),
            ),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        let status = service.handle.status().await;
        assert_eq!(status.connected_producer_count, 0);
        assert_eq!(status.connected_subscriber_count, 1);

        tokio::time::sleep(Duration::from_millis(80)).await;
        let status = service.handle.status().await;
        assert_eq!(status.connected_producer_count, 0);
        assert_eq!(status.connected_subscriber_count, 0);

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn rejects_payload_size_before_reading_body_when_content_length_is_large() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let response = client
            .post(format!("{}/message", service.handle.endpoint_url()))
            .bearer_auth(service.handle.auth_secret())
            .header(
                CONTENT_LENGTH,
                (MAX_SCREENSHOT_MESSAGE_BYTES + 1).to_string(),
            )
            .body("{}")
            .send()
            .await
            .expect("oversize response");

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn accepts_protocol_valid_large_screenshot_response() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let envelope = envelope(
            "screenshot-response-1",
            BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
                request_id: "screenshot-request-1".to_string(),
                status: ControlStatus::Ok,
                screenshot: Some(ScreenshotPayload {
                    width: 1,
                    height: 1,
                    media_type: ScreenshotMediaType::Png,
                    data_base64: "x".repeat(codex_code_bridge_protocol::MAX_SCREENSHOT_BYTES),
                }),
                error: None,
            }),
        );

        post_envelope(&client, &service.handle, envelope).await;
        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn refuses_non_loopback_bind_address() {
        let temp = TempDir::new().expect("temp home");
        let mut config = BridgeServiceConfig::new(temp.path().to_path_buf());
        config.bind_addr = SocketAddr::from(([0, 0, 0, 0], 0));

        let err = match start(config).await {
            Ok(handle) => {
                handle.shutdown().await;
                panic!("non-loopback bind should fail");
            }
            Err(err) => err,
        };
        assert!(matches!(err, BridgeServiceError::NonLoopbackBind(_)));
    }

    struct TestService {
        _temp: TempDir,
        handle: BridgeServiceHandle,
    }

    async fn start_test_service(
        stale_client_timeout: Duration,
        stale_client_sweep_interval: Duration,
    ) -> TestService {
        let temp = TempDir::new().expect("temp home");
        let mut config = BridgeServiceConfig::new(temp.path().to_path_buf());
        config.stale_client_timeout = stale_client_timeout;
        config.stale_client_sweep_interval = stale_client_sweep_interval;
        let handle = start(config).await.expect("start service");
        TestService {
            _temp: temp,
            handle,
        }
    }

    fn read_descriptor(path: &Path) -> BridgeDescriptor {
        let raw = std::fs::read(path).expect("read descriptor");
        serde_json::from_slice(&raw).expect("parse descriptor")
    }

    async fn post_envelope(
        client: &Client,
        handle: &BridgeServiceHandle,
        envelope: BridgeEnvelope,
    ) {
        let response = client
            .post(format!("{}/message", handle.endpoint_url()))
            .bearer_auth(handle.auth_secret())
            .json(&envelope)
            .send()
            .await
            .expect("message response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    fn hello_envelope(
        client_id: &str,
        auth_secret: &str,
        role: ClientRole,
        requested_capabilities: CapabilitySet,
    ) -> BridgeEnvelope {
        envelope(
            &format!("hello-{client_id}"),
            BridgePayload::Hello(HelloMessage {
                client_id: client_id.to_string(),
                role,
                auth: AuthProof::LocalSecret {
                    secret: auth_secret.to_string(),
                },
                requested_capabilities,
                metadata: ClientMetadata {
                    source_kind: SourceKind::TestFixture,
                    ..ClientMetadata::default()
                },
            }),
        )
    }

    fn envelope(message_id: &str, payload: BridgePayload) -> BridgeEnvelope {
        BridgeEnvelope {
            protocol_version: PROTOCOL_VERSION.to_string(),
            message_id: message_id.to_string(),
            timestamp_unix_ms: now_unix_ms(),
            payload,
        }
    }
}
