use axum::Router;
use axum::body::Bytes;
use axum::extract::ConnectInfo;
use axum::extract::DefaultBodyLimit;
use axum::extract::Path as AxumPath;
use axum::extract::Request;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::Method;
use axum::http::StatusCode;
use axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS;
use axum::http::header::ACCESS_CONTROL_ALLOW_METHODS;
use axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN;
use axum::http::header::ACCESS_CONTROL_MAX_AGE;
use axum::http::header::AUTHORIZATION;
use axum::http::header::ORIGIN;
use axum::http::header::VARY;
use axum::middleware;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::response::sse::Event as SseEvent;
use axum::response::sse::KeepAlive;
use axum::response::sse::Sse;
use axum::routing::get;
use axum::routing::post;
use codex_code_bridge_protocol::AckMessage;
use codex_code_bridge_protocol::AuthProof;
use codex_code_bridge_protocol::BridgeDescriptor;
use codex_code_bridge_protocol::BridgeEndpoint;
use codex_code_bridge_protocol::BridgeEnvelope;
use codex_code_bridge_protocol::BridgeEvent;
use codex_code_bridge_protocol::BridgeLimits;
use codex_code_bridge_protocol::BridgePayload;
use codex_code_bridge_protocol::BridgeServiceStatus;
use codex_code_bridge_protocol::CLIENT_SESSION_HEADER;
use codex_code_bridge_protocol::CapabilitySet;
use codex_code_bridge_protocol::ClientRole;
use codex_code_bridge_protocol::ControlCommand;
use codex_code_bridge_protocol::ErrorCode;
use codex_code_bridge_protocol::ErrorMessage;
use codex_code_bridge_protocol::EventKind;
use codex_code_bridge_protocol::HelloResponseMessage;
use codex_code_bridge_protocol::MAX_RETAINED_EVENTS;
use codex_code_bridge_protocol::MAX_SCREENSHOT_MESSAGE_BYTES;
use codex_code_bridge_protocol::PROTOCOL_VERSION;
use codex_code_bridge_protocol::SubscribeMessage;
use codex_code_bridge_protocol::SubscriptionFilter;
use codex_code_bridge_protocol::ValidationError;
use codex_code_bridge_protocol::event_kind;
use codex_code_bridge_protocol::message_family;
use codex_code_bridge_protocol::validate_descriptor;
use codex_code_bridge_protocol::validate_envelope;
use codex_code_bridge_protocol::validate_event_capabilities;
use constant_time_eq::constant_time_eq;
use rand::RngCore;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::net::IpAddr;
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
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::error;

pub use codex_code_bridge_protocol::BridgeMessageResponse;
pub use codex_code_bridge_protocol::BridgeSseMessage;

const DEFAULT_STALE_CLIENT_TIMEOUT: Duration = Duration::from_secs(45);
const DEFAULT_STALE_CLIENT_SWEEP_INTERVAL: Duration = Duration::from_secs(15);
const EVENT_STREAM_TOUCH_INTERVAL: Duration = Duration::from_secs(10);
const MAX_PENDING_REQUESTS: usize = 256;
const MAX_RETAINED_DELIVERY_BYTES: usize = 8 * 1024 * 1024;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const EVENT_WAKE_CHANNEL_CAPACITY: usize = 64;
const CORS_ALLOW_HEADERS: &str =
    "authorization, content-type, last-event-id, x-code-bridge-client-session";
const CORS_ALLOW_METHODS: &str = "GET, POST, OPTIONS";
const CORS_MAX_AGE_SECONDS: &str = "600";
const CORS_VARY_HEADERS: &str =
    "origin, access-control-request-method, access-control-request-headers";

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
        .route("/events/{client_id}", get(events_handler))
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
    delivery_tx: broadcast::Sender<Arc<BridgeDelivery>>,
}

#[derive(Clone, Copy)]
struct ValidatedClientSession<'a> {
    client_id: &'a str,
    client_session_token: &'a str,
}

impl SharedState {
    fn new(stale_client_timeout: Duration) -> Self {
        let (delivery_tx, _) = broadcast::channel(EVENT_WAKE_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(Mutex::new(ServiceState {
                started_at: Instant::now(),
                clients: HashMap::new(),
                retained_deliveries: VecDeque::new(),
                retained_delivery_bytes: 0,
                next_delivery_sequence: 1,
                pending_requests: HashMap::new(),
                stale_client_timeout,
                last_event_time_unix_ms: None,
            })),
            delivery_tx,
        }
    }

    async fn handle_envelope(
        &self,
        envelope: &BridgeEnvelope,
        client_session: Option<ValidatedClientSession<'_>>,
    ) -> Result<BridgePayload, BridgeHttpError> {
        let mut state = self.inner.lock().await;
        if let Some(client_session) = client_session {
            state.validate_client_session(
                client_session.client_id,
                client_session.client_session_token,
            )?;
        }
        match &envelope.payload {
            BridgePayload::Hello(message) => {
                let client_session_token = generate_auth_secret();
                let granted_capabilities = message.requested_capabilities.for_role(message.role);
                if let Some(client) = state.clients.get(&message.client_id)
                    && client.role != message.role
                {
                    return Ok(error_payload(
                        ErrorCode::InvalidPayload,
                        format!(
                            "Code Bridge client {} is already registered with a different role",
                            message.client_id
                        ),
                    ));
                }
                let filter = state
                    .clients
                    .get(&message.client_id)
                    .map(|client| client.filter.clone())
                    .unwrap_or_default();
                state.clients.insert(
                    message.client_id.clone(),
                    ClientState {
                        role: message.role,
                        capabilities: granted_capabilities.clone(),
                        filter,
                        session_token: client_session_token.clone(),
                        last_seen: Instant::now(),
                    },
                );
                Ok(BridgePayload::HelloResponse(HelloResponseMessage {
                    accepted: true,
                    protocol_version: PROTOCOL_VERSION.to_string(),
                    granted_capabilities,
                    client_session_token: Some(client_session_token),
                    limits: BridgeLimits::default(),
                    error: None,
                }))
            }
            BridgePayload::Heartbeat(message) => {
                state.touch_client(&message.client_id);
                Ok(ack_for(envelope))
            }
            BridgePayload::Event(message) => {
                let mut outgoing = Vec::new();
                let payload = state.handle_event(envelope, message, &mut outgoing);
                drop(state);
                self.publish_deliveries(outgoing);
                Ok(payload)
            }
            BridgePayload::Subscribe(message) => Ok(state.handle_subscribe(envelope, message)),
            BridgePayload::ScreenshotRequest(message) => {
                let mut outgoing = Vec::new();
                let payload = state.handle_screenshot_request(envelope, message, &mut outgoing);
                drop(state);
                self.publish_deliveries(outgoing);
                Ok(payload)
            }
            BridgePayload::ScreenshotResponse(message) => {
                let mut outgoing = Vec::new();
                let payload = state.handle_screenshot_response(envelope, message, &mut outgoing);
                drop(state);
                self.publish_deliveries(outgoing);
                Ok(payload)
            }
            BridgePayload::ControlRequest(message) => {
                let mut outgoing = Vec::new();
                let payload = state.handle_control_request(envelope, message, &mut outgoing);
                drop(state);
                self.publish_deliveries(outgoing);
                Ok(payload)
            }
            BridgePayload::ControlResponse(message) => {
                let mut outgoing = Vec::new();
                let payload = state.handle_control_response(envelope, message, &mut outgoing);
                drop(state);
                self.publish_deliveries(outgoing);
                Ok(payload)
            }
            BridgePayload::HelloResponse(_) | BridgePayload::Ack(_) | BridgePayload::Error(_) => {
                Ok(ack_for(envelope))
            }
        }
    }

    async fn open_event_stream(
        &self,
        client_id: &str,
        client_session_token: &str,
        last_seen_sequence: u64,
    ) -> Result<EventStreamState, BridgeHttpError> {
        let delivery_rx = self.delivery_tx.subscribe();
        let mut state = self.inner.lock().await;
        let Some(client) = state.clients.get_mut(client_id) else {
            return Err(BridgeHttpError::InvalidPayload(format!(
                "unknown Code Bridge client {client_id}"
            )));
        };
        if !constant_time_eq(
            client.session_token.as_bytes(),
            client_session_token.as_bytes(),
        ) {
            return Err(BridgeHttpError::Unauthorized(
                "invalid Code Bridge client session token".to_string(),
            ));
        }
        client.last_seen = Instant::now();
        let client = client.clone();
        let replay_cursor = state
            .retained_deliveries
            .back()
            .map(|delivery| delivery.sequence)
            .unwrap_or(0)
            .max(last_seen_sequence);
        let replay = state
            .retained_deliveries
            .iter()
            .filter(|delivery| delivery.sequence > last_seen_sequence)
            .filter(|delivery| delivery.matches_client(client_id, &client))
            .cloned()
            .collect();

        Ok(EventStreamState {
            client_id: client_id.to_string(),
            client_session_token: client_session_token.to_string(),
            cursor: replay_cursor,
            replay,
            delivery_rx,
        })
    }

    async fn client_session_matches_delivery(
        &self,
        client_id: &str,
        client_session_token: &str,
        delivery: &BridgeDelivery,
    ) -> bool {
        let mut state = self.inner.lock().await;
        let Some(client) = state.clients.get_mut(client_id) else {
            return false;
        };
        if !constant_time_eq(
            client.session_token.as_bytes(),
            client_session_token.as_bytes(),
        ) {
            return false;
        }
        client.last_seen = Instant::now();
        delivery.matches_client(client_id, client)
    }

    async fn touch_client_session(&self, client_id: &str, client_session_token: &str) {
        let mut state = self.inner.lock().await;
        state.touch_client_session(client_id, client_session_token);
    }

    fn publish_deliveries(&self, deliveries: Vec<Arc<BridgeDelivery>>) {
        for delivery in deliveries {
            let _ = self.delivery_tx.send(delivery);
        }
    }

    async fn status(&self) -> BridgeServiceStatus {
        let state = self.inner.lock().await;
        state.status()
    }

    async fn expire_stale_clients(&self) {
        let mut state = self.inner.lock().await;
        let mut outgoing = Vec::new();
        state.expire_stale_clients();
        state.expire_pending_requests(&mut outgoing);
        drop(state);
        self.publish_deliveries(outgoing);
    }
}

struct ServiceState {
    started_at: Instant,
    clients: HashMap<String, ClientState>,
    retained_deliveries: VecDeque<Arc<BridgeDelivery>>,
    retained_delivery_bytes: usize,
    next_delivery_sequence: u64,
    pending_requests: HashMap<String, PendingRequest>,
    stale_client_timeout: Duration,
    last_event_time_unix_ms: Option<u64>,
}

