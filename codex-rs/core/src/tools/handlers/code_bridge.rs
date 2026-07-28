use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::code_bridge_spec::CODE_BRIDGE_TOOL_NAME;
use crate::tools::handlers::code_bridge_spec::create_code_bridge_tool;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_code_bridge_client::CodeBridgeClient;
use codex_code_bridge_client::CodeBridgeClientError;
use codex_code_bridge_client::CodeBridgeEventStream;
use codex_code_bridge_client::CodeBridgeSession;
use codex_code_bridge_client::ControlRequest;
use codex_code_bridge_client::ScreenshotRequest;
use codex_code_bridge_protocol::BridgePayload;
use codex_code_bridge_protocol::BridgeServiceStatus;
use codex_code_bridge_protocol::CapabilitySet;
use codex_code_bridge_protocol::ClientMetadata;
use codex_code_bridge_protocol::ClientRole;
use codex_code_bridge_protocol::ControlCommand;
use codex_code_bridge_protocol::ControlResponseMessage;
use codex_code_bridge_protocol::ControlStatus;
use codex_code_bridge_protocol::DESCRIPTOR_RELATIVE_PATH;
use codex_code_bridge_protocol::ErrorCode;
use codex_code_bridge_protocol::ErrorMessage;
use codex_code_bridge_protocol::MAX_CONTROL_TIMEOUT_MS;
use codex_code_bridge_protocol::MAX_EVENT_TEXT_BYTES;
use codex_code_bridge_protocol::MAX_SCREENSHOT_BYTES;
use codex_code_bridge_protocol::MAX_SCREENSHOT_HEIGHT;
use codex_code_bridge_protocol::MAX_SCREENSHOT_WIDTH;
use codex_code_bridge_protocol::ScreenshotMediaType;
use codex_code_bridge_protocol::ScreenshotPayload;
use codex_code_bridge_protocol::ScreenshotResponseMessage;
use codex_code_bridge_protocol::SourceKind;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ImageDetail;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::borrow::Cow;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::time::Duration;
use tokio::time::timeout;

const CONTROL_RESPONSE_GRACE_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_CONTROL_TIMEOUT_MS: u64 = MAX_CONTROL_TIMEOUT_MS;
const MAX_CODE_BRIDGE_ID_BYTES: usize = 256;
const MAX_MODEL_VISIBLE_CONTROL_FIELD_BYTES: usize = 1024;
/// Hard cap for every error string the bridge can surface to the model. Bridge peers are
/// untrusted, so `ErrorMessage.message` (and anything derived from a peer payload) must be
/// bounded before it reaches the tool output.
const MAX_MODEL_VISIBLE_ERROR_MESSAGE_BYTES: usize = 1024;
/// Hard cap for the encoded image a screenshot may inline into model-visible tool output.
///
/// Protocol validation already rejects screenshots whose base64 string exceeds
/// `MAX_SCREENSHOT_BYTES` (2 MiB), so any cap at or above that value is dead code. This cap is
/// deliberately well under it: 1 MiB of base64 is 768 KiB of encoded image, which is far more
/// than a real screenshot needs while keeping a single tool result from dominating a request.
const MAX_MODEL_VISIBLE_IMAGE_BASE64_BYTES: usize = 1024 * 1024;
/// Patch edge length used by the vision token model.
/// See https://platform.openai.com/docs/guides/images-vision#calculating-costs.
const IMAGE_PATCH_SIZE: u32 = 32;
/// Hard cap on the 32px patch count of an inlined screenshot.
///
/// Image token cost scales with patch count, and protocol validation only bounds screenshots at
/// 4096x4096 (16,384 patches) — over the 10K-token ceiling a single model-visible item is allowed
/// to occupy. 9,000 patches keeps the image under that ceiling while still admitting a 4K
/// (3840x2160, 8,160 patch) display capture.
const MAX_MODEL_VISIBLE_IMAGE_PATCHES: u64 = 9_000;
// Both image caps must bind strictly inside protocol validation. A cap at or above the protocol
// limit is unreachable, which is exactly how the previous screenshot cap became dead code.
const _: () = assert!(MAX_MODEL_VISIBLE_IMAGE_BASE64_BYTES < MAX_SCREENSHOT_BYTES);
const _: () = assert!(
    (MAX_SCREENSHOT_WIDTH.div_ceil(IMAGE_PATCH_SIZE) as u64)
        * (MAX_SCREENSHOT_HEIGHT.div_ceil(IMAGE_PATCH_SIZE) as u64)
        > MAX_MODEL_VISIBLE_IMAGE_PATCHES
);
const TRUNCATED_FIELD_NOTICE: &str = " [truncated by code_bridge]";
static NEXT_REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub struct CodeBridgeHandler;

