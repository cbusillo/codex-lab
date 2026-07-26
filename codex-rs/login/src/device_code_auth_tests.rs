use super::*;

#[test]
fn device_code_prompt_renders_phishing_warning() {
    let prompt = device_code_prompt("https://example.com/device", "ABCD-EFGH");

    assert!(prompt.contains(
        "\x1b[90mContinue only if you started this login in Codex. If a website or another person gave you this code, cancel.\x1b[0m"
    ));
}

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
        headers.insert(name, http::HeaderValue::from_static(value));

        assert!(
            looks_like_cloudflare_challenge(StatusCode::FORBIDDEN, &headers, ""),
            "expected challenge header signal: {name}"
        );
    }
}

#[test]
fn cloudflare_challenge_detector_checks_all_set_cookie_headers() {
    let mut headers = HeaderMap::new();
    headers.append(
        "set-cookie",
        http::HeaderValue::from_static("session=abc123"),
    );
    headers.append(
        "set-cookie",
        http::HeaderValue::from_static("__cf_bm=abc123"),
    );

    assert!(looks_like_cloudflare_challenge(
        StatusCode::FORBIDDEN,
        &headers,
        "",
    ));
}

#[test]
fn cloudflare_challenge_detector_ignores_plain_cloudflare_proxy_headers() {
    let mut headers = HeaderMap::new();
    headers.insert("cf-ray", http::HeaderValue::from_static("abc123"));

    assert!(!looks_like_cloudflare_challenge(
        StatusCode::FORBIDDEN,
        &headers,
        "server returned forbidden",
    ));
}

#[test]
fn cloudflare_challenge_detector_ignores_non_forbidden_status() {
    let mut headers = HeaderMap::new();
    headers.insert("cf-ray", http::HeaderValue::from_static("abc123"));

    assert!(!looks_like_cloudflare_challenge(
        StatusCode::SERVICE_UNAVAILABLE,
        &headers,
        "cloudflare",
    ));
}