impl ServiceState {
    fn handle_event(
        &mut self,
        envelope: &BridgeEnvelope,
        message: &codex_code_bridge_protocol::EventPublishMessage,
        outgoing: &mut Vec<Arc<BridgeDelivery>>,
    ) -> BridgePayload {
        let Some(client) = self.clients.get_mut(&message.client_id) else {
            return error_payload(
                ErrorCode::InvalidPayload,
                format!("unknown Code Bridge client {}", message.client_id),
            );
        };
        client.last_seen = Instant::now();
        if let Err(error) = validate_event_capabilities(&client.capabilities, &message.event) {
            return validation_error_payload(error);
        }
        self.last_event_time_unix_ms = Some(now_unix_ms());
        self.enqueue_delivery(
            envelope.clone(),
            DeliveryRoute::Subscribers {
                source_client_id: message.client_id.clone(),
                event_kind: event_kind(&message.event),
                event: message.event.clone(),
            },
            true,
            outgoing,
        );
        ack_for(envelope)
    }

    fn handle_subscribe(
        &mut self,
        envelope: &BridgeEnvelope,
        message: &SubscribeMessage,
    ) -> BridgePayload {
        let Some(client) = self.clients.get_mut(&message.subscriber_id) else {
            return error_payload(
                ErrorCode::InvalidPayload,
                format!("unknown Code Bridge subscriber {}", message.subscriber_id),
            );
        };
        client.last_seen = Instant::now();
        if !client.capabilities.subscribe_events {
            return validation_error_payload(ValidationError::CapabilityDenied);
        }
        client.filter = message.filter.clone();
        ack_for(envelope)
    }

    fn handle_screenshot_request(
        &mut self,
        envelope: &BridgeEnvelope,
        message: &codex_code_bridge_protocol::ScreenshotRequestMessage,
        outgoing: &mut Vec<Arc<BridgeDelivery>>,
    ) -> BridgePayload {
        if let Some(error) = self.validate_request_participants(
            &message.requester_client_id,
            &message.target_client_id,
            |capabilities| capabilities.request_screenshot,
            |capabilities| capabilities.provide_screenshot,
        ) {
            return error;
        }
        if self.pending_requests.len() >= MAX_PENDING_REQUESTS {
            return error_payload(
                ErrorCode::InvalidPayload,
                format!("Code Bridge has {MAX_PENDING_REQUESTS} pending requests"),
            );
        }
        if self.pending_requests.contains_key(&message.request_id) {
            return error_payload(
                ErrorCode::InvalidPayload,
                format!(
                    "Code Bridge request {} is already pending",
                    message.request_id
                ),
            );
        }
        self.pending_requests.insert(
            message.request_id.clone(),
            PendingRequest {
                requester_client_id: message.requester_client_id.clone(),
                target_client_id: message.target_client_id.clone(),
                kind: PendingRequestKind::Screenshot,
                deadline: Instant::now() + Duration::from_millis(message.timeout_ms),
            },
        );
        self.enqueue_delivery(
            envelope.clone(),
            DeliveryRoute::Target(message.target_client_id.clone()),
            true,
            outgoing,
        );
        ack_for(envelope)
    }

    fn handle_screenshot_response(
        &mut self,
        envelope: &BridgeEnvelope,
        message: &codex_code_bridge_protocol::ScreenshotResponseMessage,
        outgoing: &mut Vec<Arc<BridgeDelivery>>,
    ) -> BridgePayload {
        let request_id = &message.request_id;
        let Some(pending) = self.pending_requests.remove(request_id) else {
            return unknown_request_error(request_id);
        };
        if pending.kind != PendingRequestKind::Screenshot {
            self.pending_requests.insert(request_id.clone(), pending);
            return error_payload(
                ErrorCode::InvalidPayload,
                format!("Code Bridge request {request_id} is not a screenshot request"),
            );
        }
        if pending.target_client_id != message.responding_client_id {
            self.pending_requests.insert(request_id.clone(), pending);
            return validation_error_payload(ValidationError::CapabilityDenied);
        }
        self.touch_client(&pending.target_client_id);
        self.last_event_time_unix_ms = Some(now_unix_ms());
        self.enqueue_delivery(
            envelope.clone(),
            DeliveryRoute::Target(pending.requester_client_id),
            true,
            outgoing,
        );
        BridgePayload::Ack(AckMessage {
            message_id: request_id.to_string(),
        })
    }

    fn handle_control_request(
        &mut self,
        envelope: &BridgeEnvelope,
        message: &codex_code_bridge_protocol::ControlRequestMessage,
        outgoing: &mut Vec<Arc<BridgeDelivery>>,
    ) -> BridgePayload {
        let Some(requester) = self.clients.get_mut(&message.requester_client_id) else {
            return error_payload(
                ErrorCode::InvalidPayload,
                format!(
                    "unknown Code Bridge requester {}",
                    message.requester_client_id
                ),
            );
        };
        requester.last_seen = Instant::now();
        if let Err(error) = codex_code_bridge_protocol::validate_control_capabilities(
            &requester.capabilities,
            &message.command,
        ) {
            return validation_error_payload(error);
        }

        let Some(target) = self.clients.get_mut(&message.target_client_id) else {
            return error_payload(
                ErrorCode::InvalidPayload,
                format!("unknown Code Bridge target {}", message.target_client_id),
            );
        };
        target.last_seen = Instant::now();
        if !target_can_run_command(&target.capabilities, &message.command) {
            return validation_error_payload(ValidationError::CapabilityDenied);
        }

        if self.pending_requests.len() >= MAX_PENDING_REQUESTS {
            return error_payload(
                ErrorCode::InvalidPayload,
                format!("Code Bridge has {MAX_PENDING_REQUESTS} pending requests"),
            );
        }
        if self.pending_requests.contains_key(&message.request_id) {
            return error_payload(
                ErrorCode::InvalidPayload,
                format!(
                    "Code Bridge request {} is already pending",
                    message.request_id
                ),
            );
        }

        self.pending_requests.insert(
            message.request_id.clone(),
            PendingRequest {
                requester_client_id: message.requester_client_id.clone(),
                target_client_id: message.target_client_id.clone(),
                kind: PendingRequestKind::Control,
                deadline: Instant::now() + Duration::from_millis(message.timeout_ms),
            },
        );
        self.enqueue_delivery(
            envelope.clone(),
            DeliveryRoute::Target(message.target_client_id.clone()),
            true,
            outgoing,
        );
        ack_for(envelope)
    }

    fn handle_control_response(
        &mut self,
        envelope: &BridgeEnvelope,
        message: &codex_code_bridge_protocol::ControlResponseMessage,
        outgoing: &mut Vec<Arc<BridgeDelivery>>,
    ) -> BridgePayload {
        let request_id = &message.request_id;
        let Some(pending) = self.pending_requests.remove(request_id) else {
            return unknown_request_error(request_id);
        };
        if pending.kind != PendingRequestKind::Control {
            self.pending_requests.insert(request_id.clone(), pending);
            return error_payload(
                ErrorCode::InvalidPayload,
                format!("Code Bridge request {request_id} is not a control request"),
            );
        }
        if pending.target_client_id != message.responding_client_id {
            self.pending_requests.insert(request_id.clone(), pending);
            return validation_error_payload(ValidationError::CapabilityDenied);
        }
        self.touch_client(&pending.target_client_id);
        self.last_event_time_unix_ms = Some(now_unix_ms());
        self.enqueue_delivery(
            envelope.clone(),
            DeliveryRoute::Target(pending.requester_client_id),
            true,
            outgoing,
        );
        BridgePayload::Ack(AckMessage {
            message_id: request_id.to_string(),
        })
    }

    fn validate_request_participants(
        &mut self,
        requester_client_id: &str,
        target_client_id: &str,
        requester_allows: impl FnOnce(&CapabilitySet) -> bool,
        target_allows: impl FnOnce(&CapabilitySet) -> bool,
    ) -> Option<BridgePayload> {
        let Some(requester) = self.clients.get_mut(requester_client_id) else {
            return Some(error_payload(
                ErrorCode::InvalidPayload,
                format!("unknown Code Bridge requester {requester_client_id}"),
            ));
        };
        requester.last_seen = Instant::now();
        if !requester_allows(&requester.capabilities) {
            return Some(validation_error_payload(ValidationError::CapabilityDenied));
        }

        let Some(target) = self.clients.get_mut(target_client_id) else {
            return Some(error_payload(
                ErrorCode::InvalidPayload,
                format!("unknown Code Bridge target {target_client_id}"),
            ));
        };
        target.last_seen = Instant::now();
        if !target_allows(&target.capabilities) {
            return Some(validation_error_payload(ValidationError::CapabilityDenied));
        }
        None
    }

    fn enqueue_delivery(
        &mut self,
        envelope: BridgeEnvelope,
        route: DeliveryRoute,
        retain: bool,
        outgoing: &mut Vec<Arc<BridgeDelivery>>,
    ) {
        let byte_size = envelope_approx_bytes(&envelope);
        let delivery = Arc::new(BridgeDelivery {
            sequence: self.next_delivery_sequence,
            envelope,
            route,
            byte_size,
        });
        self.next_delivery_sequence = self.next_delivery_sequence.saturating_add(1);
        if retain {
            self.retained_delivery_bytes = self
                .retained_delivery_bytes
                .saturating_add(delivery.byte_size);
            self.retained_deliveries.push_back(Arc::clone(&delivery));
            while self.retained_deliveries.len() > MAX_RETAINED_EVENTS
                || self.retained_delivery_bytes > MAX_RETAINED_DELIVERY_BYTES
            {
                let Some(removed) = self.retained_deliveries.pop_front() else {
                    self.retained_delivery_bytes = 0;
                    break;
                };
                self.retained_delivery_bytes = self
                    .retained_delivery_bytes
                    .saturating_sub(removed.byte_size);
            }
        }
        outgoing.push(delivery);
    }

    fn expire_pending_requests(&mut self, outgoing: &mut Vec<Arc<BridgeDelivery>>) {
        let now = Instant::now();
        let expired: Vec<_> = self
            .pending_requests
            .iter()
            .filter(|(_, pending)| pending.deadline <= now)
            .map(|(request_id, pending)| (request_id.clone(), pending.clone()))
            .collect();
        for (request_id, pending) in expired {
            self.pending_requests.remove(&request_id);
            let error = Some(ErrorMessage {
                code: ErrorCode::Timeout,
                message: format!("Code Bridge request {request_id} timed out"),
            });
            let envelope = BridgeEnvelope {
                protocol_version: PROTOCOL_VERSION.to_string(),
                message_id: format!("timeout-{request_id}"),
                timestamp_unix_ms: now_unix_ms(),
                payload: pending.timeout_payload(&request_id, error),
            };
            self.enqueue_delivery(
                envelope,
                DeliveryRoute::Target(pending.requester_client_id),
                true,
                outgoing,
            );
        }
    }

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

