use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn screenshot_response(screenshot: ScreenshotPayload) -> CodeBridgeResult {
    map_screenshot_response(ScreenshotResponseMessage {
        request_id: "shot-1".to_string(),
        responding_client_id: "browser-1".to_string(),
        status: ControlStatus::Ok,
        screenshot: Some(screenshot),
        error: None,
    })
}

#[test]
fn validate_timeout_rejects_zero_and_too_large() {
    assert_eq!(
        validate_timeout(Some(0)),
        Err("timeoutMs must be between 1 and 10000".to_string())
    );
    assert_eq!(
        validate_timeout(Some(MAX_CONTROL_TIMEOUT_MS + 1)),
        Err("timeoutMs must be between 1 and 10000".to_string())
    );
    assert_eq!(validate_timeout(Some(1)), Ok(1));
}

#[test]
fn required_javascript_code_rejects_empty_and_oversized() {
    assert_eq!(
        required_code(Some(String::new())),
        Err("code must not be empty".to_string())
    );
    assert_eq!(
        required_code(Some("x".repeat(MAX_EVENT_TEXT_BYTES + 1))),
        Err(format!("code must be at most {MAX_EVENT_TEXT_BYTES} bytes"))
    );
}

#[test]
fn required_target_client_id_rejects_missing_and_empty() {
    assert_eq!(
        required_target_client_id(None),
        Err("targetClientId is required for this action".to_string())
    );
    assert_eq!(
        required_target_client_id(Some(String::new())),
        Err("targetClientId must not be empty".to_string())
    );
}

#[test]
fn unavailable_response_does_not_expose_descriptor_path() {
    let path = PathBuf::from("/tmp/private/code-bridge.json");

    assert_eq!(
        unavailable_response(&path).response,
        json!({
            "status": "unavailable",
            "message": "Code Bridge descriptor is unavailable for this workspace.",
        })
    );
}

#[test]
fn screenshot_response_splits_metadata_from_image_data() {
    let response = map_screenshot_response(ScreenshotResponseMessage {
        request_id: "shot-1".to_string(),
        responding_client_id: "browser-1".to_string(),
        status: ControlStatus::Ok,
        screenshot: Some(ScreenshotPayload {
            width: 320,
            height: 200,
            media_type: ScreenshotMediaType::Png,
            data_base64: "ZmFrZS1wbmc=".to_string(),
        }),
        error: None,
    });

    assert_eq!(
        response.response,
        json!({
            "status": "ok",
            "requestId": "shot-1",
            "respondingClientId": "browser-1",
            "screenshot": {
                "width": 320,
                "height": 200,
                "mediaType": "image/png",
                "imageOmitted": null,
            },
            "error": null,
        })
    );
    assert_eq!(
        response.image.map(|image| image.data_url()),
        Some("data:image/png;base64,ZmFrZS1wbmc=".to_string())
    );
}

/// An oversized-but-protocol-valid screenshot: small enough that envelope validation accepts it,
/// large enough that inlining it would blow the model-visible budget.
#[test]
fn screenshot_response_omits_large_inline_image_data() {
    let data_base64 = "x".repeat(MAX_MODEL_VISIBLE_IMAGE_BASE64_BYTES + 1);
    let response = screenshot_response(ScreenshotPayload {
        width: 1920,
        height: 1080,
        media_type: ScreenshotMediaType::Png,
        data_base64,
    });

    assert_eq!(response.response["status"], "ok");
    assert_eq!(response.response["screenshot"]["width"], 1920);
    assert_eq!(
        response.response["screenshot"]["imageOmitted"],
        json!({
            "reason": "screenshot image exceeded the model-visible inline size limit",
            "base64Bytes": MAX_MODEL_VISIBLE_IMAGE_BASE64_BYTES + 1,
            "maxBase64Bytes": MAX_MODEL_VISIBLE_IMAGE_BASE64_BYTES,
        })
    );
    assert!(response.image.is_none());
}

