use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use codex_exec_server::ExecServerError;
use codex_exec_server::HttpClient;
use codex_exec_server::HttpRequestParams;
use codex_exec_server::HttpRequestResponse;
use codex_exec_server::HttpResponseBodyStream;
use codex_exec_server::RouteAwareHttpClient;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use futures::future::BoxFuture;
use pretty_assertions::assert_eq;
use rmcp::transport::auth::OAuthHttpRedirectPolicy;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::MAX_OAUTH_HTTP_RESPONSE_BODY_BYTES;
use super::OAuthHttpClientAdapter;
use crate::http_client_adapter::StreamableHttpRedirectMode;
use crate::utils::MCP_USER_AGENT;
use crate::utils::build_default_headers;

#[derive(Clone)]
struct RecordingHttpClient {
    inner: RouteAwareHttpClient,
    timeout_ms: Arc<Mutex<Vec<Option<u64>>>>,
    first_request_delay: Duration,
    ignore_transport_timeout: bool,
}

impl HttpClient for RecordingHttpClient {
    fn http_request(
        &self,
        params: HttpRequestParams,
    ) -> BoxFuture<'_, Result<HttpRequestResponse, ExecServerError>> {
        self.inner.http_request(params)
    }

    fn http_request_stream(
        &self,
        mut params: HttpRequestParams,
    ) -> BoxFuture<'_, Result<(HttpRequestResponse, HttpResponseBodyStream), ExecServerError>> {
        let is_first_request = {
            let mut timeout_ms = self
                .timeout_ms
                .lock()
                .expect("timeout recorder lock should not be poisoned");
            timeout_ms.push(params.timeout_ms);
            timeout_ms.len() == 1
        };
        if self.ignore_transport_timeout {
            params.timeout_ms = None;
        }
        let inner = self.inner.clone();
        let first_request_delay = self.first_request_delay;
        Box::pin(async move {
            if is_first_request {
                tokio::time::sleep(first_request_delay).await;
            }
            inner.http_request_stream(params).await
        })
    }
}

