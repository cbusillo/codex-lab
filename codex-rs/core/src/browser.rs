use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use codex_browser::BrowserManager;
use codex_browser::global;
use codex_code_bridge_protocol::MAX_SCREENSHOT_BYTES;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientFactory;
use codex_http_client::RouteAwareClientPool;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use url::Url;

pub(crate) const MAX_BROWSER_TEXT_BYTES: usize = 8 * 1024;
const MAX_BROWSER_JAVASCRIPT_BYTES: usize = 64 * 1024;
const DEFAULT_BROWSER_TIMEOUT_MS: u64 = 30_000;
const MAX_BROWSER_TIMEOUT_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum BrowserAction {
    Open,
    Close,
    Status,
    Click,
    Move,
    Type,
    Key,
    Javascript,
    Scroll,
    History,
    Inspect,
    Console,
    Cleanup,
    Cdp,
    Fetch,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BrowserArgs {
    pub(crate) action: BrowserAction,
    #[serde(default)]
    pub(crate) url: Option<String>,
    #[serde(default, rename = "type")]
    pub(crate) click_type: Option<String>,
    #[serde(default)]
    pub(crate) x: Option<f64>,
    #[serde(default)]
    pub(crate) y: Option<f64>,
    #[serde(default)]
    pub(crate) dx: Option<f64>,
    #[serde(default)]
    pub(crate) dy: Option<f64>,
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) key: Option<String>,
    #[serde(default)]
    pub(crate) code: Option<String>,
    #[serde(default)]
    pub(crate) direction: Option<String>,
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) lines: Option<usize>,
    #[serde(default)]
    pub(crate) method: Option<String>,
    #[serde(default)]
    pub(crate) params: Option<Value>,
    #[serde(default)]
    pub(crate) target: Option<String>,
    #[serde(default)]
    pub(crate) timeout_ms: Option<u64>,
    #[serde(default)]
    pub(crate) mode: Option<String>,
}

pub(crate) struct BrowserActionResult {
    pub(crate) response: Value,
    pub(crate) image: Option<BrowserImage>,
    pub(crate) success: bool,
}

pub(crate) struct BrowserImage {
    media_type: &'static str,
    data_base64: String,
}

impl BrowserImage {
    pub(crate) fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.data_base64)
    }
}

pub(crate) async fn execute_browser_action(
    args: BrowserArgs,
    http_client_factory: HttpClientFactory,
    full_cdp_access: bool,
) -> BrowserActionResult {
    let result = match args.action {
        BrowserAction::Open => open(args).await,
        BrowserAction::Close => close().await,
        BrowserAction::Status => status().await,
        BrowserAction::Click => click(args).await,
        BrowserAction::Move => move_cursor(args).await,
        BrowserAction::Type => type_text(args).await,
        BrowserAction::Key => press_key(args).await,
        BrowserAction::Javascript => javascript(args).await,
        BrowserAction::Scroll => scroll(args).await,
        BrowserAction::History => history(args).await,
        BrowserAction::Inspect => inspect(args).await,
        BrowserAction::Console => console(args).await,
        BrowserAction::Cleanup => cleanup().await,
        BrowserAction::Cdp => cdp(args, full_cdp_access).await,
        BrowserAction::Fetch => fetch(args, http_client_factory).await,
    };
    match result {
        Ok(result) => result,
        Err(message) => BrowserActionResult {
            response: json!({
                "status": "failed",
                "error": { "message": message },
            }),
            image: None,
            success: false,
        },
    }
}

async fn open(args: BrowserArgs) -> Result<BrowserActionResult, String> {
    let url = args.url.as_deref().unwrap_or("about:blank");
    validate_navigation_url(url)?;
    let timeout_duration = browser_timeout(args.timeout_ms)?;
    let manager = global::get_or_create_browser_manager().await;
    if !manager.is_enabled().await {
        timeout(timeout_duration, manager.set_enabled(true))
            .await
            .map_err(|_| "browser startup timed out".to_string())?
            .map_err(|err| format!("browser startup failed: {err}"))?;
    }
    let navigation = timeout(timeout_duration, manager.goto(url))
        .await
        .map_err(|_| "browser navigation timed out".to_string())?
        .map_err(|err| format!("browser navigation failed: {err}"))?;
    with_screenshot(
        &manager,
        json!({
            "status": "ok",
            "action": "open",
            "url": navigation.url,
            "title": navigation.title,
        }),
    )
    .await
}