#[test]
fn screenshot_response_omits_image_over_the_patch_budget() {
    let response = screenshot_response(ScreenshotPayload {
        width: MAX_SCREENSHOT_WIDTH,
        height: MAX_SCREENSHOT_HEIGHT,
        media_type: ScreenshotMediaType::Png,
        data_base64: "ZmFrZS1wbmc=".to_string(),
    });

    assert_eq!(
        response.response["screenshot"]["imageOmitted"],
        json!({
            "reason": "screenshot image exceeded the model-visible inline token limit",
            "patches": 16_384,
            "maxPatches": MAX_MODEL_VISIBLE_IMAGE_PATCHES,
        })
    );
    assert!(response.image.is_none());
}

#[test]
fn screenshot_response_inlines_a_four_k_capture() {
    let response = screenshot_response(ScreenshotPayload {
        width: 3840,
        height: 2160,
        media_type: ScreenshotMediaType::Png,
        data_base64: "ZmFrZS1wbmc=".to_string(),
    });

    assert_eq!(response.response["screenshot"]["imageOmitted"], json!(null));
    assert!(response.image.is_some());
}

#[test]
fn screenshot_response_bounds_oversized_peer_identifiers() {
    let response = map_screenshot_response(ScreenshotResponseMessage {
        request_id: "r".repeat(MAX_SCREENSHOT_BYTES),
        responding_client_id: "c".repeat(MAX_SCREENSHOT_BYTES),
        status: ControlStatus::Ok,
        screenshot: None,
        error: None,
    });

    for field in ["requestId", "respondingClientId"] {
        let value = response.response[field].as_str().unwrap_or_default();
        assert!(value.len() <= MAX_CODE_BRIDGE_ID_BYTES, "{field}");
        assert!(value.ends_with(TRUNCATED_FIELD_NOTICE), "{field}");
    }
}

#[test]
fn control_response_bounds_oversized_peer_identifiers() {
    let response = map_control_response(ControlResponseMessage {
        request_id: "r".repeat(MAX_SCREENSHOT_BYTES),
        responding_client_id: "c".repeat(MAX_SCREENSHOT_BYTES),
        status: ControlStatus::Ok,
        summary: String::new(),
        result: None,
        error: None,
    });

    for field in ["requestId", "respondingClientId"] {
        let value = response.response[field].as_str().unwrap_or_default();
        assert!(value.len() <= MAX_CODE_BRIDGE_ID_BYTES, "{field}");
        assert!(value.ends_with(TRUNCATED_FIELD_NOTICE), "{field}");
    }
}

#[test]
fn service_status_bounds_oversized_protocol_version() {
    let status = model_visible_service_status(&BridgeServiceStatus {
        protocol_version: "v".repeat(MAX_SCREENSHOT_BYTES),
        connected_producer_count: 1,
        connected_subscriber_count: 2,
        uptime_ms: 3,
        last_event_time_unix_ms: None,
    });

    let protocol_version = status["protocolVersion"].as_str().unwrap_or_default();
    assert!(protocol_version.len() <= MAX_CODE_BRIDGE_ID_BYTES);
    assert!(protocol_version.ends_with(TRUNCATED_FIELD_NOTICE));
    assert_eq!(status["connectedProducerCount"], 1);
}

#[test]
fn control_response_caps_model_visible_summary_and_result() {
    let response = map_control_response(ControlResponseMessage {
        request_id: "js-1".to_string(),
        responding_client_id: "browser-1".to_string(),
        status: ControlStatus::Ok,
        summary: "x".repeat(MAX_MODEL_VISIBLE_CONTROL_FIELD_BYTES + 100),
        result: Some(json!({ "value": "y".repeat(MAX_MODEL_VISIBLE_CONTROL_FIELD_BYTES + 100) })),
        error: None,
    });

    assert_eq!(response.response["status"], "ok");
    let summary = response.response["summary"].as_str().unwrap_or_default();
    assert!(summary.len() <= MAX_MODEL_VISIBLE_CONTROL_FIELD_BYTES);
    assert!(summary.ends_with(TRUNCATED_FIELD_NOTICE));
    assert_eq!(response.response["result"]["truncated"], true);
}

