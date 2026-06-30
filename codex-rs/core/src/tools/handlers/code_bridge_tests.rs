use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

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

#[test]
fn screenshot_response_omits_large_inline_image_data() {
    let data_base64 = "x".repeat(MAX_MODEL_VISIBLE_IMAGE_BASE64_BYTES + 1);
    let response = map_screenshot_response(ScreenshotResponseMessage {
        request_id: "shot-large".to_string(),
        responding_client_id: "browser-1".to_string(),
        status: ControlStatus::Ok,
        screenshot: Some(ScreenshotPayload {
            width: 1920,
            height: 1080,
            media_type: ScreenshotMediaType::Png,
            data_base64,
        }),
        error: None,
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