async fn close() -> Result<BrowserActionResult, String> {
    let Some(manager) = global::get_browser_manager().await else {
        return Ok(text_result(json!({
            "status": "ok",
            "action": "close",
            "closed": false,
        })));
    };
    manager
        .set_enabled(false)
        .await
        .map_err(|err| format!("browser shutdown failed: {err}"))?;
    global::clear_browser_manager().await;
    Ok(text_result(json!({
        "status": "ok",
        "action": "close",
        "closed": true,
    })))
}

async fn status() -> Result<BrowserActionResult, String> {
    let Some(manager) = global::get_browser_manager().await else {
        return Ok(text_result(json!({
            "status": "available",
            "enabled": false,
            "browserActive": false,
        })));
    };
    let browser_status = manager.get_status().await;
    Ok(text_result(json!({
        "status": "available",
        "browser": browser_status,
        "browserType": manager.get_browser_type().await,
    })))
}

async fn click(args: BrowserArgs) -> Result<BrowserActionResult, String> {
    let manager = active_manager().await?;
    move_to_optional_coordinates(&manager, args.x, args.y).await?;
    let (x, y, event) = match args.click_type.as_deref().unwrap_or("click") {
        "click" => {
            let (x, y) = manager
                .click_at_current()
                .await
                .map_err(|err| format!("browser click failed: {err}"))?;
            (x, y, "click")
        }
        "mousedown" => {
            let (x, y) = manager
                .mouse_down_at_current()
                .await
                .map_err(|err| format!("browser mouse down failed: {err}"))?;
            (x, y, "mousedown")
        }
        "mouseup" => {
            let (x, y) = manager
                .mouse_up_at_current()
                .await
                .map_err(|err| format!("browser mouse up failed: {err}"))?;
            (x, y, "mouseup")
        }
        value => return Err(format!("unsupported click type `{value}`")),
    };
    with_screenshot(
        &manager,
        json!({
            "status": "ok",
            "action": "click",
            "event": event,
            "x": x,
            "y": y,
        }),
    )
    .await
}

async fn move_cursor(args: BrowserArgs) -> Result<BrowserActionResult, String> {
    let manager = active_manager().await?;
    let (x, y) = match (args.x, args.y, args.dx, args.dy) {
        (Some(x), Some(y), None, None) => {
            manager
                .move_mouse(x, y)
                .await
                .map_err(|err| format!("browser move failed: {err}"))?;
            (x, y)
        }
        (None, None, dx, dy) if dx.is_some() || dy.is_some() => manager
            .move_mouse_relative(dx.unwrap_or_default(), dy.unwrap_or_default())
            .await
            .map_err(|err| format!("browser move failed: {err}"))?,
        _ => {
            return Err("move requires both x and y, or one or both of dx and dy".to_string());
        }
    };
    with_screenshot(
        &manager,
        json!({
            "status": "ok",
            "action": "move",
            "x": x,
            "y": y,
        }),
    )
    .await
}

async fn type_text(args: BrowserArgs) -> Result<BrowserActionResult, String> {
    let text = required_nonempty("text", args.text)?;
    let manager = active_manager().await?;
    manager
        .type_text(&text)
        .await
        .map_err(|err| format!("browser typing failed: {err}"))?;
    with_screenshot(
        &manager,
        json!({
            "status": "ok",
            "action": "type",
            "characters": text.chars().count(),
        }),
    )
    .await
}

async fn press_key(args: BrowserArgs) -> Result<BrowserActionResult, String> {
    let key = required_nonempty("key", args.key)?;
    let manager = active_manager().await?;
    manager
        .press_key(&key)
        .await
        .map_err(|err| format!("browser key press failed: {err}"))?;
    with_screenshot(
        &manager,
        json!({
            "status": "ok",
            "action": "key",
            "key": key,
        }),
    )
    .await
}