/// Model-visible envelope overhead for the small status/id fields wrapped around a bounded
/// error string. Kept tight so the assertions below fail if the error itself grows.
const ERROR_ENVELOPE_SLACK_BYTES: usize = 256;

#[test]
fn screenshot_response_bounds_oversized_peer_error_message() {
    let response = map_screenshot_response(ScreenshotResponseMessage {
        request_id: "shot-err".to_string(),
        responding_client_id: "browser-1".to_string(),
        status: ControlStatus::Failed,
        screenshot: None,
        error: Some(ErrorMessage {
            code: ErrorCode::PayloadTooLarge,
            message: "e".repeat(MAX_SCREENSHOT_BYTES),
        }),
    });
    let rendered = serde_json::to_string(&response.response).expect("serialize response");

    let message = response.response["error"]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(message.len() <= MAX_MODEL_VISIBLE_ERROR_MESSAGE_BYTES);
    assert!(message.ends_with(TRUNCATED_FIELD_NOTICE));
    assert_eq!(response.response["error"]["code"], "payloadTooLarge");
    assert!(
        rendered.len() <= MAX_MODEL_VISIBLE_ERROR_MESSAGE_BYTES + ERROR_ENVELOPE_SLACK_BYTES,
        "rendered {} bytes",
        rendered.len()
    );
}

#[test]
fn control_response_bounds_oversized_peer_error_message() {
    let response = map_control_response(ControlResponseMessage {
        request_id: "js-err".to_string(),
        responding_client_id: "browser-1".to_string(),
        status: ControlStatus::Failed,
        summary: String::new(),
        result: None,
        error: Some(ErrorMessage {
            code: ErrorCode::InvalidPayload,
            message: "e".repeat(MAX_EVENT_TEXT_BYTES),
        }),
    });

    let message = response.response["error"]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(message.len() <= MAX_MODEL_VISIBLE_ERROR_MESSAGE_BYTES);
    assert!(message.ends_with(TRUNCATED_FIELD_NOTICE));
}

#[test]
fn unexpected_ack_payload_reports_kind_without_screenshot_base64() {
    let data_base64 = "A".repeat(MAX_SCREENSHOT_BYTES);
    let err = expect_ack(BridgePayload::ScreenshotResponse(
        ScreenshotResponseMessage {
            request_id: "shot-unexpected".to_string(),
            responding_client_id: "browser-1".to_string(),
            status: ControlStatus::Ok,
            screenshot: Some(ScreenshotPayload {
                width: 1920,
                height: 1080,
                media_type: ScreenshotMediaType::Png,
                data_base64: data_base64.clone(),
            }),
            error: None,
        },
    ))
    .expect_err("an unexpected payload must not be accepted as an ack");

    let response = error_response("failed", err.to_string());
    let rendered = serde_json::to_string(&response.response).expect("serialize response");

    assert!(!rendered.contains(&data_base64[..64]));
    assert!(rendered.contains("screenshotResponse"));
    assert!(response.image.is_none());
    assert!(
        rendered.len() <= MAX_MODEL_VISIBLE_ERROR_MESSAGE_BYTES + ERROR_ENVELOPE_SLACK_BYTES,
        "rendered {} bytes",
        rendered.len()
    );
}

#[test]
fn error_response_bounds_oversized_peer_error_text() {
    let response = error_response("failed", "z".repeat(MAX_SCREENSHOT_BYTES));
    let rendered = serde_json::to_string(&response.response).expect("serialize response");

    let message = response.response["error"]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(message.len() <= MAX_MODEL_VISIBLE_ERROR_MESSAGE_BYTES);
    assert!(message.ends_with(TRUNCATED_FIELD_NOTICE));
    assert!(
        rendered.len() <= MAX_MODEL_VISIBLE_ERROR_MESSAGE_BYTES + ERROR_ENVELOPE_SLACK_BYTES,
        "rendered {} bytes",
        rendered.len()
    );
}
