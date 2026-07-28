use super::*;
use pretty_assertions::assert_eq;

#[test]
fn validates_browser_urls() {
    assert_eq!(validate_navigation_url("about:blank"), Ok(()));
    assert_eq!(validate_fetch_url("https://example.com/path"), Ok(()));
    assert_eq!(
        validate_fetch_url("file:///tmp/private"),
        Err("URL scheme must be http or https".to_string())
    );
}

#[test]
fn validates_browser_timeout_bounds() {
    assert_eq!(
        browser_timeout(Some(0)),
        Err(format!(
            "timeout_ms must be between 1 and {MAX_BROWSER_TIMEOUT_MS}"
        ))
    );
    assert_eq!(
        browser_timeout(Some(MAX_BROWSER_TIMEOUT_MS + 1)),
        Err(format!(
            "timeout_ms must be between 1 and {MAX_BROWSER_TIMEOUT_MS}"
        ))
    );
    assert_eq!(
        browser_timeout(Some(1)).expect("valid timeout"),
        Duration::from_millis(1)
    );
}

#[test]
fn bounds_model_visible_json_results() {
    let value = json!({ "content": "x".repeat(MAX_BROWSER_TEXT_BYTES) });

    assert_eq!(bounded_json(value)["truncated"], true);
}
