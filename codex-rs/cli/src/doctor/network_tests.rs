use pretty_assertions::assert_eq;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;

use super::super::CheckStatus;
use super::super::HttpClientFactory;
use super::super::OutboundProxyPolicy;
use super::super::ProviderAuthReachabilityMode;
use super::super::provider_reachability_check;
use super::super::provider_reachability_plan_from_parts;
use codex_http_client::cache_direct_system_proxy_route_for_test;

#[tokio::test]
async fn provider_reachability_rejects_proxy_authentication_challenges() {
    let proxy = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(407))
        .mount(&proxy)
        .await;
    let mut plan = provider_reachability_plan_from_parts(
        ProviderAuthReachabilityMode::Chatgpt,
        "openai",
        "OpenAI",
        /*provider_base_url*/ None,
        /*provider_query_params*/ None,
        /*is_amazon_bedrock*/ false,
        &format!("{}/backend-api/", proxy.uri()),
    );
    for endpoint in &plan.endpoints {
        cache_direct_system_proxy_route_for_test(&endpoint.url);
    }
    plan.http_client_factory = HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy);

    let check = provider_reachability_check(plan).await;
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check
            .details
            .join(" ")
            .contains("proxy authentication required (HTTP 407)")
    );
}
