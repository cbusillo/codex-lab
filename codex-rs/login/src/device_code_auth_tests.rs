use super::*;
use pretty_assertions::assert_eq;
use reqwest::header::HeaderValue;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[test]
fn cloudflare_challenge_detector_matches_body_signals() {
    let headers = HeaderMap::new();

    for body in [
        "Just a moment...",
        "Enable JavaScript and cookies to continue",
        "window._cf_chl_opt = {};",
        "https://challenges.cloudflare.com/cdn-cgi/challenge-platform/",
        "Cloudflare challenge page",
    ] {
        assert!(
            looks_like_cloudflare_challenge(StatusCode::FORBIDDEN, &headers, body),
            "expected challenge body signal: {body}"
        );
    }
}

#[test]
fn cloudflare_challenge_detector_matches_header_signals() {
    for (name, value) in [
        ("cf-mitigated", "challenge"),
        ("server-timing", "chlray=\"abc123\""),
        ("set-cookie", "__cf_bm=abc123; Path=/; HttpOnly"),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_static(value));

        assert!(
            looks_like_cloudflare_challenge(StatusCode::FORBIDDEN, &headers, ""),
            "expected challenge header signal: {name}"
        );
    }
}

#[test]
fn cloudflare_challenge_detector_checks_all_set_cookie_headers() {
    let mut headers = HeaderMap::new();
    headers.append("set-cookie", HeaderValue::from_static("session=abc123"));
    headers.append("set-cookie", HeaderValue::from_static("__cf_bm=abc123"));

    assert!(looks_like_cloudflare_challenge(
        StatusCode::FORBIDDEN,
        &headers,
        "",
    ));
}

#[test]
fn cloudflare_challenge_detector_ignores_plain_cloudflare_proxy_headers() {
    let mut headers = HeaderMap::new();
    headers.insert("cf-ray", HeaderValue::from_static("abc123"));

    assert!(!looks_like_cloudflare_challenge(
        StatusCode::FORBIDDEN,
        &headers,
        "server returned forbidden",
    ));
}

#[test]
fn cloudflare_challenge_detector_ignores_non_forbidden_status() {
    let mut headers = HeaderMap::new();
    headers.insert("cf-ray", HeaderValue::from_static("abc123"));

    assert!(!looks_like_cloudflare_challenge(
        StatusCode::SERVICE_UNAVAILABLE,
        &headers,
        "cloudflare",
    ));
}

#[tokio::test]
async fn request_user_code_preserves_not_found_diagnostic() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(StatusCode::NOT_FOUND.as_u16()))
        .mount(&server)
        .await;

    let result = request_user_code(
        &reqwest::Client::new(),
        &server.uri(),
        &server.uri(),
        "test-client",
    )
    .await;
    let err = match result {
        Ok(_) => panic!("404 should return the device-code diagnostic"),
        Err(err) => err,
    };

    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    assert!(
        err.to_string().contains("device code login is not enabled"),
        "unexpected error: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_fallback_requests_user_code_from_browser_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/device"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<main>device login</main>"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_auth_id": "device-auth-id",
            "user_code": "USER-CODE",
            "interval": "2",
        })))
        .mount(&server)
        .await;

    let manager = BrowserManager::new(BrowserConfig {
        enabled: true,
        headless: true,
        ..Default::default()
    });

    let result = request_user_code_via_browser_with_manager(
        &manager,
        &server.uri(),
        "test-client",
        Duration::ZERO,
    )
    .await;
    let _ = manager.stop().await;

    let user_code = match result {
        Ok(user_code) => user_code,
        Err(err) => panic!("browser fallback should request user code: {err}"),
    };

    assert_eq!(
        user_code,
        UserCodeResp {
            device_auth_id: "device-auth-id".to_string(),
            user_code: "USER-CODE".to_string(),
            interval: 2,
        }
    );
}
