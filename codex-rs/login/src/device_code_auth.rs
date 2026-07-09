use codex_browser::BrowserConfig;
use codex_browser::BrowserManager;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::Deserialize;
use serde::Serialize;
use serde::de::Deserializer;
use serde::de::{self};
use std::time::Duration;
use std::time::Instant;

use crate::pkce::PkceCodes;
use crate::server::ServerOptions;
use codex_client::build_reqwest_client_with_custom_ca;
use std::io;

const ANSI_BLUE: &str = "\x1b[94m";
const ANSI_GRAY: &str = "\x1b[90m";
const ANSI_RESET: &str = "\x1b[0m";

#[derive(Debug, Clone)]
pub struct DeviceCode {
    pub verification_url: String,
    pub user_code: String,
    device_auth_id: String,
    interval: u64,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
struct UserCodeResp {
    device_auth_id: String,
    #[serde(alias = "user_code", alias = "usercode")]
    user_code: String,
    #[serde(default, deserialize_with = "deserialize_interval")]
    interval: u64,
}

#[derive(Serialize)]
struct UserCodeReq {
    client_id: String,
}

#[derive(Serialize)]
struct TokenPollReq {
    device_auth_id: String,
    user_code: String,
}

fn deserialize_interval<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    s.trim().parse::<u64>().map_err(de::Error::custom)
}

#[derive(Deserialize)]
struct CodeSuccessResp {
    authorization_code: String,
    code_challenge: String,
    code_verifier: String,
}

/// Request the user code and polling interval.
async fn request_user_code(
    client: &reqwest::Client,
    auth_base_url: &str,
    base_url: &str,
    client_id: &str,
) -> std::io::Result<UserCodeResp> {
    let url = format!("{auth_base_url}/deviceauth/usercode");
    let body = serde_json::to_string(&UserCodeReq {
        client_id: client_id.to_string(),
    })
    .map_err(std::io::Error::other)?;
    let resp = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(std::io::Error::other)?;

    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp.text().await.map_err(std::io::Error::other)?;

    if !status.is_success() {
        if status == StatusCode::NOT_FOUND {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "device code login is not enabled for this Codex server. Use the browser login or verify the server URL.",
            ));
        }

        if looks_like_cloudflare_challenge(status, &headers, &body) {
            return request_user_code_via_browser(base_url, client_id).await;
        }

        return Err(std::io::Error::other(format!(
            "device code request failed with status {status}"
        )));
    }

    serde_json::from_str(&body).map_err(std::io::Error::other)
}

fn looks_like_cloudflare_challenge(status: StatusCode, headers: &HeaderMap, body: &str) -> bool {
    if status != StatusCode::FORBIDDEN {
        return false;
    }

    if body_has_cloudflare_challenge_signal(body) {
        return true;
    }

    header_is_cloudflare_challenge(headers)
}

fn body_has_cloudflare_challenge_signal(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("_cf_chl_opt")
        || lower.contains("challenge-platform")
        || lower.contains("just a moment")
        || lower.contains("enable javascript and cookies")
        || (lower.contains("cloudflare") && lower.contains("challenge"))
}

fn header_is_cloudflare_challenge(headers: &HeaderMap) -> bool {
    headers
        .get("cf-mitigated")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("challenge"))
        || headers
            .get_all("server-timing")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|value| value.to_ascii_lowercase().contains("chlray"))
        || headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|cookie| cookie.contains("__cf_bm="))
}

async fn request_user_code_via_browser(
    base_url: &str,
    client_id: &str,
) -> std::io::Result<UserCodeResp> {
    let manager = BrowserManager::new(BrowserConfig {
        enabled: true,
        headless: true,
        ..Default::default()
    });
    let result = request_user_code_via_browser_with_manager(
        &manager,
        base_url,
        client_id,
        Duration::from_secs(4),
    )
    .await;
    let _ = manager.stop().await;
    result
}

async fn request_user_code_via_browser_with_manager(
    manager: &BrowserManager,
    base_url: &str,
    client_id: &str,
    settle_delay: Duration,
) -> std::io::Result<UserCodeResp> {
    let issuer = base_url.trim_end_matches('/');
    let authorize_page = format!("{issuer}/codex/device");

    tokio::time::timeout(Duration::from_secs(30), manager.goto(&authorize_page))
        .await
        .map_err(|_| std::io::Error::other("browser navigation timed out"))?
        .map_err(|err| std::io::Error::other(format!("browser navigation failed: {err}")))?;

    // Give browser-managed challenge scripts a bounded window to set cookies
    // before retrying the device-code API request from that browser context.
    tokio::time::sleep(settle_delay).await;

    let api_url = format!("{issuer}/api/accounts/deviceauth/usercode");
    let api_url_literal = serde_json::to_string(&api_url).map_err(std::io::Error::other)?;
    let payload_literal = serde_json::to_string(&serde_json::json!({ "client_id": client_id }))
        .map_err(std::io::Error::other)?;
    let script = format!(
        r#"(async () => {{
            try {{
                const resp = await fetch({api_url_literal}, {{
                    method: "POST",
                    credentials: "include",
                    headers: {{ "Content-Type": "application/json" }},
                    body: {payload_literal}
                }});
                const text = await resp.text();
                return {{ ok: resp.ok, status: resp.status, body: text }};
            }} catch (err) {{
                return {{ ok: false, status: 0, body: String(err) }};
            }}
        }})()"#
    );

    for _ in 0..3 {
        let value =
            tokio::time::timeout(Duration::from_secs(15), manager.execute_javascript(&script))
                .await
                .map_err(|_| std::io::Error::other("browser fetch timed out"))?
                .map_err(|err| std::io::Error::other(format!("browser execution failed: {err}")))?;

        let fetch_result = value.get("value").unwrap_or(&value);
        let status = fetch_result
            .get("status")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default();
        let ok = fetch_result
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let body = fetch_result
            .get("body")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        if ok {
            return serde_json::from_str(body).map_err(std::io::Error::other);
        }

        if status == i64::from(StatusCode::FORBIDDEN.as_u16()) {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        return Err(std::io::Error::other(format!(
            "device code request failed with status {status} while using browser fallback"
        )));
    }

    Err(std::io::Error::other(
        "device code request failed after browser fallback retries",
    ))
}