async fn javascript(args: BrowserArgs) -> Result<BrowserActionResult, String> {
    let code = required_nonempty("code", args.code)?;
    if code.len() > MAX_BROWSER_JAVASCRIPT_BYTES {
        return Err(format!(
            "code must be at most {MAX_BROWSER_JAVASCRIPT_BYTES} bytes"
        ));
    }
    let manager = active_manager().await?;
    let result = manager
        .execute_javascript(&code)
        .await
        .map_err(|err| format!("browser JavaScript failed: {err}"))?;
    Ok(text_result(json!({
        "status": "ok",
        "action": "javascript",
        "result": bounded_json(result),
    })))
}

async fn scroll(args: BrowserArgs) -> Result<BrowserActionResult, String> {
    let dx = args.dx.unwrap_or_default();
    let dy = args.dy.unwrap_or_default();
    if dx == 0.0 && dy == 0.0 {
        return Err("scroll requires a non-zero dx or dy".to_string());
    }
    let manager = active_manager().await?;
    manager
        .scroll_by(dx, dy)
        .await
        .map_err(|err| format!("browser scroll failed: {err}"))?;
    with_screenshot(
        &manager,
        json!({
            "status": "ok",
            "action": "scroll",
            "dx": dx,
            "dy": dy,
        }),
    )
    .await
}

async fn history(args: BrowserArgs) -> Result<BrowserActionResult, String> {
    let direction = required_nonempty("direction", args.direction)?;
    let manager = active_manager().await?;
    match direction.as_str() {
        "back" => manager.history_back().await,
        "forward" => manager.history_forward().await,
        _ => return Err("direction must be `back` or `forward`".to_string()),
    }
    .map_err(|err| format!("browser history navigation failed: {err}"))?;
    with_screenshot(
        &manager,
        json!({
            "status": "ok",
            "action": "history",
            "direction": direction,
            "url": manager.get_current_url().await,
        }),
    )
    .await
}

async fn inspect(args: BrowserArgs) -> Result<BrowserActionResult, String> {
    let manager = active_manager().await?;
    let selector = match (args.id, args.x, args.y) {
        (Some(id), None, None) => {
            let id = serde_json::to_string(&id).map_err(|err| err.to_string())?;
            format!("document.getElementById({id})")
        }
        (None, Some(x), Some(y)) => format!("document.elementFromPoint({x}, {y})"),
        _ => return Err("inspect requires id, or both x and y".to_string()),
    };
    let code = format!(
        "(() => {{ const element = {selector}; if (!element) return null; const rect = element.getBoundingClientRect(); return {{ tagName: element.tagName, id: element.id || null, className: String(element.className || ''), text: String(element.innerText || element.textContent || '').slice(0, 4096), outerHTML: String(element.outerHTML || '').slice(0, 4096), rect: {{ x: rect.x, y: rect.y, width: rect.width, height: rect.height }} }}; }})()"
    );
    let result = manager
        .execute_javascript(&code)
        .await
        .map_err(|err| format!("browser inspect failed: {err}"))?;
    Ok(text_result(json!({
        "status": "ok",
        "action": "inspect",
        "element": bounded_json(result),
    })))
}

async fn console(args: BrowserArgs) -> Result<BrowserActionResult, String> {
    let manager = active_manager().await?;
    let lines = args.lines.map(|lines| lines.clamp(1, 1_000));
    let logs = manager
        .get_console_logs(lines)
        .await
        .map_err(|err| format!("browser console read failed: {err}"))?;
    Ok(text_result(json!({
        "status": "ok",
        "action": "console",
        "logs": bounded_json(logs),
    })))
}

async fn cleanup() -> Result<BrowserActionResult, String> {
    let Some(manager) = global::get_browser_manager().await else {
        return Ok(text_result(json!({
            "status": "ok",
            "action": "cleanup",
            "cleaned": false,
        })));
    };
    manager
        .cleanup()
        .await
        .map_err(|err| format!("browser cleanup failed: {err}"))?;
    Ok(text_result(json!({
        "status": "ok",
        "action": "cleanup",
        "cleaned": true,
    })))
}