struct CodeBridgeResult {
    response: Value,
    image: Option<ScreenshotToolImage>,
}

struct ScreenshotToolImage {
    media_type: ScreenshotMediaType,
    data_base64: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodeBridgeArgs {
    action: CodeBridgeAction,
    #[serde(default)]
    target_client_id: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    level: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum CodeBridgeAction {
    Status,
    Subscribe,
    Screenshot,
    Javascript,
}

impl ToolExecutor<ToolInvocation> for CodeBridgeHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(CODE_BRIDGE_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        create_code_bridge_tool()
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(handle_invocation(invocation))
    }
}

async fn handle_invocation(
    invocation: ToolInvocation,
) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
    {
        let ToolInvocation { turn, payload, .. } = invocation;
        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "{CODE_BRIDGE_TOOL_NAME} handler received unsupported payload"
                )));
            }
        };
        let args: CodeBridgeArgs = parse_arguments(&arguments)?;
        let descriptor_path = turn
            .config
            .codex_home
            .join(DESCRIPTOR_RELATIVE_PATH)
            .to_path_buf();
        let result = handle_code_bridge(args, descriptor_path).await;
        let success = result
            .response
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| matches!(status, "available" | "ok"));
        let content = serde_json::to_string(&result.response).map_err(|err| {
            FunctionCallError::Fatal(format!(
                "failed to serialize {CODE_BRIDGE_TOOL_NAME} response: {err}"
            ))
        })?;
        let mut items = vec![FunctionCallOutputContentItem::InputText { text: content }];
        if success && let Some(image) = result.image {
            items.push(FunctionCallOutputContentItem::InputImage {
                image_url: image.data_url(),
                detail: Some(ImageDetail::High),
            });
        }
        Ok(boxed_tool_output(FunctionToolOutput::from_content(
            items,
            Some(success),
        )))
    }
}

impl CoreToolRuntime for CodeBridgeHandler {}

async fn handle_code_bridge(args: CodeBridgeArgs, descriptor_path: PathBuf) -> CodeBridgeResult {
    match args.action {
        CodeBridgeAction::Status => status_response(&descriptor_path).await,
        CodeBridgeAction::Subscribe => subscribe_response(args),
        CodeBridgeAction::Screenshot => screenshot_response(args, &descriptor_path).await,
        CodeBridgeAction::Javascript => javascript_response(args, &descriptor_path).await,
    }
}

async fn status_response(descriptor_path: &Path) -> CodeBridgeResult {
    let Ok(client) = client_from_descriptor(descriptor_path) else {
        return unavailable_response(descriptor_path);
    };
    match client.status().await {
        Ok(status) => CodeBridgeResult::text(json!({
            "status": "available",
            "service": model_visible_service_status(&status),
        })),
        Err(err) => error_response("unavailable", format!("Code Bridge status failed: {err}")),
    }
}