#[tokio::test]
async fn oauth_registration_redirects_never_forward_resource_only_headers() -> Result<()> {
    const RESOURCE_API_KEY: &str = "resource-api-key-secret";
    const RESOURCE_USER_AGENT: &str = "resource-only-user-agent";

    for (redirect_mode, has_resource_only_headers) in [
        (StreamableHttpRedirectMode::Legacy, true),
        (StreamableHttpRedirectMode::AgentPluginV1, true),
        (StreamableHttpRedirectMode::Legacy, false),
    ] {
        let resource_server = MockServer::start().await;
        let redirect_target = MockServer::start().await;
        let resource_url = format!("{}/mcp", resource_server.uri());

        Mock::given(method("POST"))
            .and(path("/register"))
            .and(header("content-type", "application/json"))
            .and(header(
                "user-agent",
                if has_resource_only_headers {
                    RESOURCE_USER_AGENT
                } else {
                    MCP_USER_AGENT
                },
            ))
            .respond_with(ResponseTemplate::new(307).insert_header(
                "location",
                format!("{}/redirected-register", redirect_target.uri()),
            ))
            .expect(1)
            .mount(&resource_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/redirected-register"))
            .and(header("content-type", "application/json"))
            .and(header("user-agent", MCP_USER_AGENT))
            .respond_with(ResponseTemplate::new(201))
            .expect(u64::from(!has_resource_only_headers))
            .mount(&redirect_target)
            .await;

        let configured_headers = if has_resource_only_headers {
            HashMap::from([
                ("X-Api-Key".to_string(), RESOURCE_API_KEY.to_string()),
                ("User-Agent".to_string(), RESOURCE_USER_AGENT.to_string()),
            ])
        } else {
            HashMap::from([(
                "Content-Type".to_string(),
                "resource-only-content-type".to_string(),
            )])
        };
        let adapter = OAuthHttpClientAdapter::new_with_redirect_mode(
            Arc::new(RouteAwareHttpClient::new(HttpClientFactory::new(
                OutboundProxyPolicy::ReqwestDefault,
            ))),
            build_default_headers(Some(configured_headers), /*env_http_headers*/ None)?,
            &resource_url,
            /*has_configured_headers*/ true,
            redirect_mode,
        )?;
        let response = adapter
            .execute_request(
                oauth2::http::Request::builder()
                    .method("POST")
                    .uri(format!("{}/register", resource_server.uri()))
                    .header("content-type", "application/json")
                    .body(br#"{"client_name":"Codex"}"#.to_vec())?,
                OAuthHttpRedirectPolicy::Follow,
                /*timeout*/ None,
            )
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        assert_eq!(
            response.status(),
            if has_resource_only_headers {
                oauth2::http::StatusCode::TEMPORARY_REDIRECT
            } else {
                oauth2::http::StatusCode::CREATED
            }
        );
        resource_server.verify().await;
        redirect_target.verify().await;
    }

    Ok(())
}

#[tokio::test]
async fn same_origin_redirects_preserve_timeout_and_response_body_limits() -> Result<()> {
    let server = MockServer::start().await;
    let resource_url = format!("{}/mcp", server.uri());
    Mock::given(method("POST"))
        .and(path("/register"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", "/register/"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/register/"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;

    let timeout_ms = Arc::new(Mutex::new(Vec::new()));
    let request_timeout = Duration::from_secs(/*secs*/ 10);
    let first_request_delay = Duration::from_millis(/*millis*/ 100);
    let adapter = OAuthHttpClientAdapter::new(
        Arc::new(RecordingHttpClient {
            inner: RouteAwareHttpClient::new(HttpClientFactory::new(
                OutboundProxyPolicy::ReqwestDefault,
            )),
            timeout_ms: Arc::clone(&timeout_ms),
            first_request_delay,
            ignore_transport_timeout: false,
        }),
        build_default_headers(
            Some(HashMap::from([(
                "X-Api-Key".to_string(),
                "resource-api-key-secret".to_string(),
            )])),
            /*env_http_headers*/ None,
        )?,
        &resource_url,
    );
    let response = adapter
        .execute_request(
            oauth2::http::Request::builder()
                .method("POST")
                .uri(format!("{}/register", server.uri()))
                .body(Vec::new())?,
            OAuthHttpRedirectPolicy::Follow,
            Some(request_timeout),
        )
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
    assert_eq!(response.status(), oauth2::http::StatusCode::CREATED);
    let recorded_timeout_ms = timeout_ms
        .lock()
        .expect("timeout recorder lock should not be poisoned")
        .clone();
    let [Some(initial_timeout_ms), Some(redirect_timeout_ms)] = recorded_timeout_ms.as_slice()
    else {
        anyhow::bail!("expected timeout values for the initial and redirected requests");
    };
    assert_eq!(
        *initial_timeout_ms,
        u64::try_from(request_timeout.as_millis())?
    );
    let maximum_redirect_timeout_ms =
        initial_timeout_ms.saturating_sub(u64::try_from(first_request_delay.as_millis())?);
    assert!(*redirect_timeout_ms <= maximum_redirect_timeout_ms);
    assert!(*redirect_timeout_ms > initial_timeout_ms / 2);
    server.verify().await;

    let server = MockServer::start().await;
    let resource_url = format!("{}/mcp", server.uri());
    Mock::given(method("POST"))
        .and(path("/register"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", "/register/"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/register/"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&server)
        .await;

    let adapter = OAuthHttpClientAdapter::new(
        Arc::new(RecordingHttpClient {
            inner: RouteAwareHttpClient::new(HttpClientFactory::new(
                OutboundProxyPolicy::ReqwestDefault,
            )),
            timeout_ms: Arc::new(Mutex::new(Vec::new())),
            first_request_delay: Duration::from_millis(/*millis*/ 200),
            ignore_transport_timeout: true,
        }),
        build_default_headers(
            Some(HashMap::from([(
                "X-Api-Key".to_string(),
                "resource-api-key-secret".to_string(),
            )])),
            /*env_http_headers*/ None,
        )?,
        &resource_url,
    );
    let error = adapter
        .execute_request(
            oauth2::http::Request::builder()
                .method("POST")
                .uri(format!("{}/register", server.uri()))
                .body(Vec::new())?,
            OAuthHttpRedirectPolicy::Follow,
            Some(Duration::from_millis(/*millis*/ 100)),
        )
        .await
        .expect_err("expired redirect deadlines must fail before replay");
    assert!(error.to_string().contains("timed out"));
    server.verify().await;

    let server = MockServer::start().await;
    let resource_url = format!("{}/mcp", server.uri());
    Mock::given(method("POST"))
        .and(path("/register"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", "/register/")
                .set_body_bytes(vec![0; MAX_OAUTH_HTTP_RESPONSE_BODY_BYTES + 1]),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/register/"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&server)
        .await;

    let adapter = OAuthHttpClientAdapter::new(
        Arc::new(RouteAwareHttpClient::new(HttpClientFactory::new(
            OutboundProxyPolicy::ReqwestDefault,
        ))),
        build_default_headers(
            Some(HashMap::from([(
                "X-Api-Key".to_string(),
                "resource-api-key-secret".to_string(),
            )])),
            /*env_http_headers*/ None,
        )?,
        &resource_url,
    );
    let error = adapter
        .execute_request(
            oauth2::http::Request::builder()
                .method("POST")
                .uri(format!("{}/register", server.uri()))
                .body(Vec::new())?,
            OAuthHttpRedirectPolicy::Follow,
            /*timeout*/ None,
        )
        .await
        .expect_err("oversized redirect bodies must be rejected before replay");
    assert!(error.to_string().contains("exceeds"));
    server.verify().await;

    Ok(())
}