async fn cdp(args: BrowserArgs, full_cdp_access: bool) -> Result<BrowserActionResult, String> {
    if !full_cdp_access {
        return Err("CDP access is disabled by browser-use policy".to_string());
    }
    let method = required_nonempty("method", args.method)?;
    let params = args.params.unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return Err("params must be an object".to_string());
    }
    let manager = active_manager().await?;
    let result = match args.target.as_deref().unwrap_or("page") {
        "page" => manager.execute_cdp(&method, params).await,
        "browser" => manager.execute_cdp_browser(&method, params).await,
        _ => return Err("target must be `page` or `browser`".to_string()),
    }
    .map_err(|err| format!("browser CDP command failed: {err}"))?;
    Ok(text_result(json!({
        "status": "ok",
        "action": "cdp",
        "result": bounded_json(result),
    })))
}

async fn fetch(
    args: BrowserArgs,
    http_client_factory: HttpClientFactory,
) -> Result<BrowserActionResult, String> {
    let url = required_nonempty("url", args.url)?;
    validate_fetch_url(&url)?;
    let timeout_duration = browser_timeout(args.timeout_ms)?;
    match args.mode.as_deref().unwrap_or("auto") {
        "http" => fetch_http(&url, timeout_duration, http_client_factory).await,
        "browser" => fetch_browser(&url, timeout_duration).await,
        "auto" => match fetch_http(&url, timeout_duration, http_client_factory).await {
            Ok(result) => Ok(result),
            Err(_) => fetch_browser(&url, timeout_duration).await,
        },
        _ => Err("mode must be `auto`, `browser`, or `http`".to_string()),
    }
}

async fn fetch_http(
    url: &str,
    timeout_duration: Duration,
    http_client_factory: HttpClientFactory,
) -> Result<BrowserActionResult, String> {
    let pool = RouteAwareClientPool::new_without_request_logging(
        http_client_factory,
        ClientRouteClass::Api,
    );
    let response = timeout(timeout_duration, pool.get(url).send())
        .await
        .map_err(|_| "browser fetch timed out".to_string())?
        .map_err(|err| format!("browser fetch failed: {err}"))?;
    let status = response.status();
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let (body, truncated) = timeout(timeout_duration, collect_bounded_body(response))
        .await
        .map_err(|_| "browser fetch body timed out".to_string())?
        .map_err(|err| format!("browser fetch body failed: {err}"))?;
    Ok(text_result(json!({
        "status": "ok",
        "action": "fetch",
        "mode": "http",
        "httpStatus": status.as_u16(),
        "url": final_url,
        "contentType": content_type,
        "body": body,
        "truncated": truncated,
    })))
}

async fn fetch_browser(
    url: &str,
    timeout_duration: Duration,
) -> Result<BrowserActionResult, String> {
    let manager = global::get_or_create_browser_manager().await;
    if !manager.is_enabled().await {
        timeout(timeout_duration, manager.set_enabled(true))
            .await
            .map_err(|_| "browser startup timed out".to_string())?
            .map_err(|err| format!("browser startup failed: {err}"))?;
    }
    timeout(timeout_duration, manager.goto(url))
        .await
        .map_err(|_| "browser navigation timed out".to_string())?
        .map_err(|err| format!("browser navigation failed: {err}"))?;
    let value = timeout(
        timeout_duration,
        manager.execute_javascript(
            "(() => ({ url: location.href, title: document.title, text: String(document.body?.innerText || document.documentElement?.innerText || '').slice(0, 8192) }))()",
        ),
    )
    .await
    .map_err(|_| "browser content extraction timed out".to_string())?
    .map_err(|err| format!("browser content extraction failed: {err}"))?;
    Ok(text_result(json!({
        "status": "ok",
        "action": "fetch",
        "mode": "browser",
        "document": bounded_json(value),
    })))
}

async fn active_manager() -> Result<Arc<BrowserManager>, String> {
    let manager = global::get_browser_manager()
        .await
        .ok_or_else(|| "browser is not initialized; use action=open first".to_string())?;
    if !manager.is_enabled().await {
        return Err("browser is not enabled; use action=open first".to_string());
    }
    Ok(manager)
}

async fn move_to_optional_coordinates(
    manager: &BrowserManager,
    x: Option<f64>,
    y: Option<f64>,
) -> Result<(), String> {
    if x.is_none() && y.is_none() {
        return Ok(());
    }
    let (current_x, current_y) = manager
        .get_cursor_position()
        .await
        .map_err(|err| format!("failed to read browser cursor position: {err}"))?;
    manager
        .move_mouse(x.unwrap_or(current_x), y.unwrap_or(current_y))
        .await
        .map_err(|err| format!("failed to move browser cursor: {err}"))
}