    fn validate_client_session(
        &self,
        client_id: &str,
        client_session_token: &str,
    ) -> Result<(), BridgeHttpError> {
        let Some(client) = self.clients.get(client_id) else {
            return Err(BridgeHttpError::InvalidPayload(format!(
                "unknown Code Bridge client {client_id}"
            )));
        };
        if constant_time_eq(
            client.session_token.as_bytes(),
            client_session_token.as_bytes(),
        ) {
            Ok(())
        } else {
            Err(BridgeHttpError::Unauthorized(
                "invalid Code Bridge client session token".to_string(),
            ))
        }
    }

    fn touch_client_session(&mut self, client_id: &str, client_session_token: &str) {
        if let Some(client) = self.clients.get_mut(client_id)
            && constant_time_eq(
                client.session_token.as_bytes(),
                client_session_token.as_bytes(),
            )
        {
            client.last_seen = Instant::now();
        }
    }

    fn expire_stale_clients(&mut self) {
        let now = Instant::now();
        self.clients
            .retain(|_, client| now.duration_since(client.last_seen) <= self.stale_client_timeout);
    }
}

#[derive(Clone)]
struct ClientState {
    role: ClientRole,
    capabilities: CapabilitySet,
    filter: SubscriptionFilter,
    session_token: String,
    last_seen: Instant,
}

#[derive(Clone)]
struct BridgeDelivery {
    sequence: u64,
    envelope: BridgeEnvelope,
    route: DeliveryRoute,
    byte_size: usize,
}

impl BridgeDelivery {
    fn matches_client(&self, client_id: &str, client: &ClientState) -> bool {
        match &self.route {
            DeliveryRoute::Target(target_client_id) => target_client_id == client_id,
            DeliveryRoute::Subscribers {
                source_client_id,
                event_kind,
                event,
            } => {
                client.capabilities.subscribe_events
                    && filter_matches(&client.filter, source_client_id, event_kind.clone(), event)
            }
        }
    }
}

fn envelope_approx_bytes(envelope: &BridgeEnvelope) -> usize {
    serde_json::to_vec(envelope)
        .map(|bytes| bytes.len())
        .unwrap_or(MAX_SCREENSHOT_MESSAGE_BYTES)
}

#[derive(Clone)]
enum DeliveryRoute {
    Target(String),
    Subscribers {
        source_client_id: String,
        event_kind: EventKind,
        event: BridgeEvent,
    },
}

struct EventStreamState {
    client_id: String,
    client_session_token: String,
    cursor: u64,
    replay: Vec<Arc<BridgeDelivery>>,
    delivery_rx: broadcast::Receiver<Arc<BridgeDelivery>>,
}

#[derive(Clone)]
struct PendingRequest {
    requester_client_id: String,
    target_client_id: String,
    kind: PendingRequestKind,
    deadline: Instant,
}