/// Re-render the bridge service status with every free-form field bounded. `protocol_version` is
/// echoed from the bridge service over HTTP and never passes through envelope validation, so it
/// cannot be serialized straight into model-visible output.
fn model_visible_service_status(status: &BridgeServiceStatus) -> Value {
    json!({
        "protocolVersion": model_visible_id(&status.protocol_version),
        "connectedProducerCount": status.connected_producer_count,
        "connectedSubscriberCount": status.connected_subscriber_count,
        "uptimeMs": status.uptime_ms,
        "lastEventTimeUnixMs": status.last_event_time_unix_ms,
    })
}

fn subscribe_response(args: CodeBridgeArgs) -> CodeBridgeResult {
    CodeBridgeResult::text(json!({
        "status": "reserved",
        "message": "Code Bridge event streaming is reserved until the core bridge listener and bounded context fragment are enabled.",
        "level": args.level,
    }))
}

async fn screenshot_response(args: CodeBridgeArgs, descriptor_path: &Path) -> CodeBridgeResult {
    let target_client_id = match required_target_client_id(args.target_client_id) {
        Ok(target_client_id) => target_client_id,
        Err(message) => return error_response("failed", message),
    };
    let timeout_ms = match validate_timeout(args.timeout_ms) {
        Ok(timeout_ms) => timeout_ms,
        Err(message) => return error_response("failed", message),
    };
    let Ok((client, session, mut events)) = control_session(
        descriptor_path,
        "core-code-bridge-screenshot",
        CapabilitySet {
            subscribe_events: true,
            request_screenshot: true,
            ..CapabilitySet::default()
        },
    )
    .await
    else {
        return unavailable_response(descriptor_path);
    };
    let request_id = request_id("screenshot");
    match client
        .request_screenshot(
            &session,
            ScreenshotRequest {
                request_id: request_id.clone(),
                target_client_id,
                timeout_ms,
            },
        )
        .await
        .and_then(expect_ack)
    {
        Ok(()) => {}
        Err(err) => return error_response("failed", err.to_string()),
    }
    match wait_for_screenshot_response(&mut events, &request_id, timeout_ms).await {
        Ok(response) => map_screenshot_response(response),
        Err(err) => error_response("failed", err.to_string()),
    }
}

async fn javascript_response(args: CodeBridgeArgs, descriptor_path: &Path) -> CodeBridgeResult {
    let target_client_id = match required_target_client_id(args.target_client_id) {
        Ok(target_client_id) => target_client_id,
        Err(message) => return error_response("failed", message),
    };
    let code = match required_code(args.code) {
        Ok(code) => code,
        Err(message) => return error_response("failed", message),
    };
    let timeout_ms = match validate_timeout(args.timeout_ms) {
        Ok(timeout_ms) => timeout_ms,
        Err(message) => return error_response("failed", message),
    };
    let Ok((client, session, mut events)) = control_session(
        descriptor_path,
        "core-code-bridge-javascript",
        CapabilitySet {
            subscribe_events: true,
            request_control: true,
            ..CapabilitySet::default()
        },
    )
    .await
    else {
        return unavailable_response(descriptor_path);
    };
    let request_id = request_id("javascript");
    match client
        .request_control(
            &session,
            ControlRequest {
                request_id: request_id.clone(),
                target_client_id,
                command: ControlCommand::ExecuteJavascript { code },
                timeout_ms,
            },
        )
        .await
        .and_then(expect_ack)
    {
        Ok(()) => {}
        Err(err) => return error_response("failed", err.to_string()),
    }
    match wait_for_control_response(&mut events, &request_id, timeout_ms).await {
        Ok(response) => map_control_response(response),
        Err(err) => error_response("failed", err.to_string()),
    }
}

async fn control_session(
    descriptor_path: &Path,
    client_id_prefix: &str,
    capabilities: CapabilitySet,
) -> Result<(CodeBridgeClient, CodeBridgeSession, CodeBridgeEventStream), CodeBridgeClientError> {
    let client = client_from_descriptor(descriptor_path)?;
    let session = client
        .hello(
            request_id(client_id_prefix),
            ClientRole::Subscriber,
            capabilities,
            ClientMetadata {
                source_kind: SourceKind::Cli,
                label: Some("core".to_string()),
                provenance: None,
            },
        )
        .await?;
    let events = client.events(&session, /*last_event_id*/ 0).await?;
    Ok((client, session, events))
}