async fn with_screenshot(
    manager: &BrowserManager,
    mut response: Value,
) -> Result<BrowserActionResult, String> {
    let screenshot = manager.capture_screenshot_with_url().await;
    let (image, metadata) = match screenshot {
        Ok((paths, url)) => screenshot_image(paths.first().map(std::path::PathBuf::as_path))
            .await
            .map_or_else(
                |message| (None, json!({ "error": message, "url": url })),
                |image| {
                    let bytes = image
                        .as_ref()
                        .map(|image| image.data_base64.len())
                        .unwrap_or_default();
                    (image, json!({ "url": url, "base64Bytes": bytes }))
                },
            ),
        Err(err) => (
            None,
            json!({ "error": format!("browser screenshot failed: {err}") }),
        ),
    };
    if let Some(object) = response.as_object_mut() {
        object.insert("screenshot".to_string(), metadata);
    }
    Ok(BrowserActionResult {
        response,
        image,
        success: true,
    })
}

async fn screenshot_image(path: Option<&Path>) -> Result<Option<BrowserImage>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|err| format!("failed to inspect browser screenshot: {err}"))?;
    if metadata.len() > MAX_SCREENSHOT_BYTES as u64 {
        return Err(format!(
            "browser screenshot exceeded the {MAX_SCREENSHOT_BYTES} byte model-visible limit"
        ));
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|err| format!("failed to read browser screenshot: {err}"))?;
    let media_type = match path.extension().and_then(|value| value.to_str()) {
        Some("webp") => "image/webp",
        Some("jpg" | "jpeg") => "image/jpeg",
        _ => "image/png",
    };
    Ok(Some(BrowserImage {
        media_type,
        data_base64: BASE64_STANDARD.encode(bytes),
    }))
}

async fn collect_bounded_body(
    response: reqwest::Response,
) -> Result<(String, bool), reqwest::Error> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    let mut truncated = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let remaining = MAX_BROWSER_TEXT_BYTES.saturating_sub(body.len());
        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
        if body.len() == MAX_BROWSER_TEXT_BYTES {
            truncated = stream.next().await.transpose()?.is_some();
            break;
        }
    }
    Ok((String::from_utf8_lossy(&body).into_owned(), truncated))
}

fn validate_navigation_url(value: &str) -> Result<(), String> {
    if value == "about:blank" {
        return Ok(());
    }
    validate_fetch_url(value)
}

fn validate_fetch_url(value: &str) -> Result<(), String> {
    let url = Url::parse(value).map_err(|err| format!("invalid URL: {err}"))?;
    if matches!(url.scheme(), "http" | "https") {
        Ok(())
    } else {
        Err("URL scheme must be http or https".to_string())
    }
}

fn browser_timeout(timeout_ms: Option<u64>) -> Result<Duration, String> {
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_BROWSER_TIMEOUT_MS);
    if timeout_ms == 0 || timeout_ms > MAX_BROWSER_TIMEOUT_MS {
        return Err(format!(
            "timeout_ms must be between 1 and {MAX_BROWSER_TIMEOUT_MS}"
        ));
    }
    Ok(Duration::from_millis(timeout_ms))
}

fn required_nonempty(field_name: &str, value: Option<String>) -> Result<String, String> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field_name} is required and must not be empty"))
}

fn bounded_json(value: Value) -> Value {
    let Ok(serialized) = serde_json::to_vec(&value) else {
        return json!({
            "truncated": true,
            "message": "browser result could not be serialized",
        });
    };
    if serialized.len() <= MAX_BROWSER_TEXT_BYTES {
        value
    } else {
        json!({
            "truncated": true,
            "originalBytes": serialized.len(),
            "message": format!(
                "browser result exceeded the {MAX_BROWSER_TEXT_BYTES} byte model-visible limit"
            ),
        })
    }
}

fn text_result(response: Value) -> BrowserActionResult {
    BrowserActionResult {
        response,
        image: None,
        success: true,
    }
}

#[cfg(test)]
#[path = "browser_tests.rs"]
mod tests;