impl PendingRequest {
    fn timeout_payload(&self, request_id: &str, error: Option<ErrorMessage>) -> BridgePayload {
        match self.kind {
            PendingRequestKind::Screenshot => BridgePayload::ScreenshotResponse(
                codex_code_bridge_protocol::ScreenshotResponseMessage {
                    request_id: request_id.to_string(),
                    responding_client_id: self.target_client_id.clone(),
                    status: codex_code_bridge_protocol::ControlStatus::TimedOut,
                    screenshot: None,
                    error,
                },
            ),
            PendingRequestKind::Control => {
                BridgePayload::ControlResponse(codex_code_bridge_protocol::ControlResponseMessage {
                    request_id: request_id.to_string(),
                    responding_client_id: self.target_client_id.clone(),
                    status: codex_code_bridge_protocol::ControlStatus::TimedOut,
                    summary: String::new(),
                    error,
                })
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PendingRequestKind {
    Screenshot,
    Control,
}

fn filter_matches(
    filter: &SubscriptionFilter,
    source_client_id: &str,
    event_kind: EventKind,
    event: &BridgeEvent,
) -> bool {
    if !filter.client_ids.is_empty()
        && !filter
            .client_ids
            .iter()
            .any(|client_id| client_id == source_client_id)
    {
        return false;
    }
    if !filter.event_kinds.is_empty()
        && !filter
            .event_kinds
            .iter()
            .any(|allowed_kind| allowed_kind == &event_kind)
    {
        return false;
    }
    if !filter.levels.is_empty()
        && let BridgeEvent::Console(console) = event
    {
        return filter
            .levels
            .iter()
            .any(|allowed_level| allowed_level == &console.level);
    }
    true
}

fn target_can_run_command(capabilities: &CapabilitySet, command: &ControlCommand) -> bool {
    match command {
        ControlCommand::CaptureScreenshot => capabilities.provide_screenshot,
        ControlCommand::ExecuteJavascript { .. } => {
            capabilities.provide_control && capabilities.provide_javascript_execution
        }
    }
}

fn delivery_to_sse_event(delivery: &BridgeDelivery) -> Result<SseEvent, Infallible> {
    envelope_to_sse_event(delivery.sequence, delivery.envelope.clone())
}

fn envelope_to_sse_event(sequence: u64, envelope: BridgeEnvelope) -> Result<SseEvent, Infallible> {
    let event_name = message_family(&envelope.payload);
    let message = BridgeSseMessage { sequence, envelope };
    let data = serde_json::to_string(&message).unwrap_or_else(|err| {
        let fallback = BridgeSseMessage {
            sequence,
            envelope: BridgeEnvelope {
                protocol_version: PROTOCOL_VERSION.to_string(),
                message_id: format!("event-serialization-error-{sequence}"),
                timestamp_unix_ms: now_unix_ms(),
                payload: error_payload(
                    ErrorCode::InvalidPayload,
                    format!("failed to serialize Code Bridge event: {err}"),
                ),
            },
        };
        serde_json::to_string(&fallback)
            .expect("Code Bridge serialization fallback should serialize")
    });
    Ok(SseEvent::default()
        .id(sequence.to_string())
        .event(event_name)
        .data(data))
}

fn validation_error_payload(error: ValidationError) -> BridgePayload {
    let (_, message) = validation_http_error(error);
    BridgePayload::Error(message)
}

fn unknown_request_error(request_id: &str) -> BridgePayload {
    error_payload(
        ErrorCode::InvalidPayload,
        format!("unknown Code Bridge request {request_id}"),
    )
}

fn error_payload(code: ErrorCode, message: String) -> BridgePayload {
    BridgePayload::Error(ErrorMessage { code, message })
}

async fn readyz_handler() -> StatusCode {
    StatusCode::OK
}

async fn status_handler(State(state): State<AppState>) -> axum::Json<BridgeServiceStatus> {
    axum::Json(state.state.status().await)
}

async fn events_handler(
    State(state): State<AppState>,
    AxumPath(client_id): AxumPath<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, BridgeHttpError> {
    // Browser clients need a fetch-based SSE reader here: native EventSource
    // cannot attach the bearer and client-session headers this endpoint uses.
    let client_session_token = client_session_token_from_headers(&headers)?;
    let last_seen_sequence = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let stream_state = state
        .state
        .open_event_stream(&client_id, client_session_token, last_seen_sequence)
        .await?;
    let shared_state = state.state.clone();
    let stream = async_stream::stream! {
        let client_id = stream_state.client_id;
        let client_session_token = stream_state.client_session_token;
        for delivery in stream_state.replay {
            if shared_state
                .client_session_matches_delivery(&client_id, &client_session_token, &delivery)
                .await
            {
                yield delivery_to_sse_event(&delivery);
            } else {
                return;
            }
        }

        let mut cursor = stream_state.cursor;
        let mut delivery_rx = stream_state.delivery_rx;
        let mut touch_interval = tokio::time::interval(EVENT_STREAM_TOUCH_INTERVAL);
        loop {
            tokio::select! {
                biased;

                _ = touch_interval.tick() => {
                    shared_state.touch_client_session(&client_id, &client_session_token).await;
                }
                delivery = delivery_rx.recv() => {
                    match delivery {
                        Ok(delivery) => {
                            if delivery.sequence <= cursor {
                                continue;
                            }
                            cursor = delivery.sequence;
                            if shared_state
                                .client_session_matches_delivery(
                                    &client_id,
                                    &client_session_token,
                                    &delivery,
                                )
                                .await
                            {
                                yield delivery_to_sse_event(&delivery);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            let envelope = BridgeEnvelope {
                                protocol_version: PROTOCOL_VERSION.to_string(),
                                message_id: format!("event-stream-lagged-{client_id}"),
                                timestamp_unix_ms: now_unix_ms(),
                                payload: error_payload(
                                    ErrorCode::InvalidPayload,
                                    format!("Code Bridge event stream lagged by {skipped} messages"),
                                ),
                            };
                            yield envelope_to_sse_event(0, envelope);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn message_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
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

    let client_session = if let BridgePayload::Hello(message) = &envelope.payload {
        validate_payload_auth(&message.auth, state.auth_secret.as_str())?;
        None
    } else {
        let client_id = payload_client_id(&envelope.payload).ok_or_else(|| {
            BridgeHttpError::InvalidPayload(
                "Code Bridge payload does not identify a client".to_string(),
            )
        })?;
        let client_session_token = client_session_token_from_headers(&headers)?;
        Some(ValidatedClientSession {
            client_id,
            client_session_token,
        })
    };

    let payload = state
        .state
        .handle_envelope(&envelope, client_session)
        .await?;
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
    let cors_origin = cors_origin_from_headers(request.headers());
    if request.method() == Method::OPTIONS {
        return Ok(preflight_response(cors_origin));
    }
    let token = match bearer_token_from_headers(request.headers()) {
        Ok(token) => token,
        Err(error) => return Ok(cors_error_response(error, cors_origin)),
    };
    if !constant_time_eq(token.as_bytes(), state.auth_secret.as_bytes()) {
        return Ok(cors_error_response(
            BridgeHttpError::Unauthorized("invalid Code Bridge bearer token".to_string()),
            cors_origin,
        ));
    }
    if let Some(content_length) = request
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        && content_length > MAX_SCREENSHOT_MESSAGE_BYTES
    {
        return Ok(cors_error_response(
            BridgeHttpError::PayloadTooLarge {
                limit: MAX_SCREENSHOT_MESSAGE_BYTES,
                actual: content_length,
            },
            cors_origin,
        ));
    }
    let mut response = next.run(request).await;
    add_cors_headers(response.headers_mut(), cors_origin);
    Ok(response)
}

fn cors_error_response(error: BridgeHttpError, cors_origin: Option<HeaderValue>) -> Response {
    let mut response = error.into_response();
    add_cors_headers(response.headers_mut(), cors_origin);
    response
}

fn preflight_response(cors_origin: Option<HeaderValue>) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    add_cors_headers(response.headers_mut(), cors_origin);
    response
}

fn add_cors_headers(headers: &mut HeaderMap, cors_origin: Option<HeaderValue>) {
    if let Some(origin) = cors_origin {
        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static(CORS_ALLOW_METHODS),
        );
        headers.insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(CORS_ALLOW_HEADERS),
        );
        headers.insert(
            ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static(CORS_MAX_AGE_SECONDS),
        );
    }
    headers.insert(VARY, HeaderValue::from_static(CORS_VARY_HEADERS));
}

fn cors_origin_from_headers(headers: &HeaderMap) -> Option<HeaderValue> {
    let origin = headers.get(ORIGIN)?;
    if is_loopback_origin(origin) {
        Some(origin.clone())
    } else {
        None
    }
}

fn is_loopback_origin(origin: &HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Ok(uri) = origin.parse::<http::Uri>() else {
        return false;
    };
    if !matches!(uri.scheme_str(), Some("http") | Some("https")) {
        return false;
    }
    if uri
        .path_and_query()
        .is_some_and(|path| path.as_str() != "/")
    {
        return false;
    }
    let Some(host) = uri.host() else {
        return false;
    };
    let host = host.trim_matches(['[', ']']);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
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

fn client_session_token_from_headers(headers: &HeaderMap) -> Result<&str, BridgeHttpError> {
    headers
        .get(CLIENT_SESSION_HEADER)
        .ok_or_else(|| {
            BridgeHttpError::Unauthorized("missing Code Bridge client session token".to_string())
        })?
        .to_str()
        .map_err(|_| {
            BridgeHttpError::Unauthorized("invalid Code Bridge client session token".to_string())
        })
}

fn payload_client_id(payload: &BridgePayload) -> Option<&str> {
    match payload {
        BridgePayload::Heartbeat(message) => Some(&message.client_id),
        BridgePayload::Event(message) => Some(&message.client_id),
        BridgePayload::Subscribe(message) => Some(&message.subscriber_id),
        BridgePayload::ScreenshotRequest(message) => Some(&message.requester_client_id),
        BridgePayload::ScreenshotResponse(message) => Some(&message.responding_client_id),
        BridgePayload::ControlRequest(message) => Some(&message.requester_client_id),
        BridgePayload::ControlResponse(message) => Some(&message.responding_client_id),
        BridgePayload::Hello(_)
        | BridgePayload::HelloResponse(_)
        | BridgePayload::Ack(_)
        | BridgePayload::Error(_) => None,
    }
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
        ValidationError::InvalidEndpoint
        | ValidationError::InvalidDimensions
        | ValidationError::InvalidProvenance => (
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
    use codex_code_bridge_protocol::ControlCommand;
    use codex_code_bridge_protocol::ControlRequestMessage;
    use codex_code_bridge_protocol::ControlResponseMessage;
    use codex_code_bridge_protocol::ControlStatus;
    use codex_code_bridge_protocol::EventKind;
    use codex_code_bridge_protocol::EventPublishMessage;
    use codex_code_bridge_protocol::HeartbeatMessage;
    use codex_code_bridge_protocol::HelloMessage;
    use codex_code_bridge_protocol::ProvenanceMetadata;
    use codex_code_bridge_protocol::ScreenshotMediaType;
    use codex_code_bridge_protocol::ScreenshotPayload;
    use codex_code_bridge_protocol::ScreenshotRequestMessage;
    use codex_code_bridge_protocol::ScreenshotResponseMessage;
    use codex_code_bridge_protocol::SourceKind;
    use codex_code_bridge_protocol::SubscribeMessage;
    use codex_code_bridge_protocol::SubscriptionFilter;
    use eventsource_stream::Eventsource;
    use futures::StreamExt;
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
            .header(ORIGIN, "http://127.0.0.1:5173")
            .body("not json")
            .send()
            .await
            .expect("missing auth response");
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            missing.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("http://127.0.0.1:5173"))
        );

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
    async fn accepts_loopback_browser_preflight_without_bearer_auth() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let url = format!("{}/message", service.handle.endpoint_url());

        let response = client
            .request(Method::OPTIONS, &url)
            .header(ORIGIN, "http://127.0.0.1:5173")
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                "authorization, content-type, x-code-bridge-client-session",
            )
            .send()
            .await
            .expect("preflight response");

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("http://127.0.0.1:5173"))
        );
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_METHODS),
            Some(&HeaderValue::from_static(CORS_ALLOW_METHODS))
        );
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_HEADERS),
            Some(&HeaderValue::from_static(CORS_ALLOW_HEADERS))
        );

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn accepts_loopback_event_stream_preflight_without_bearer_auth() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let url = format!("{}/events/browser-client-1", service.handle.endpoint_url());

        let response = client
            .request(Method::OPTIONS, &url)
            .header(ORIGIN, "http://[::1]:5173")
            .header("access-control-request-method", "GET")
            .header(
                "access-control-request-headers",
                "authorization, last-event-id, x-code-bridge-client-session",
            )
            .send()
            .await
            .expect("preflight response");

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("http://[::1]:5173"))
        );
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_METHODS),
            Some(&HeaderValue::from_static(CORS_ALLOW_METHODS))
        );

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn includes_loopback_cors_headers_on_authenticated_event_streams() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let subscriber = register_subscriber(
            &client,
            &service.handle,
            "browser-subscriber-1",
            subscriber_capabilities(),
        )
        .await;

        let response = client
            .get(format!(
                "{}/events/browser-subscriber-1",
                service.handle.endpoint_url()
            ))
            .bearer_auth(service.handle.auth_secret())
            .header(CLIENT_SESSION_HEADER, subscriber.session_token.as_str())
            .header("last-event-id", "0")
            .header(ORIGIN, "http://127.0.0.1:5173")
            .send()
            .await
            .expect("event stream response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("http://127.0.0.1:5173"))
        );
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_HEADERS),
            Some(&HeaderValue::from_static(CORS_ALLOW_HEADERS))
        );
        drop(response);

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn omits_cors_allow_origin_for_non_loopback_browser_origins() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let url = format!("{}/message", service.handle.endpoint_url());

        let response = client
            .request(Method::OPTIONS, &url)
            .header(ORIGIN, "https://example.com")
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "authorization")
            .send()
            .await
            .expect("preflight response");

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            response
                .headers()
                .get(ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
        assert_eq!(
            response.headers().get(VARY),
            Some(&HeaderValue::from_static(CORS_VARY_HEADERS))
        );

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn includes_loopback_cors_headers_on_authenticated_message_responses() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let envelope = hello_envelope(
            "browser-client-1",
            service.handle.auth_secret(),
            ClientRole::Producer,
            producer_capabilities(),
        );
        let response = client
            .post(format!("{}/message", service.handle.endpoint_url()))
            .bearer_auth(service.handle.auth_secret())
            .header(ORIGIN, "http://localhost:3000")
            .json(&envelope)
            .send()
            .await
            .expect("message response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("http://localhost:3000"))
        );
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_HEADERS),
            Some(&HeaderValue::from_static(CORS_ALLOW_HEADERS))
        );
        let payload = response
            .json::<BridgeMessageResponse>()
            .await
            .expect("message response json")
            .payload;
        assert!(matches!(payload, BridgePayload::HelloResponse(_)));

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn events_endpoint_requires_auth_and_registered_client() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let url = format!("{}/events/subscriber-1", service.handle.endpoint_url());

        let missing = client.get(&url).send().await.expect("missing auth");
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let invalid = client
            .get(&url)
            .bearer_auth("wrong")
            .send()
            .await
            .expect("invalid auth");
        assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);

        let unknown = client
            .get(&url)
            .bearer_auth(service.handle.auth_secret())
            .header(CLIENT_SESSION_HEADER, "bogus-session")
            .send()
            .await
            .expect("unknown client");
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn events_endpoint_replays_matching_subscribed_events() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let producer = register_producer(
            &client,
            &service.handle,
            "producer-1",
            producer_capabilities(),
        )
        .await;
        let subscriber = register_subscriber(
            &client,
            &service.handle,
            "subscriber-1",
            subscriber_capabilities(),
        )
        .await;
        subscribe(
            &client,
            &service.handle,
            &subscriber,
            "subscriber-1",
            SubscriptionFilter {
                levels: vec![ConsoleLevel::Error],
                event_kinds: vec![EventKind::Console],
                client_ids: vec!["producer-1".to_string()],
            },
        )
        .await;

        publish_console(
            &client,
            &service.handle,
            &producer,
            "producer-1",
            "info-1",
            ConsoleLevel::Info,
        )
        .await;
        publish_console(
            &client,
            &service.handle,
            &producer,
            "producer-1",
            "error-1",
            ConsoleLevel::Error,
        )
        .await;

        let response = open_events(&client, &service.handle, &subscriber, "subscriber-1")
            .await
            .expect("events response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("text/event-stream"))
        );
        let mut events = response.bytes_stream().eventsource();
        let message = next_sse_message(&mut events).await;
        assert_eq!(message.sequence, 2);
        let BridgePayload::Event(event) = message.envelope.payload else {
            panic!("expected event payload");
        };
        assert_eq!(event.client_id, "producer-1");
        assert_eq!(event.event_id, "error-1");
        assert!(matches!(
            event.event,
            BridgeEvent::Console(ConsoleEvent {
                level: ConsoleLevel::Error,
                ..
            })
        ));

        assert_no_sse_message(&mut events).await;
        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn broadcast_event_replay_after_subscriber_reconnect() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let producer = register_producer(
            &client,
            &service.handle,
            "producer-1",
            producer_capabilities(),
        )
        .await;
        let subscriber_a = register_subscriber(
            &client,
            &service.handle,
            "subscriber-a",
            subscriber_capabilities(),
        )
        .await;
        let subscriber_b = register_subscriber(
            &client,
            &service.handle,
            "subscriber-b",
            subscriber_capabilities(),
        )
        .await;
        subscribe_to_producer(
            &client,
            &service.handle,
            &subscriber_a,
            "subscriber-a",
            "producer-1",
        )
        .await;
        subscribe_to_producer(
            &client,
            &service.handle,
            &subscriber_b,
            "subscriber-b",
            "producer-1",
        )
        .await;

        for index in 1..=3 {
            publish_console(
                &client,
                &service.handle,
                &producer,
                "producer-1",
                &format!("event-{index}"),
                ConsoleLevel::Info,
            )
            .await;
        }

        let subscriber_a_events =
            open_events_after(&client, &service.handle, &subscriber_a, "subscriber-a", 0)
                .await
                .expect("subscriber-a initial stream");
        let mut subscriber_a_events = subscriber_a_events.bytes_stream().eventsource();
        let initial_messages = next_event_messages(&mut subscriber_a_events, 3).await;
        assert_event_ids(&initial_messages, &["event-1", "event-2", "event-3"]);
        let last_seen_sequence = initial_messages
            .last()
            .expect("initial event messages")
            .sequence;
        drop(subscriber_a_events);

        for index in 4..=5 {
            publish_console(
                &client,
                &service.handle,
                &producer,
                "producer-1",
                &format!("event-{index}"),
                ConsoleLevel::Info,
            )
            .await;
        }

        let subscriber_a_events = open_events_after(
            &client,
            &service.handle,
            &subscriber_a,
            "subscriber-a",
            last_seen_sequence,
        )
        .await
        .expect("subscriber-a reconnect stream");
        let mut subscriber_a_events = subscriber_a_events.bytes_stream().eventsource();
        let replay_messages = next_event_messages(&mut subscriber_a_events, 2).await;
        assert_event_ids(&replay_messages, &["event-4", "event-5"]);
        assert_no_sse_message(&mut subscriber_a_events).await;

        let subscriber_b_events =
            open_events_after(&client, &service.handle, &subscriber_b, "subscriber-b", 0)
                .await
                .expect("subscriber-b first stream");
        let mut subscriber_b_events = subscriber_b_events.bytes_stream().eventsource();
        let late_messages = next_event_messages(&mut subscriber_b_events, 5).await;
        assert_event_ids(
            &late_messages,
            &["event-1", "event-2", "event-3", "event-4", "event-5"],
        );
        assert!(
            late_messages
                .windows(2)
                .all(|window| window[0].sequence < window[1].sequence)
        );
        assert_no_sse_message(&mut subscriber_b_events).await;

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn live_client_witness_round_trips_descriptor_replay_screenshot_and_control() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let descriptor = read_descriptor(service.handle.descriptor_path());
        assert_eq!(validate_descriptor(&descriptor), Ok(()));
        assert_eq!(descriptor.auth_secret, service.handle.auth_secret());
        let endpoint_url = descriptor_endpoint_url(&descriptor);
        assert_eq!(endpoint_url, service.handle.endpoint_url());

        let client = Client::new();
        let producer = register_producer_with_endpoint(
            &client,
            &endpoint_url,
            &descriptor.auth_secret,
            "producer-1",
            CapabilitySet {
                provide_control: true,
                provide_javascript_execution: true,
                ..producer_capabilities()
            },
        )
        .await;
        let subscriber = register_subscriber_with_endpoint(
            &client,
            &endpoint_url,
            &descriptor.auth_secret,
            "subscriber-1",
            subscriber_capabilities(),
        )
        .await;
        subscribe_to_producer_with_endpoint(
            &client,
            &endpoint_url,
            &descriptor.auth_secret,
            &subscriber,
            "subscriber-1",
            "producer-1",
        )
        .await;

        publish_console_with_endpoint(
            &client,
            &endpoint_url,
            &descriptor.auth_secret,
            &producer,
            "producer-1",
            "event-1",
            ConsoleLevel::Info,
        )
        .await;
        let subscriber_events = open_events_with_endpoint(
            &client,
            &endpoint_url,
            &descriptor.auth_secret,
            &subscriber,
            "subscriber-1",
        )
        .await
        .expect("subscriber events");
        let mut subscriber_events = subscriber_events.bytes_stream().eventsource();
        let message = next_sse_message(&mut subscriber_events).await;
        let first_sequence = message.sequence;
        assert_event_ids(&[message], &["event-1"]);
        drop(subscriber_events);

        publish_console_with_endpoint(
            &client,
            &endpoint_url,
            &descriptor.auth_secret,
            &producer,
            "producer-1",
            "event-2",
            ConsoleLevel::Warn,
        )
        .await;
        let subscriber_events = open_events_after_with_endpoint(
            &client,
            &endpoint_url,
            &descriptor.auth_secret,
            &subscriber,
            "subscriber-1",
            first_sequence,
        )
        .await
        .expect("subscriber reconnect events");
        let mut subscriber_events = subscriber_events.bytes_stream().eventsource();
        let replayed = next_event_messages(&mut subscriber_events, 1).await;
        let second_sequence = replayed[0].sequence;
        assert_event_ids(&replayed, &["event-2"]);
        drop(subscriber_events);

        let producer_events = open_events_with_endpoint(
            &client,
            &endpoint_url,
            &descriptor.auth_secret,
            &producer,
            "producer-1",
        )
        .await
        .expect("producer events");
        let mut producer_events = producer_events.bytes_stream().eventsource();
        let subscriber_events = open_events_after_with_endpoint(
            &client,
            &endpoint_url,
            &descriptor.auth_secret,
            &subscriber,
            "subscriber-1",
            second_sequence,
        )
        .await
        .expect("subscriber targeted events");
        let mut subscriber_events = subscriber_events.bytes_stream().eventsource();

        post_envelope_with_payload_endpoint(
            &client,
            &endpoint_url,
            &descriptor.auth_secret,
            &subscriber,
            envelope(
                "screenshot-request-1",
                BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
                    request_id: "shot-1".to_string(),
                    requester_client_id: "subscriber-1".to_string(),
                    target_client_id: "producer-1".to_string(),
                    timeout_ms: 1_000,
                }),
            ),
        )
        .await;
        let message = next_sse_message(&mut producer_events).await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
                request_id,
                ..
            }) if request_id == "shot-1"
        ));

        post_envelope_with_payload_endpoint(
            &client,
            &endpoint_url,
            &descriptor.auth_secret,
            &producer,
            envelope(
                "screenshot-response-1",
                BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
                    request_id: "shot-1".to_string(),
                    responding_client_id: "producer-1".to_string(),
                    status: ControlStatus::Ok,
                    screenshot: Some(ScreenshotPayload {
                        width: 1,
                        height: 1,
                        media_type: ScreenshotMediaType::Png,
                        data_base64: "iVBORw0KGgo=".to_string(),
                    }),
                    error: None,
                }),
            ),
        )
        .await;
        let message = next_sse_message(&mut subscriber_events).await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
                request_id,
                status: ControlStatus::Ok,
                ..
            }) if request_id == "shot-1"
        ));

        post_envelope_with_payload_endpoint(
            &client,
            &endpoint_url,
            &descriptor.auth_secret,
            &subscriber,
            control_request_envelope("js-1", "subscriber-1", "producer-1"),
        )
        .await;
        let message = next_sse_message(&mut producer_events).await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ControlRequest(ControlRequestMessage {
                request_id,
                ..
            }) if request_id == "js-1"
        ));

        post_envelope_with_payload_endpoint(
            &client,
            &endpoint_url,
            &descriptor.auth_secret,
            &producer,
            envelope(
                "control-response-1",
                BridgePayload::ControlResponse(ControlResponseMessage {
                    request_id: "js-1".to_string(),
                    responding_client_id: "producer-1".to_string(),
                    status: ControlStatus::Ok,
                    summary: "https://example.test/page".to_string(),
                    error: None,
                }),
            ),
        )
        .await;
        let message = next_sse_message(&mut subscriber_events).await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ControlResponse(ControlResponseMessage {
                request_id,
                status: ControlStatus::Ok,
                ..
            }) if request_id == "js-1"
        ));
        assert_no_sse_message(&mut producer_events).await;

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn level_filter_does_not_hide_matching_non_console_events() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let producer = register_producer(
            &client,
            &service.handle,
            "producer-1",
            producer_capabilities(),
        )
        .await;
        let subscriber = register_subscriber(
            &client,
            &service.handle,
            "subscriber-1",
            subscriber_capabilities(),
        )
        .await;
        subscribe(
            &client,
            &service.handle,
            &subscriber,
            "subscriber-1",
            SubscriptionFilter {
                levels: vec![ConsoleLevel::Error],
                event_kinds: vec![EventKind::Console, EventKind::Error],
                client_ids: vec!["producer-1".to_string()],
            },
        )
        .await;

        post_envelope(
            &client,
            &service.handle,
            &producer,
            envelope(
                "error-event-1",
                BridgePayload::Event(EventPublishMessage {
                    client_id: "producer-1".to_string(),
                    event_id: "error-event-1".to_string(),
                    event: BridgeEvent::Error(codex_code_bridge_protocol::ErrorEvent {
                        message: "boom".to_string(),
                        stack: None,
                    }),
                }),
            ),
        )
        .await;

        let response = open_events(&client, &service.handle, &subscriber, "subscriber-1")
            .await
            .expect("events response");
        let mut events = response.bytes_stream().eventsource();
        let message = next_sse_message(&mut events).await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::Event(EventPublishMessage {
                event_id,
                event: BridgeEvent::Error(_),
                ..
            }) if event_id == "error-event-1"
        ));

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn client_session_token_is_bound_to_stream_and_message_identity() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let producer = register_producer(
            &client,
            &service.handle,
            "producer-1",
            producer_capabilities(),
        )
        .await;
        let subscriber = register_subscriber(
            &client,
            &service.handle,
            "subscriber-1",
            subscriber_capabilities(),
        )
        .await;

        let response = open_events(&client, &service.handle, &producer, "subscriber-1")
            .await
            .expect("cross-client stream response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = client
            .post(format!("{}/message", service.handle.endpoint_url()))
            .bearer_auth(service.handle.auth_secret())
            .header(CLIENT_SESSION_HEADER, producer.session_token.as_str())
            .json(&envelope(
                "forged-subscribe",
                BridgePayload::Subscribe(SubscribeMessage {
                    subscriber_id: "subscriber-1".to_string(),
                    filter: SubscriptionFilter::default(),
                }),
            ))
            .send()
            .await
            .expect("cross-client message response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = open_events(&client, &service.handle, &subscriber, "subscriber-1")
            .await
            .expect("own stream response");
        assert_eq!(response.status(), StatusCode::OK);
        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn invalid_provenance_hello_is_rejected_before_registration() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let envelope = envelope(
            "hello-producer-1",
            BridgePayload::Hello(HelloMessage {
                client_id: "producer-1".to_string(),
                role: ClientRole::Producer,
                auth: AuthProof::LocalSecret {
                    secret: service.handle.auth_secret().to_string(),
                },
                requested_capabilities: producer_capabilities(),
                metadata: ClientMetadata {
                    source_kind: SourceKind::Cli,
                    label: Some("launchplane worker".to_string()),
                    provenance: Some(ProvenanceMetadata {
                        repository_url: Some("https://127.0.0.1/cbusillo/codex-lab".to_string()),
                        issue_or_pr_url: None,
                        request_id: None,
                        trace_id: None,
                        environment_label: None,
                    }),
                },
            }),
        );

        let response = client
            .post(format!("{}/message", service.handle.endpoint_url()))
            .bearer_auth(service.handle.auth_secret())
            .json(&envelope)
            .send()
            .await
            .expect("invalid provenance hello response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let status = service.handle.status().await;
        assert_eq!(status.connected_producer_count, 0);
        assert_eq!(status.connected_subscriber_count, 0);
        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn duplicate_hello_refreshes_existing_client_session() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let first = register_producer(
            &client,
            &service.handle,
            "producer-1",
            producer_capabilities(),
        )
        .await;
        let second = register_producer(
            &client,
            &service.handle,
            "producer-1",
            producer_capabilities(),
        )
        .await;

        assert_ne!(first.session_token, second.session_token);

        let stale_response = client
            .post(format!("{}/message", service.handle.endpoint_url()))
            .bearer_auth(service.handle.auth_secret())
            .header(CLIENT_SESSION_HEADER, first.session_token.as_str())
            .json(&envelope(
                "stale-heartbeat",
                BridgePayload::Heartbeat(HeartbeatMessage {
                    client_id: "producer-1".to_string(),
                    sequence: 1,
                }),
            ))
            .send()
            .await
            .expect("stale heartbeat response");
        assert_eq!(stale_response.status(), StatusCode::UNAUTHORIZED);

        post_envelope(
            &client,
            &service.handle,
            &second,
            envelope(
                "fresh-heartbeat",
                BridgePayload::Heartbeat(HeartbeatMessage {
                    client_id: "producer-1".to_string(),
                    sequence: 2,
                }),
            ),
        )
        .await;
        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn duplicate_hello_rejects_role_change_for_existing_client_id() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let producer = register_producer(
            &client,
            &service.handle,
            "client-1",
            producer_capabilities(),
        )
        .await;

        let payload = post_envelope_with_payload(
            &client,
            &service.handle,
            &producer,
            hello_envelope(
                "client-1",
                service.handle.auth_secret(),
                ClientRole::Subscriber,
                subscriber_capabilities(),
            ),
        )
        .await;
        assert!(matches!(
            payload,
            BridgePayload::Error(ErrorMessage {
                code: ErrorCode::InvalidPayload,
                ..
            })
        ));

        post_envelope(
            &client,
            &service.handle,
            &producer,
            envelope(
                "still-producer-heartbeat",
                BridgePayload::Heartbeat(HeartbeatMessage {
                    client_id: "client-1".to_string(),
                    sequence: 1,
                }),
            ),
        )
        .await;
        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn stale_session_is_rejected_inside_envelope_handling() {
        let shared = SharedState::new(Duration::from_secs(30));
        let first = shared
            .handle_envelope(
                &hello_envelope(
                    "producer-1",
                    "unused-secret",
                    ClientRole::Producer,
                    producer_capabilities(),
                ),
                None,
            )
            .await
            .expect("first hello");
        let BridgePayload::HelloResponse(first) = first else {
            panic!("expected first hello response");
        };
        let first_token = first
            .client_session_token
            .expect("first client session token");

        shared
            .handle_envelope(
                &hello_envelope(
                    "producer-1",
                    "unused-secret",
                    ClientRole::Producer,
                    producer_capabilities(),
                ),
                None,
            )
            .await
            .expect("duplicate hello");

        let stale = shared
            .handle_envelope(
                &envelope(
                    "stale-heartbeat",
                    BridgePayload::Heartbeat(HeartbeatMessage {
                        client_id: "producer-1".to_string(),
                        sequence: 1,
                    }),
                ),
                Some(ValidatedClientSession {
                    client_id: "producer-1",
                    client_session_token: &first_token,
                }),
            )
            .await;
        assert!(matches!(stale, Err(BridgeHttpError::Unauthorized(_))));
    }

    #[tokio::test]
    async fn staged_replay_is_rejected_after_duplicate_hello() {
        let shared = SharedState::new(Duration::from_secs(30));
        let first = shared
            .handle_envelope(
                &hello_envelope(
                    "producer-1",
                    "unused-secret",
                    ClientRole::Producer,
                    producer_capabilities(),
                ),
                None,
            )
            .await
            .expect("first hello");
        let BridgePayload::HelloResponse(first) = first else {
            panic!("expected first hello response");
        };
        let first_token = first
            .client_session_token
            .expect("first client session token");

        let subscriber = shared
            .handle_envelope(
                &hello_envelope(
                    "requester-1",
                    "unused-secret",
                    ClientRole::Subscriber,
                    subscriber_capabilities(),
                ),
                None,
            )
            .await
            .expect("subscriber hello");
        let BridgePayload::HelloResponse(subscriber) = subscriber else {
            panic!("expected subscriber hello response");
        };
        let subscriber_token = subscriber
            .client_session_token
            .expect("subscriber client session token");

        shared
            .handle_envelope(
                &envelope(
                    "screenshot-request-before-refresh",
                    BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
                        request_id: "shot-before-refresh".to_string(),
                        requester_client_id: "requester-1".to_string(),
                        target_client_id: "producer-1".to_string(),
                        timeout_ms: 1_000,
                    }),
                ),
                Some(ValidatedClientSession {
                    client_id: "requester-1",
                    client_session_token: &subscriber_token,
                }),
            )
            .await
            .expect("screenshot request");

        let stream_state = shared
            .open_event_stream("producer-1", &first_token, 0)
            .await
            .expect("staged replay stream");
        assert_eq!(stream_state.replay.len(), 1);

        shared
            .handle_envelope(
                &hello_envelope(
                    "producer-1",
                    "unused-secret",
                    ClientRole::Producer,
                    producer_capabilities(),
                ),
                None,
            )
            .await
            .expect("duplicate hello");

        assert!(
            !shared
                .client_session_matches_delivery(
                    &stream_state.client_id,
                    &stream_state.client_session_token,
                    &stream_state.replay[0],
                )
                .await
        );

        let fresh_token = current_session_token(&shared, "producer-1").await;
        let fresh = shared
            .open_event_stream("producer-1", &fresh_token, 0)
            .await
            .expect("fresh replay stream");
        assert_eq!(fresh.replay.len(), 1);
        assert!(matches!(
            fresh.replay[0].envelope.payload,
            BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
                ref request_id,
                ..
            }) if request_id == "shot-before-refresh"
        ));
    }

    #[tokio::test]
    async fn old_sse_stream_loses_authority_after_duplicate_hello() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let first = register_producer(
            &client,
            &service.handle,
            "producer-1",
            producer_capabilities(),
        )
        .await;
        let requester = register_subscriber(
            &client,
            &service.handle,
            "requester-1",
            subscriber_capabilities(),
        )
        .await;
        let first_events = open_events(&client, &service.handle, &first, "producer-1")
            .await
            .expect("first producer stream");
        let mut first_events = first_events.bytes_stream().eventsource();

        let second = register_producer(
            &client,
            &service.handle,
            "producer-1",
            producer_capabilities(),
        )
        .await;

        post_envelope(
            &client,
            &service.handle,
            &requester,
            envelope(
                "screenshot-request-after-refresh",
                BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
                    request_id: "shot-after-refresh".to_string(),
                    requester_client_id: "requester-1".to_string(),
                    target_client_id: "producer-1".to_string(),
                    timeout_ms: 1_000,
                }),
            ),
        )
        .await;
        assert_no_sse_message(&mut first_events).await;

        let second_events = open_events(&client, &service.handle, &second, "producer-1")
            .await
            .expect("second producer stream");
        let mut second_events = second_events.bytes_stream().eventsource();
        let message = next_sse_message(&mut second_events).await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
                request_id,
                ..
            }) if request_id == "shot-after-refresh"
        ));

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn screenshot_request_and_response_route_only_to_target_and_requester() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let producer = register_producer(
            &client,
            &service.handle,
            "producer-1",
            producer_capabilities(),
        )
        .await;
        let requester = register_subscriber(
            &client,
            &service.handle,
            "requester-1",
            subscriber_capabilities(),
        )
        .await;

        let producer_events = open_events(&client, &service.handle, &producer, "producer-1")
            .await
            .expect("producer events");
        let mut producer_events = producer_events.bytes_stream().eventsource();
        let requester_events = open_events(&client, &service.handle, &requester, "requester-1")
            .await
            .expect("requester events");
        let mut requester_events = requester_events.bytes_stream().eventsource();

        post_envelope_with_payload(
            &client,
            &service.handle,
            &requester,
            envelope(
                "screenshot-request-1",
                BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
                    request_id: "shot-1".to_string(),
                    requester_client_id: "requester-1".to_string(),
                    target_client_id: "producer-1".to_string(),
                    timeout_ms: 1_000,
                }),
            ),
        )
        .await;
        let message = next_sse_message(&mut producer_events).await;
        let BridgePayload::ScreenshotRequest(request) = message.envelope.payload else {
            panic!("expected screenshot request");
        };
        assert_eq!(request.request_id, "shot-1");
        assert_eq!(request.requester_client_id, "requester-1");
        assert_eq!(request.target_client_id, "producer-1");

        post_envelope_with_payload(
            &client,
            &service.handle,
            &producer,
            envelope(
                "screenshot-response-1",
                BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
                    request_id: "shot-1".to_string(),
                    responding_client_id: "producer-1".to_string(),
                    status: ControlStatus::Ok,
                    screenshot: Some(ScreenshotPayload {
                        width: 1,
                        height: 1,
                        media_type: ScreenshotMediaType::Png,
                        data_base64: "iVBORw0KGgo=".to_string(),
                    }),
                    error: None,
                }),
            ),
        )
        .await;
        let message = next_sse_message(&mut requester_events).await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
                request_id,
                status: ControlStatus::Ok,
                ..
            }) if request_id == "shot-1"
        ));
        assert_no_sse_message(&mut producer_events).await;

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn targeted_request_and_response_replay_after_sse_reconnect() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let producer = register_producer(
            &client,
            &service.handle,
            "producer-1",
            producer_capabilities(),
        )
        .await;
        let requester = register_subscriber(
            &client,
            &service.handle,
            "requester-1",
            subscriber_capabilities(),
        )
        .await;

        post_envelope(
            &client,
            &service.handle,
            &requester,
            envelope(
                "screenshot-request-1",
                BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
                    request_id: "shot-1".to_string(),
                    requester_client_id: "requester-1".to_string(),
                    target_client_id: "producer-1".to_string(),
                    timeout_ms: 1_000,
                }),
            ),
        )
        .await;

        let producer_events =
            open_events_after(&client, &service.handle, &producer, "producer-1", 0)
                .await
                .expect("producer reconnect events");
        let mut producer_events = producer_events.bytes_stream().eventsource();
        let message = next_sse_message(&mut producer_events).await;
        let request_sequence = message.sequence;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
                request_id,
                ..
            }) if request_id == "shot-1"
        ));
        drop(producer_events);

        post_envelope(
            &client,
            &service.handle,
            &producer,
            envelope(
                "screenshot-response-1",
                BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
                    request_id: "shot-1".to_string(),
                    responding_client_id: "producer-1".to_string(),
                    status: ControlStatus::Ok,
                    screenshot: None,
                    error: None,
                }),
            ),
        )
        .await;

        let producer_events = open_events_after(
            &client,
            &service.handle,
            &producer,
            "producer-1",
            request_sequence,
        )
        .await
        .expect("producer reconnect after request");
        let mut producer_events = producer_events.bytes_stream().eventsource();
        assert_no_sse_message(&mut producer_events).await;

        let requester_events =
            open_events_after(&client, &service.handle, &requester, "requester-1", 0)
                .await
                .expect("requester reconnect events");
        let mut requester_events = requester_events.bytes_stream().eventsource();
        let message = next_sse_message(&mut requester_events).await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
                request_id,
                status: ControlStatus::Ok,
                ..
            }) if request_id == "shot-1"
        ));

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn response_sender_must_match_pending_request_target() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let producer = register_producer(
            &client,
            &service.handle,
            "producer-1",
            producer_capabilities(),
        )
        .await;
        let other_producer = register_producer(
            &client,
            &service.handle,
            "producer-2",
            producer_capabilities(),
        )
        .await;
        let requester = register_subscriber(
            &client,
            &service.handle,
            "requester-1",
            subscriber_capabilities(),
        )
        .await;
        post_envelope(
            &client,
            &service.handle,
            &requester,
            envelope(
                "screenshot-request-1",
                BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
                    request_id: "shot-1".to_string(),
                    requester_client_id: "requester-1".to_string(),
                    target_client_id: "producer-1".to_string(),
                    timeout_ms: 1_000,
                }),
            ),
        )
        .await;

        let forged = post_envelope_with_payload(
            &client,
            &service.handle,
            &other_producer,
            envelope(
                "forged-screenshot-response",
                BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
                    request_id: "shot-1".to_string(),
                    responding_client_id: "producer-2".to_string(),
                    status: ControlStatus::Ok,
                    screenshot: None,
                    error: None,
                }),
            ),
        )
        .await;
        assert!(matches!(
            forged,
            BridgePayload::Error(ErrorMessage {
                code: ErrorCode::CapabilityDenied,
                ..
            })
        ));

        let accepted = post_envelope_with_payload(
            &client,
            &service.handle,
            &producer,
            envelope(
                "screenshot-response-1",
                BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
                    request_id: "shot-1".to_string(),
                    responding_client_id: "producer-1".to_string(),
                    status: ControlStatus::Ok,
                    screenshot: None,
                    error: None,
                }),
            ),
        )
        .await;
        assert!(matches!(accepted, BridgePayload::Ack(_)));
        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn javascript_execution_requires_explicit_provider_grant() {
        let service = start_test_service(Duration::from_secs(30), Duration::from_secs(30)).await;
        let client = Client::new();
        let requester = register_subscriber(
            &client,
            &service.handle,
            "requester-1",
            subscriber_capabilities(),
        )
        .await;
        let producer_denied = register_producer(
            &client,
            &service.handle,
            "producer-denied",
            CapabilitySet {
                provide_control: true,
                ..producer_capabilities()
            },
        )
        .await;

        let denied = post_envelope_with_payload(
            &client,
            &service.handle,
            &requester,
            control_request_envelope("js-denied", "requester-1", "producer-denied"),
        )
        .await;
        assert!(matches!(
            denied,
            BridgePayload::Error(ErrorMessage {
                code: ErrorCode::CapabilityDenied,
                ..
            })
        ));

        let producer_granted = register_producer(
            &client,
            &service.handle,
            "producer-granted",
            CapabilitySet {
                provide_control: true,
                provide_javascript_execution: true,
                ..producer_capabilities()
            },
        )
        .await;
        assert_ne!(
            producer_denied.session_token,
            producer_granted.session_token
        );
        let producer_events = open_events(
            &client,
            &service.handle,
            &producer_granted,
            "producer-granted",
        )
        .await
        .expect("producer events");
        let mut producer_events = producer_events.bytes_stream().eventsource();
        let requester_events = open_events(&client, &service.handle, &requester, "requester-1")
            .await
            .expect("requester events");
        let mut requester_events = requester_events.bytes_stream().eventsource();

        let accepted = post_envelope_with_payload(
            &client,
            &service.handle,
            &requester,
            control_request_envelope("js-1", "requester-1", "producer-granted"),
        )
        .await;
        assert!(matches!(accepted, BridgePayload::Ack(_)));
        let message = next_sse_message(&mut producer_events).await;
        let BridgePayload::ControlRequest(request) = message.envelope.payload else {
            panic!("expected control request");
        };
        assert_eq!(request.request_id, "js-1");
        assert!(matches!(
            request.command,
            ControlCommand::ExecuteJavascript { ref code } if code == "window.location.href"
        ));

        post_envelope_with_payload(
            &client,
            &service.handle,
            &producer_granted,
            envelope(
                "control-response-1",
                BridgePayload::ControlResponse(ControlResponseMessage {
                    request_id: "js-1".to_string(),
                    responding_client_id: "producer-granted".to_string(),
                    status: ControlStatus::Ok,
                    summary: "https://example.test/page".to_string(),
                    error: None,
                }),
            ),
        )
        .await;
        let message = next_sse_message(&mut requester_events).await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ControlResponse(ControlResponseMessage {
                request_id,
                status: ControlStatus::Ok,
                ..
            }) if request_id == "js-1"
        ));

        service.handle.shutdown().await;
    }

    #[tokio::test]
    async fn status_tracks_clients_heartbeats_events_and_stale_timeout() {
        let service =
            start_test_service(Duration::from_millis(80), Duration::from_millis(20)).await;
        let client = Client::new();

        let producer = register_producer(
            &client,
            &service.handle,
            "producer-1",
            CapabilitySet {
                publish_events: true,
                ..CapabilitySet::default()
            },
        )
        .await;
        let subscriber = register_subscriber(
            &client,
            &service.handle,
            "subscriber-1",
            CapabilitySet {
                subscribe_events: true,
                ..CapabilitySet::default()
            },
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
            &producer,
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
            &subscriber,
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
        let producer = register_producer(
            &client,
            &service.handle,
            "producer-1",
            producer_capabilities(),
        )
        .await;
        let requester = register_subscriber(
            &client,
            &service.handle,
            "requester-1",
            subscriber_capabilities(),
        )
        .await;
        post_envelope(
            &client,
            &service.handle,
            &requester,
            envelope(
                "screenshot-request-1",
                BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
                    request_id: "screenshot-request-1".to_string(),
                    requester_client_id: "requester-1".to_string(),
                    target_client_id: "producer-1".to_string(),
                    timeout_ms: 1_000,
                }),
            ),
        )
        .await;
        let envelope = envelope(
            "screenshot-response-1",
            BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
                request_id: "screenshot-request-1".to_string(),
                responding_client_id: "producer-1".to_string(),
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

        post_envelope(&client, &service.handle, &producer, envelope).await;
        service.handle.shutdown().await;
    }

    #[test]
    fn retained_delivery_store_is_bounded_by_bytes() {
        let mut state = ServiceState {
            started_at: Instant::now(),
            clients: HashMap::new(),
            retained_deliveries: VecDeque::new(),
            retained_delivery_bytes: 0,
            next_delivery_sequence: 1,
            pending_requests: HashMap::new(),
            stale_client_timeout: Duration::from_secs(30),
            last_event_time_unix_ms: None,
        };
        let mut outgoing = Vec::new();

        for index in 0..6 {
            state.enqueue_delivery(
                envelope(
                    &format!("large-screenshot-response-{index}"),
                    BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
                        request_id: format!("shot-{index}"),
                        responding_client_id: "producer-1".to_string(),
                        status: ControlStatus::Ok,
                        screenshot: Some(ScreenshotPayload {
                            width: 1,
                            height: 1,
                            media_type: ScreenshotMediaType::Png,
                            data_base64: "x".repeat(2 * 1024 * 1024),
                        }),
                        error: None,
                    }),
                ),
                DeliveryRoute::Target("requester-1".to_string()),
                true,
                &mut outgoing,
            );
        }

        assert!(state.retained_delivery_bytes <= MAX_RETAINED_DELIVERY_BYTES);
        assert!(state.retained_deliveries.len() < 6);
        assert_eq!(outgoing.len(), 6);
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

    async fn current_session_token(shared: &SharedState, client_id: &str) -> String {
        let state = shared.inner.lock().await;
        state
            .clients
            .get(client_id)
            .expect("registered client")
            .session_token
            .clone()
    }

    fn read_descriptor(path: &Path) -> BridgeDescriptor {
        let raw = std::fs::read(path).expect("read descriptor");
        serde_json::from_slice(&raw).expect("parse descriptor")
    }

    fn descriptor_endpoint_url(descriptor: &BridgeDescriptor) -> String {
        match &descriptor.endpoint {
            BridgeEndpoint::LoopbackHttp { url } => url.clone(),
            endpoint => panic!("expected loopback HTTP endpoint, got {endpoint:?}"),
        }
    }

    async fn post_envelope(
        client: &Client,
        handle: &BridgeServiceHandle,
        session: &TestClientSession,
        envelope: BridgeEnvelope,
    ) -> TestClientSession {
        let payload = post_envelope_with_payload_endpoint(
            client,
            &handle.endpoint_url(),
            handle.auth_secret(),
            session,
            envelope,
        )
        .await;
        payload_to_session(session, payload)
    }

    async fn post_envelope_endpoint(
        client: &Client,
        endpoint_url: &str,
        auth_secret: &str,
        session: &TestClientSession,
        envelope: BridgeEnvelope,
    ) -> TestClientSession {
        let payload = post_envelope_with_payload_endpoint(
            client,
            endpoint_url,
            auth_secret,
            session,
            envelope,
        )
        .await;
        payload_to_session(session, payload)
    }

    fn payload_to_session(
        session: &TestClientSession,
        payload: BridgePayload,
    ) -> TestClientSession {
        assert!(matches!(
            payload,
            BridgePayload::Ack(_) | BridgePayload::HelloResponse(_)
        ));
        if let BridgePayload::HelloResponse(response) = payload {
            TestClientSession {
                client_id: session.client_id.clone(),
                session_token: response
                    .client_session_token
                    .expect("hello response client session token"),
            }
        } else {
            session.clone()
        }
    }

    async fn post_envelope_with_payload(
        client: &Client,
        handle: &BridgeServiceHandle,
        session: &TestClientSession,
        envelope: BridgeEnvelope,
    ) -> BridgePayload {
        post_envelope_with_payload_endpoint(
            client,
            &handle.endpoint_url(),
            handle.auth_secret(),
            session,
            envelope,
        )
        .await
    }

    async fn post_envelope_with_payload_endpoint(
        client: &Client,
        endpoint_url: &str,
        auth_secret: &str,
        session: &TestClientSession,
        envelope: BridgeEnvelope,
    ) -> BridgePayload {
        let mut request = client
            .post(format!("{endpoint_url}/message"))
            .bearer_auth(auth_secret);
        if !session.session_token.is_empty() {
            request = request.header(CLIENT_SESSION_HEADER, session.session_token.as_str());
        }
        let response = request
            .json(&envelope)
            .send()
            .await
            .expect("message response");
        assert_eq!(response.status(), StatusCode::OK);
        response
            .json::<BridgeMessageResponse>()
            .await
            .expect("message response json")
            .payload
    }

    async fn register_producer(
        client: &Client,
        handle: &BridgeServiceHandle,
        client_id: &str,
        capabilities: CapabilitySet,
    ) -> TestClientSession {
        register_producer_with_endpoint(
            client,
            &handle.endpoint_url(),
            handle.auth_secret(),
            client_id,
            capabilities,
        )
        .await
    }

    async fn register_producer_with_endpoint(
        client: &Client,
        endpoint_url: &str,
        auth_secret: &str,
        client_id: &str,
        capabilities: CapabilitySet,
    ) -> TestClientSession {
        post_envelope_endpoint(
            client,
            endpoint_url,
            auth_secret,
            &TestClientSession::new(client_id),
            hello_envelope(client_id, auth_secret, ClientRole::Producer, capabilities),
        )
        .await
    }

    async fn register_subscriber(
        client: &Client,
        handle: &BridgeServiceHandle,
        client_id: &str,
        capabilities: CapabilitySet,
    ) -> TestClientSession {
        register_subscriber_with_endpoint(
            client,
            &handle.endpoint_url(),
            handle.auth_secret(),
            client_id,
            capabilities,
        )
        .await
    }

    async fn register_subscriber_with_endpoint(
        client: &Client,
        endpoint_url: &str,
        auth_secret: &str,
        client_id: &str,
        capabilities: CapabilitySet,
    ) -> TestClientSession {
        post_envelope_endpoint(
            client,
            endpoint_url,
            auth_secret,
            &TestClientSession::new(client_id),
            hello_envelope(client_id, auth_secret, ClientRole::Subscriber, capabilities),
        )
        .await
    }

    async fn subscribe(
        client: &Client,
        handle: &BridgeServiceHandle,
        session: &TestClientSession,
        subscriber_id: &str,
        filter: SubscriptionFilter,
    ) {
        subscribe_with_endpoint(
            client,
            &handle.endpoint_url(),
            handle.auth_secret(),
            session,
            subscriber_id,
            filter,
        )
        .await;
    }

    async fn subscribe_with_endpoint(
        client: &Client,
        endpoint_url: &str,
        auth_secret: &str,
        session: &TestClientSession,
        subscriber_id: &str,
        filter: SubscriptionFilter,
    ) {
        post_envelope_endpoint(
            client,
            endpoint_url,
            auth_secret,
            session,
            envelope(
                &format!("subscribe-{subscriber_id}"),
                BridgePayload::Subscribe(SubscribeMessage {
                    subscriber_id: subscriber_id.to_string(),
                    filter,
                }),
            ),
        )
        .await;
    }

    async fn subscribe_to_producer(
        client: &Client,
        handle: &BridgeServiceHandle,
        session: &TestClientSession,
        subscriber_id: &str,
        producer_id: &str,
    ) {
        subscribe_to_producer_with_endpoint(
            client,
            &handle.endpoint_url(),
            handle.auth_secret(),
            session,
            subscriber_id,
            producer_id,
        )
        .await;
    }

    async fn subscribe_to_producer_with_endpoint(
        client: &Client,
        endpoint_url: &str,
        auth_secret: &str,
        session: &TestClientSession,
        subscriber_id: &str,
        producer_id: &str,
    ) {
        subscribe_with_endpoint(
            client,
            endpoint_url,
            auth_secret,
            session,
            subscriber_id,
            SubscriptionFilter {
                levels: Vec::new(),
                event_kinds: vec![EventKind::Console],
                client_ids: vec![producer_id.to_string()],
            },
        )
        .await;
    }

    async fn publish_console(
        client: &Client,
        handle: &BridgeServiceHandle,
        session: &TestClientSession,
        client_id: &str,
        event_id: &str,
        level: ConsoleLevel,
    ) {
        publish_console_with_endpoint(
            client,
            &handle.endpoint_url(),
            handle.auth_secret(),
            session,
            client_id,
            event_id,
            level,
        )
        .await;
    }

    async fn publish_console_with_endpoint(
        client: &Client,
        endpoint_url: &str,
        auth_secret: &str,
        session: &TestClientSession,
        client_id: &str,
        event_id: &str,
        level: ConsoleLevel,
    ) {
        post_envelope_endpoint(
            client,
            endpoint_url,
            auth_secret,
            session,
            envelope(
                event_id,
                BridgePayload::Event(EventPublishMessage {
                    client_id: client_id.to_string(),
                    event_id: event_id.to_string(),
                    event: BridgeEvent::Console(ConsoleEvent {
                        level,
                        text: format!("{event_id} text"),
                    }),
                }),
            ),
        )
        .await;
    }

    async fn next_event_messages<S>(stream: &mut S, count: usize) -> Vec<BridgeSseMessage>
    where
        S: futures::Stream<
                Item = Result<
                    eventsource_stream::Event,
                    eventsource_stream::EventStreamError<reqwest::Error>,
                >,
            > + Unpin,
    {
        let mut messages = Vec::with_capacity(count);
        for _ in 0..count {
            let message = next_sse_message(stream).await;
            assert!(matches!(message.envelope.payload, BridgePayload::Event(_)));
            messages.push(message);
        }
        messages
    }

    fn assert_event_ids(messages: &[BridgeSseMessage], expected: &[&str]) {
        let actual: Vec<&str> = messages
            .iter()
            .map(|message| match &message.envelope.payload {
                BridgePayload::Event(event) => event.event_id.as_str(),
                _ => panic!("expected event payload"),
            })
            .collect();
        assert_eq!(actual, expected);
    }

    async fn open_events(
        client: &Client,
        handle: &BridgeServiceHandle,
        session: &TestClientSession,
        client_id: &str,
    ) -> reqwest::Result<reqwest::Response> {
        open_events_with_endpoint(
            client,
            &handle.endpoint_url(),
            handle.auth_secret(),
            session,
            client_id,
        )
        .await
    }

    async fn open_events_with_endpoint(
        client: &Client,
        endpoint_url: &str,
        auth_secret: &str,
        session: &TestClientSession,
        client_id: &str,
    ) -> reqwest::Result<reqwest::Response> {
        open_events_after_with_endpoint(client, endpoint_url, auth_secret, session, client_id, 0)
            .await
    }

    async fn open_events_after(
        client: &Client,
        handle: &BridgeServiceHandle,
        session: &TestClientSession,
        client_id: &str,
        last_event_id: u64,
    ) -> reqwest::Result<reqwest::Response> {
        open_events_after_with_endpoint(
            client,
            &handle.endpoint_url(),
            handle.auth_secret(),
            session,
            client_id,
            last_event_id,
        )
        .await
    }

    async fn open_events_after_with_endpoint(
        client: &Client,
        endpoint_url: &str,
        auth_secret: &str,
        session: &TestClientSession,
        client_id: &str,
        last_event_id: u64,
    ) -> reqwest::Result<reqwest::Response> {
        client
            .get(format!("{endpoint_url}/events/{client_id}"))
            .bearer_auth(auth_secret)
            .header(CLIENT_SESSION_HEADER, session.session_token.as_str())
            .header("last-event-id", last_event_id.to_string())
            .send()
            .await
    }

    #[derive(Clone)]
    struct TestClientSession {
        client_id: String,
        session_token: String,
    }

    impl TestClientSession {
        fn new(client_id: &str) -> Self {
            Self {
                client_id: client_id.to_string(),
                session_token: String::new(),
            }
        }
    }

    async fn next_sse_message<S>(stream: &mut S) -> BridgeSseMessage
    where
        S: futures::Stream<
                Item = Result<
                    eventsource_stream::Event,
                    eventsource_stream::EventStreamError<reqwest::Error>,
                >,
            > + Unpin,
    {
        let event = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("timed out waiting for SSE message")
            .expect("SSE stream ended")
            .expect("SSE event");
        serde_json::from_str(&event.data).expect("SSE bridge message")
    }

    async fn assert_no_sse_message<S>(stream: &mut S)
    where
        S: futures::Stream<
                Item = Result<
                    eventsource_stream::Event,
                    eventsource_stream::EventStreamError<reqwest::Error>,
                >,
            > + Unpin,
    {
        let event = tokio::time::timeout(Duration::from_millis(150), stream.next()).await;
        assert!(event.is_err(), "unexpected SSE event: {event:?}");
    }

    fn producer_capabilities() -> CapabilitySet {
        CapabilitySet {
            publish_events: true,
            provide_screenshot: true,
            ..CapabilitySet::default()
        }
    }

    fn subscriber_capabilities() -> CapabilitySet {
        CapabilitySet {
            subscribe_events: true,
            request_screenshot: true,
            request_control: true,
            ..CapabilitySet::default()
        }
    }

    fn control_request_envelope(
        request_id: &str,
        requester_client_id: &str,
        target_client_id: &str,
    ) -> BridgeEnvelope {
        envelope(
            request_id,
            BridgePayload::ControlRequest(ControlRequestMessage {
                request_id: request_id.to_string(),
                requester_client_id: requester_client_id.to_string(),
                target_client_id: target_client_id.to_string(),
                command: ControlCommand::ExecuteJavascript {
                    code: "window.location.href".to_string(),
                },
                timeout_ms: 1_000,
            }),
        )
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