fn client_from_descriptor(
    descriptor_path: &Path,
) -> Result<CodeBridgeClient, CodeBridgeClientError> {
    CodeBridgeClient::from_descriptor_path(descriptor_path)
}

async fn wait_for_screenshot_response(
    events: &mut CodeBridgeEventStream,
    request_id: &str,
    timeout_ms: u64,
) -> Result<ScreenshotResponseMessage, CodeBridgeClientError> {
    timeout(response_wait_timeout(timeout_ms), async {
        loop {
            let message = events.next_message().await?;
            if let BridgePayload::ScreenshotResponse(response) = message.envelope.payload
                && response.request_id == request_id
            {
                return Ok(response);
            }
        }
    })
    .await
    .unwrap_or_else(|_| Ok(screenshot_timeout_response(request_id)))
}

async fn wait_for_control_response(
    events: &mut CodeBridgeEventStream,
    request_id: &str,
    timeout_ms: u64,
) -> Result<ControlResponseMessage, CodeBridgeClientError> {
    timeout(response_wait_timeout(timeout_ms), async {
        loop {
            let message = events.next_message().await?;
            if let BridgePayload::ControlResponse(response) = message.envelope.payload
                && response.request_id == request_id
            {
                return Ok(response);
            }
        }
    })
    .await
    .unwrap_or_else(|_| Ok(control_timeout_response(request_id)))
}

fn response_wait_timeout(timeout_ms: u64) -> Duration {
    Duration::from_millis(timeout_ms) + CONTROL_RESPONSE_GRACE_TIMEOUT
}

fn screenshot_timeout_response(request_id: &str) -> ScreenshotResponseMessage {
    ScreenshotResponseMessage {
        request_id: request_id.to_string(),
        responding_client_id: String::new(),
        status: ControlStatus::TimedOut,
        screenshot: None,
        error: Some(timeout_error(request_id)),
    }
}

fn control_timeout_response(request_id: &str) -> ControlResponseMessage {
    ControlResponseMessage {
        request_id: request_id.to_string(),
        responding_client_id: String::new(),
        status: ControlStatus::TimedOut,
        summary: String::new(),
        result: None,
        error: Some(timeout_error(request_id)),
    }
}

fn timeout_error(request_id: &str) -> ErrorMessage {
    ErrorMessage {
        code: ErrorCode::Timeout,
        message: format!("Code Bridge request {request_id} timed out"),
    }
}

fn expect_ack(payload: BridgePayload) -> Result<(), CodeBridgeClientError> {
    match payload {
        BridgePayload::Ack(_) => Ok(()),
        BridgePayload::Error(error) => Err(CodeBridgeClientError::HelloRejected(
            model_visible_error_text(&error.message),
        )),
        other => Err(CodeBridgeClientError::HelloRejected(format!(
            "unexpected Code Bridge response payload kind: {}",
            bridge_payload_kind(&other)
        ))),
    }
}

/// Bounded, non-`Debug` label for a bridge payload. Payload bodies can carry megabytes of
/// screenshot base64, so only the variant name is ever model-visible.
fn bridge_payload_kind(payload: &BridgePayload) -> &'static str {
    match payload {
        BridgePayload::Hello(_) => "hello",
        BridgePayload::HelloResponse(_) => "helloResponse",
        BridgePayload::Heartbeat(_) => "heartbeat",
        BridgePayload::Event(_) => "event",
        BridgePayload::Subscribe(_) => "subscribe",
        BridgePayload::Ack(_) => "ack",
        BridgePayload::Error(_) => "error",
        BridgePayload::ScreenshotRequest(_) => "screenshotRequest",
        BridgePayload::ScreenshotResponse(_) => "screenshotResponse",
        BridgePayload::ControlRequest(_) => "controlRequest",
        BridgePayload::ControlResponse(_) => "controlResponse",
    }
}