/// Poll token endpoint until a code is issued or timeout occurs.
async fn poll_for_token(
    client: &reqwest::Client,
    auth_base_url: &str,
    device_auth_id: &str,
    user_code: &str,
    interval: u64,
) -> std::io::Result<CodeSuccessResp> {
    let url = format!("{auth_base_url}/deviceauth/token");
    let max_wait = Duration::from_secs(15 * 60);
    let start = Instant::now();

    loop {
        let body = serde_json::to_string(&TokenPollReq {
            device_auth_id: device_auth_id.to_string(),
            user_code: user_code.to_string(),
        })
        .map_err(std::io::Error::other)?;
        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(std::io::Error::other)?;

        let status = resp.status();

        if status.is_success() {
            return resp.json().await.map_err(std::io::Error::other);
        }

        if status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND {
            if start.elapsed() >= max_wait {
                return Err(std::io::Error::other(
                    "device auth timed out after 15 minutes",
                ));
            }
            let sleep_for = Duration::from_secs(interval).min(max_wait - start.elapsed());
            tokio::time::sleep(sleep_for).await;
            continue;
        }

        return Err(std::io::Error::other(format!(
            "device auth failed with status {}",
            resp.status()
        )));
    }
}

fn print_device_code_prompt(verification_url: &str, code: &str) {
    let version = env!("CARGO_PKG_VERSION");
    println!(
        "\nWelcome to Codex [v{ANSI_GRAY}{version}{ANSI_RESET}]\n{ANSI_GRAY}OpenAI's command-line coding agent{ANSI_RESET}\n\
\nFollow these steps to sign in with ChatGPT using device code authorization:\n\
\n1. Open this link in your browser and sign in to your account\n   {ANSI_BLUE}{verification_url}{ANSI_RESET}\n\
\n2. Enter this one-time code {ANSI_GRAY}(expires in 15 minutes){ANSI_RESET}\n   {ANSI_BLUE}{code}{ANSI_RESET}\n\
\n{ANSI_GRAY}Device codes are a common phishing target. Never share this code.{ANSI_RESET}\n",
    );
}

pub async fn request_device_code(opts: &ServerOptions) -> std::io::Result<DeviceCode> {
    let client = build_reqwest_client_with_custom_ca(reqwest::Client::builder())?;
    let base_url = opts.issuer.trim_end_matches('/');
    let api_base_url = format!("{base_url}/api/accounts");
    let uc = request_user_code(&client, &api_base_url, base_url, &opts.client_id).await?;

    Ok(DeviceCode {
        verification_url: format!("{base_url}/codex/device"),
        user_code: uc.user_code,
        device_auth_id: uc.device_auth_id,
        interval: uc.interval,
    })
}

pub async fn complete_device_code_login(
    opts: ServerOptions,
    device_code: DeviceCode,
) -> std::io::Result<()> {
    let client = build_reqwest_client_with_custom_ca(reqwest::Client::builder())?;
    let base_url = opts.issuer.trim_end_matches('/');
    let api_base_url = format!("{base_url}/api/accounts");

    let code_resp = poll_for_token(
        &client,
        &api_base_url,
        &device_code.device_auth_id,
        &device_code.user_code,
        device_code.interval,
    )
    .await?;

    let pkce = PkceCodes {
        code_verifier: code_resp.code_verifier,
        code_challenge: code_resp.code_challenge,
    };
    let redirect_uri = format!("{base_url}/deviceauth/callback");

    let tokens = crate::server::exchange_code_for_tokens(
        base_url,
        &opts.client_id,
        &redirect_uri,
        &pkce,
        &code_resp.authorization_code,
    )
    .await
    .map_err(|err| std::io::Error::other(format!("device code exchange failed: {err}")))?;

    if let Err(message) = crate::server::ensure_workspace_allowed(
        opts.forced_chatgpt_workspace_id.as_deref(),
        &tokens.id_token,
    ) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, message));
    }

    crate::server::persist_tokens_async(
        &opts.codex_home,
        /*api_key*/ None,
        tokens.id_token,
        tokens.access_token,
        tokens.refresh_token,
        opts.cli_auth_credentials_store_mode,
        opts.previous_auth_handling,
    )
    .await
}

pub async fn run_device_code_login(opts: ServerOptions) -> std::io::Result<()> {
    let device_code = request_device_code(&opts).await?;
    print_device_code_prompt(&device_code.verification_url, &device_code.user_code);
    complete_device_code_login(opts, device_code).await
}

#[cfg(test)]
#[path = "device_code_auth_tests.rs"]
mod tests;