fn map_screenshot_response(response: ScreenshotResponseMessage) -> CodeBridgeResult {
    let (screenshot, image) = response
        .screenshot
        .map(split_screenshot_payload)
        .map(|(metadata, image)| (Some(metadata), image))
        .unwrap_or((None, None));
    CodeBridgeResult {
        response: json!({
        "status": map_control_status(response.status),
        "requestId": model_visible_id(&response.request_id),
        "respondingClientId": model_visible_id(&response.responding_client_id),
        "screenshot": screenshot,
        "error": model_visible_error(response.error),
        }),
        image,
    }
}

fn map_control_response(response: ControlResponseMessage) -> CodeBridgeResult {
    let summary =
        truncate_model_visible_text(&response.summary, MAX_MODEL_VISIBLE_CONTROL_FIELD_BYTES);
    let result = response
        .result
        .map(|result| model_visible_json_result(result, MAX_MODEL_VISIBLE_CONTROL_FIELD_BYTES));
    CodeBridgeResult::text(json!({
        "status": map_control_status(response.status),
        "requestId": model_visible_id(&response.request_id),
        "respondingClientId": model_visible_id(&response.responding_client_id),
        "summary": summary,
        "result": result,
        "error": model_visible_error(response.error),
    }))
}

fn map_control_status(status: ControlStatus) -> &'static str {
    match status {
        ControlStatus::Ok => "ok",
        ControlStatus::Failed => "failed",
        ControlStatus::TimedOut => "timedOut",
        ControlStatus::Denied => "denied",
    }
}

fn unavailable_response(_descriptor_path: &Path) -> CodeBridgeResult {
    CodeBridgeResult::text(json!({
        "status": "unavailable",
        "message": "Code Bridge descriptor is unavailable for this workspace.",
    }))
}

fn error_response(status: &str, message: String) -> CodeBridgeResult {
    CodeBridgeResult::text(json!({
        "status": status,
        "error": {
            "message": model_visible_error_text(&message),
        },
    }))
}

fn model_visible_error(error: Option<ErrorMessage>) -> Option<Value> {
    error.map(|error| {
        json!({
            "code": error.code,
            "message": model_visible_error_text(&error.message),
        })
    })
}

fn model_visible_error_text(message: &str) -> String {
    truncate_model_visible_text(message, MAX_MODEL_VISIBLE_ERROR_MESSAGE_BYTES).into_owned()
}

/// Bound a peer-supplied identifier (client id, request id, protocol version). These are all
/// expected to be short, but nothing on the wire enforces that.
fn model_visible_id(value: &str) -> String {
    truncate_model_visible_text(value, MAX_CODE_BRIDGE_ID_BYTES).into_owned()
}

fn split_screenshot_payload(payload: ScreenshotPayload) -> (Value, Option<ScreenshotToolImage>) {
    let media_type = payload.media_type;
    let base64_len = payload.data_base64.len();
    // Base64 cannot be trimmed to fit without producing an image the model cannot decode, so an
    // over-budget screenshot is omitted entirely and only its metadata stays model-visible.
    let omitted = if base64_len > MAX_MODEL_VISIBLE_IMAGE_BASE64_BYTES {
        Some(json!({
            "reason": "screenshot image exceeded the model-visible inline size limit",
            "base64Bytes": base64_len,
            "maxBase64Bytes": MAX_MODEL_VISIBLE_IMAGE_BASE64_BYTES,
        }))
    } else {
        image_patch_count(payload.width, payload.height)
            .filter(|patches| *patches > MAX_MODEL_VISIBLE_IMAGE_PATCHES)
            .map(|patches| {
                json!({
                    "reason": "screenshot image exceeded the model-visible inline token limit",
                    "patches": patches,
                    "maxPatches": MAX_MODEL_VISIBLE_IMAGE_PATCHES,
                })
            })
    };
    let image = omitted.is_none().then_some(ScreenshotToolImage {
        media_type,
        data_base64: payload.data_base64,
    });
    (
        json!({
            "width": payload.width,
            "height": payload.height,
            "mediaType": media_type_name(media_type),
            "imageOmitted": omitted,
        }),
        image,
    )
}

/// Number of 32px vision patches a screenshot of these dimensions occupies, or `None` when the
/// payload declares a degenerate size that carries no image.
fn image_patch_count(width: u32, height: u32) -> Option<u64> {
    if width == 0 || height == 0 {
        return None;
    }
    Some(u64::from(width.div_ceil(IMAGE_PATCH_SIZE)) * u64::from(height.div_ceil(IMAGE_PATCH_SIZE)))
}

fn media_type_name(media_type: ScreenshotMediaType) -> &'static str {
    match media_type {
        ScreenshotMediaType::Png => "image/png",
        ScreenshotMediaType::Jpeg => "image/jpeg",
    }
}

fn model_visible_json_result(result: Value, max_bytes: usize) -> Value {
    let Ok(serialized) = serde_json::to_vec(&result) else {
        return json!({
            "truncated": true,
            "message": "Code Bridge JavaScript result could not be serialized for model-visible output.",
        });
    };
    if serialized.len() <= max_bytes {
        return result;
    }
    json!({
        "truncated": true,
        "originalBytes": serialized.len(),
        "message": format!(
            "Code Bridge JavaScript result exceeded the {max_bytes} byte model-visible limit and was omitted."
        ),
    })
}

fn truncate_model_visible_text(text: &str, max_bytes: usize) -> Cow<'_, str> {
    if text.len() <= max_bytes {
        return Cow::Borrowed(text);
    }

    let payload_limit = max_bytes.saturating_sub(TRUNCATED_FIELD_NOTICE.len());
    let truncated = take_bytes_at_char_boundary(text, payload_limit);
    Cow::Owned(format!("{truncated}{TRUNCATED_FIELD_NOTICE}"))
}

fn take_bytes_at_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }

    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &text[..boundary]
}

impl CodeBridgeResult {
    fn text(response: Value) -> Self {
        Self {
            response,
            image: None,
        }
    }
}

impl ScreenshotToolImage {
    fn data_url(&self) -> String {
        format!(
            "data:{};base64,{}",
            media_type_name(self.media_type),
            self.data_base64
        )
    }
}

fn required_target_client_id(value: Option<String>) -> Result<String, String> {
    let value = value.ok_or_else(|| "targetClientId is required for this action".to_string())?;
    validate_text_bytes("targetClientId", &value, MAX_CODE_BRIDGE_ID_BYTES)?;
    Ok(value)
}

fn required_code(value: Option<String>) -> Result<String, String> {
    let value = value.ok_or_else(|| "code is required for javascript action".to_string())?;
    validate_text_bytes("code", &value, MAX_EVENT_TEXT_BYTES)?;
    Ok(value)
}

fn validate_text_bytes(field_name: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{field_name} must not be empty"));
    }
    if value.len() > max_bytes {
        return Err(format!("{field_name} must be at most {max_bytes} bytes"));
    }
    Ok(())
}

fn validate_timeout(timeout_ms: Option<u64>) -> Result<u64, String> {
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_CONTROL_TIMEOUT_MS);
    if timeout_ms == 0 || timeout_ms > MAX_CONTROL_TIMEOUT_MS {
        return Err(format!(
            "timeoutMs must be between 1 and {MAX_CONTROL_TIMEOUT_MS}"
        ));
    }
    Ok(timeout_ms)
}

fn request_id(prefix: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = NEXT_REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{timestamp}-{sequence}")
}

#[cfg(test)]
#[path = "code_bridge_tests.rs"]
mod tests;
