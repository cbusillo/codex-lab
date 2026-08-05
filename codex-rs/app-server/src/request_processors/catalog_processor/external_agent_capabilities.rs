use super::*;
use codex_app_server_protocol::ExternalAgentAuthState;
use codex_app_server_protocol::ExternalAgentCapabilitiesReadParams;
use codex_app_server_protocol::ExternalAgentCapabilitiesReadResponse;
use codex_app_server_protocol::ExternalAgentCapabilitiesRefreshCancelParams;
use codex_app_server_protocol::ExternalAgentCapabilitiesRefreshCancelResponse;
use codex_app_server_protocol::ExternalAgentCapabilitiesRefreshHandle;
use codex_app_server_protocol::ExternalAgentCapabilitiesRefreshParams;
use codex_app_server_protocol::ExternalAgentCapabilitiesRefreshStatus;
use codex_app_server_protocol::ExternalAgentCapabilitiesUpdatedNotification;
use codex_app_server_protocol::ExternalAgentCapabilityFailure;
use codex_app_server_protocol::ExternalAgentCapabilityFailureKind;
use codex_app_server_protocol::ExternalAgentCapabilityFreshness;
use codex_app_server_protocol::ExternalAgentCapabilitySource;
use codex_app_server_protocol::ExternalAgentInstallState;
use codex_app_server_protocol::ExternalAgentProviderCapabilities;
use codex_app_server_protocol::ExternalAgentSelectorState;
use codex_app_server_protocol::ExternalAgentSettingOrigin;
use codex_app_server_protocol::ServerNotification;
use codex_config::ConfigLayerMetadata;
use codex_config::ConfigLayerSource;
use futures::StreamExt;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const CAPABILITY_REFRESH_DEADLINE: Duration = Duration::from_secs(20);
const CAPABILITY_REFRESH_CONCURRENCY: usize = 4;
const MAX_CONCURRENT_CAPABILITY_REFRESHES: usize = 2;
const EXTERNAL_AGENT_FAMILIES: &[&str] = &["antigravity", "claude", "copilot", "qwen"];

#[derive(Clone)]
struct CapabilityContext {
    config: Arc<Config>,
    cwd: PathBuf,
    origins: HashMap<String, ConfigLayerMetadata>,
}

impl CapabilityContext {
    fn new(config: Config, cwd: PathBuf) -> Self {
        let origins = config.config_layer_stack.origins();
        Self {
            config: Arc::new(config),
            cwd,
            origins,
        }
    }

    fn setting_origin(&self, selector: &str, field: &str) -> Option<ExternalAgentSettingOrigin> {
        let lookup_path = format!("agents.selectors.{selector}.{field}");
        let metadata = self.origins.get(&lookup_path)?.clone();
        let read_only = !matches!(metadata.name, ConfigLayerSource::User { .. });
        Some(ExternalAgentSettingOrigin {
            config_path: format!("agents.selectors.\"{selector}\".{field}"),
            layer: crate::config_layer::config_layer_metadata_to_api(metadata),
            read_only,
        })
    }

    fn configured_selector_for_field(&self, selector: &str, field: &str) -> Option<String> {
        let has_field = |candidate: &str| {
            self.config
                .agent_selector_overrides
                .get(candidate)
                .is_some_and(|override_config| match field {
                    "enabled" => override_config.enabled.is_some(),
                    "model" => override_config.model.is_some(),
                    "effort" => override_config.effort.is_some(),
                    _ => false,
                })
        };
        if has_field(selector) {
            return Some(selector.to_string());
        }
        if selector.starts_with("antigravity-") && has_field("antigravity") {
            return Some("antigravity".to_string());
        }
        None
    }
}

impl CatalogRequestProcessor {
    pub(crate) async fn external_agent_capabilities_read(
        &self,
        params: ExternalAgentCapabilitiesReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let ExternalAgentCapabilitiesReadParams {
            cwd,
            families,
            refresh,
        } = params;
        let cwd = cwd
            .map(PathBuf::from)
            .unwrap_or_else(|| self.config.cwd.to_path_buf());
        let families = resolve_families(families.as_deref())?;
        let config = self.load_latest_config(Some(cwd.clone())).await?;
        let context = CapabilityContext::new(config, cwd);
        let providers = self.capability_snapshot(&context, &families);
        let refresh = match refresh {
            Some(refresh) => {
                self.arm_capability_refresh(context, families, refresh)
                    .await?
            }
            None => None,
        };
        Ok(Some(
            ExternalAgentCapabilitiesReadResponse { providers, refresh }.into(),
        ))
    }

    pub(crate) async fn external_agent_capabilities_refresh_cancel(
        &self,
        params: ExternalAgentCapabilitiesRefreshCancelParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let cancelled = self
            .active_capability_refreshes
            .lock()
            .await
            .remove(&params.refresh_id)
            .is_some_and(|token| {
                token.cancel();
                true
            });
        Ok(Some(
            ExternalAgentCapabilitiesRefreshCancelResponse { cancelled }.into(),
        ))
    }

    fn capability_snapshot(
        &self,
        context: &CapabilityContext,
        families: &[String],
    ) -> Vec<ExternalAgentProviderCapabilities> {
        families
            .iter()
            .filter_map(|family| self.capability_provider(context, family))
            .collect()
    }

    fn capability_provider(
        &self,
        context: &CapabilityContext,
        family: &str,
    ) -> Option<ExternalAgentProviderCapabilities> {
        let specs = codex_config::agent_defaults::agent_model_specs()
            .iter()
            .filter(|spec| spec.family == family)
            .collect::<Vec<_>>();
        let selector = specs.first()?.slug;
        let backend = codex_core::external_agent_backend_for_selector(&context.config, selector)?;
        let cached = codex_core::cached_external_agent_capabilities(&backend, &context.cwd);
        let advertised_selectors = cached
            .as_ref()
            .filter(|capabilities| !capabilities.models.is_empty())
            .map(|capabilities| {
                capabilities
                    .models
                    .iter()
                    .map(|model| model.selector.as_str())
                    .collect::<HashSet<_>>()
            });
        let mut selectors = specs
            .into_iter()
            .filter(|spec| {
                family == "antigravity"
                    || advertised_selectors
                        .as_ref()
                        .is_none_or(|advertised| advertised.contains(spec.slug))
            })
            .map(|spec| {
                self.selector_state(
                    context,
                    spec.slug,
                    spec.model_args.get(1).copied(),
                    spec.explicit_only,
                    /*discovered*/ false,
                )
            })
            .collect::<Vec<_>>();
        if family == "antigravity" {
            let mut seen = selectors
                .iter()
                .map(|selector| selector.selector.clone())
                .collect::<HashSet<_>>();
            selectors.extend(
                codex_core::discovered_antigravity_selectors(&backend, &context.cwd)
                    .into_iter()
                    .filter(|model| seen.insert(model.selector.clone()))
                    .map(|model| {
                        self.selector_state(
                            context,
                            &model.selector,
                            Some(&model.model),
                            model.explicit_only,
                            /*discovered*/ true,
                        )
                    }),
            );
        }
        let (
            install,
            auth,
            cli_version,
            supports_model_selection,
            supports_effort_selection,
            effort_levels,
            source,
            freshness,
            observed_at,
            failure,
        ) = cached.map_or_else(
            || {
                (
                    ExternalAgentInstallState::Unknown,
                    ExternalAgentAuthState::Unknown,
                    None,
                    false,
                    false,
                    Vec::new(),
                    ExternalAgentCapabilitySource::CacheMiss,
                    ExternalAgentCapabilityFreshness::Unknown,
                    None,
                    None,
                )
            },
            |capabilities| {
                let (install, auth) = capability_states(
                    capabilities.source,
                    capabilities.failure.as_ref().map(|detail| detail.kind),
                );
                (
                    install,
                    auth,
                    capabilities.cli_version,
                    capabilities.supports_model_selection,
                    capabilities.supports_effort_selection,
                    capabilities.effort_levels,
                    map_source(capabilities.source),
                    map_freshness(capabilities.freshness),
                    Some(i64::try_from(capabilities.observed_at_unix_seconds).unwrap_or(i64::MAX)),
                    capabilities.failure.as_ref().map(map_failure),
                )
            },
        );
        Some(ExternalAgentProviderCapabilities {
            family: family.to_string(),
            command: backend.command,
            install,
            auth,
            cli_version,
            supports_model_selection,
            supports_effort_selection,
            effort_levels,
            source,
            freshness,
            observed_at,
            failure,
            selectors,
        })
    }

    fn selector_state(
        &self,
        context: &CapabilityContext,
        selector: &str,
        model: Option<&str>,
        explicit_only: bool,
        discovered: bool,
    ) -> ExternalAgentSelectorState {
        let override_config = context.config.agent_selector_overrides.get(selector);
        let enabled_selector = context.configured_selector_for_field(selector, "enabled");
        let model_selector = context.configured_selector_for_field(selector, "model");
        let effort_selector = context.configured_selector_for_field(selector, "effort");
        ExternalAgentSelectorState {
            selector: selector.to_string(),
            discovered,
            model: model.map(str::to_string),
            explicit_only,
            enabled: codex_core::agent_selector_enabled(&context.config, selector),
            enabled_origin: enabled_selector
                .as_deref()
                .and_then(|selector| context.setting_origin(selector, "enabled")),
            default_model: override_config
                .and_then(|override_config| override_config.model.clone())
                .or_else(|| {
                    model_selector
                        .as_deref()
                        .filter(|configured| *configured != selector)
                        .and_then(|configured| {
                            context
                                .config
                                .agent_selector_overrides
                                .get(configured)
                                .and_then(|override_config| override_config.model.clone())
                        })
                }),
            default_model_origin: model_selector
                .as_deref()
                .and_then(|selector| context.setting_origin(selector, "model")),
            default_effort: override_config
                .and_then(|override_config| override_config.effort.clone())
                .or_else(|| {
                    effort_selector
                        .as_deref()
                        .filter(|configured| *configured != selector)
                        .and_then(|configured| {
                            context
                                .config
                                .agent_selector_overrides
                                .get(configured)
                                .and_then(|override_config| override_config.effort.clone())
                        })
                }),
            default_effort_origin: effort_selector
                .as_deref()
                .and_then(|selector| context.setting_origin(selector, "effort")),
        }
    }

    async fn arm_capability_refresh(
        &self,
        context: CapabilityContext,
        families: Vec<String>,
        refresh: ExternalAgentCapabilitiesRefreshParams,
    ) -> Result<Option<ExternalAgentCapabilitiesRefreshHandle>, JSONRPCErrorError> {
        let refresh_id = refresh.refresh_id.trim().to_string();
        if refresh_id.is_empty() || refresh_id.len() > 128 {
            return Err(invalid_request(
                "external-agent capability refresh id must contain 1 to 128 characters",
            ));
        }
        let token = Arc::new(CancellationToken::new());
        {
            let mut active = self.active_capability_refreshes.lock().await;
            active.retain(|_, token| !token.is_cancelled());
            if active.len() >= MAX_CONCURRENT_CAPABILITY_REFRESHES {
                return Ok(None);
            }
            if active.contains_key(&refresh_id) {
                return Ok(None);
            }
            active.insert(refresh_id.clone(), token.clone());
        }
        let processor = self.clone();
        let outgoing = self.outgoing.clone();
        let active = self.active_capability_refreshes.clone();
        let notification_id = refresh_id.clone();
        let notification_families = families.clone();
        let probe_families = families.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            let backends = processor
                .capability_snapshot(&context, &probe_families)
                .into_iter()
                .filter_map(|provider| {
                    provider.selectors.first().and_then(|selector| {
                        codex_core::external_agent_backend_for_selector(
                            &context.config,
                            &selector.selector,
                        )
                    })
                })
                .collect::<Vec<_>>();
            let probes = futures::stream::iter(backends)
                .map(|backend| {
                    let cwd = context.cwd.clone();
                    async move {
                        codex_core::refresh_external_agent_capabilities(&backend, &cwd).await;
                    }
                })
                .buffer_unordered(CAPABILITY_REFRESH_CONCURRENCY)
                .collect::<Vec<_>>();
            let status = tokio::select! {
                biased;
                result = tokio::time::timeout(CAPABILITY_REFRESH_DEADLINE, probes) => if result.is_ok() { ExternalAgentCapabilitiesRefreshStatus::Completed } else { ExternalAgentCapabilitiesRefreshStatus::DeadlineExceeded },
                () = token.cancelled() => ExternalAgentCapabilitiesRefreshStatus::Cancelled,
            };
            {
                let mut active = active.lock().await;
                if active
                    .get(&notification_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &token))
                {
                    active.remove(&notification_id);
                }
            }
            outgoing
                .send_server_notification(ServerNotification::ExternalAgentCapabilitiesUpdated(
                    ExternalAgentCapabilitiesUpdatedNotification {
                        refresh_id: notification_id,
                        status,
                        providers: processor.capability_snapshot(&context, &notification_families),
                    },
                ))
                .await;
        });
        Ok(Some(ExternalAgentCapabilitiesRefreshHandle {
            refresh_id,
            families,
            deadline_ms: CAPABILITY_REFRESH_DEADLINE.as_millis() as u64,
        }))
    }
}

fn resolve_families(
    requested_families: Option<&[String]>,
) -> Result<Vec<String>, JSONRPCErrorError> {
    let requested_families = requested_families.unwrap_or_default();
    if requested_families.is_empty() {
        return Ok(EXTERNAL_AGENT_FAMILIES
            .iter()
            .map(ToString::to_string)
            .collect());
    }
    let mut families = Vec::new();
    for family in requested_families {
        if !EXTERNAL_AGENT_FAMILIES.contains(&family.as_str()) {
            return Err(invalid_request(format!(
                "unknown external-agent family `{family}`"
            )));
        }
        if !families.contains(family) {
            families.push(family.clone());
        }
    }
    Ok(families)
}

fn capability_states(
    source: codex_core::ExternalAgentCapabilitySource,
    failure: Option<codex_core::ExternalAgentFailureKind>,
) -> (ExternalAgentInstallState, ExternalAgentAuthState) {
    let install = match failure {
        Some(codex_core::ExternalAgentFailureKind::CommandMissing) => {
            ExternalAgentInstallState::NotInstalled
        }
        Some(codex_core::ExternalAgentFailureKind::LaunchFailed) => {
            ExternalAgentInstallState::Unknown
        }
        _ => ExternalAgentInstallState::Installed,
    };
    let auth = match failure {
        Some(codex_core::ExternalAgentFailureKind::AuthenticationRequired) => {
            ExternalAgentAuthState::NotAuthenticated
        }
        Some(codex_core::ExternalAgentFailureKind::QuotaOrRateLimited) => {
            ExternalAgentAuthState::Authenticated
        }
        Some(_) => ExternalAgentAuthState::Unknown,
        None if matches!(source, codex_core::ExternalAgentCapabilitySource::NotProbed) => {
            ExternalAgentAuthState::NotProbed
        }
        None => ExternalAgentAuthState::Authenticated,
    };
    (install, auth)
}

fn map_source(source: codex_core::ExternalAgentCapabilitySource) -> ExternalAgentCapabilitySource {
    match source {
        codex_core::ExternalAgentCapabilitySource::StaticCatalog => {
            ExternalAgentCapabilitySource::StaticCatalog
        }
        codex_core::ExternalAgentCapabilitySource::LocalCli => {
            ExternalAgentCapabilitySource::LocalCli
        }
        codex_core::ExternalAgentCapabilitySource::NotProbed => {
            ExternalAgentCapabilitySource::NotProbed
        }
        codex_core::ExternalAgentCapabilitySource::ConservativeFallback => {
            ExternalAgentCapabilitySource::ConservativeFallback
        }
    }
}

fn map_freshness(
    freshness: codex_core::ExternalAgentCapabilityFreshness,
) -> ExternalAgentCapabilityFreshness {
    match freshness {
        codex_core::ExternalAgentCapabilityFreshness::Fresh => {
            ExternalAgentCapabilityFreshness::Fresh
        }
        codex_core::ExternalAgentCapabilityFreshness::Cached => {
            ExternalAgentCapabilityFreshness::Cached
        }
    }
}

fn map_failure(detail: &codex_core::ExternalAgentFailureDetail) -> ExternalAgentCapabilityFailure {
    let kind = match detail.kind {
        codex_core::ExternalAgentFailureKind::CommandMissing => {
            ExternalAgentCapabilityFailureKind::CommandMissing
        }
        codex_core::ExternalAgentFailureKind::AuthenticationRequired => {
            ExternalAgentCapabilityFailureKind::AuthenticationRequired
        }
        codex_core::ExternalAgentFailureKind::QuotaOrRateLimited => {
            ExternalAgentCapabilityFailureKind::QuotaOrRateLimited
        }
        codex_core::ExternalAgentFailureKind::UnsupportedMode => {
            ExternalAgentCapabilityFailureKind::UnsupportedMode
        }
        codex_core::ExternalAgentFailureKind::TimedOut => {
            ExternalAgentCapabilityFailureKind::TimedOut
        }
        codex_core::ExternalAgentFailureKind::MalformedOutput => {
            ExternalAgentCapabilityFailureKind::MalformedOutput
        }
        codex_core::ExternalAgentFailureKind::EmptyOutput => {
            ExternalAgentCapabilityFailureKind::EmptyOutput
        }
        codex_core::ExternalAgentFailureKind::LaunchFailed => {
            ExternalAgentCapabilityFailureKind::LaunchFailed
        }
        codex_core::ExternalAgentFailureKind::ProviderFailed => {
            ExternalAgentCapabilityFailureKind::ProviderFailed
        }
    };
    ExternalAgentCapabilityFailure {
        kind,
        message: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_core::ExternalAgentCapabilitySource as CoreCapabilitySource;
    use codex_core::ExternalAgentFailureKind as CoreFailureKind;

    #[test]
    fn provider_states_do_not_claim_authentication_after_probe_failures() {
        assert_eq!(
            capability_states(
                CoreCapabilitySource::ConservativeFallback,
                Some(CoreFailureKind::TimedOut),
            ),
            (
                ExternalAgentInstallState::Installed,
                ExternalAgentAuthState::Unknown,
            )
        );
        assert_eq!(
            capability_states(
                CoreCapabilitySource::ConservativeFallback,
                Some(CoreFailureKind::LaunchFailed),
            ),
            (
                ExternalAgentInstallState::Unknown,
                ExternalAgentAuthState::Unknown,
            )
        );
        assert_eq!(
            capability_states(
                CoreCapabilitySource::ConservativeFallback,
                Some(CoreFailureKind::CommandMissing),
            ),
            (
                ExternalAgentInstallState::NotInstalled,
                ExternalAgentAuthState::Unknown,
            )
        );
    }

    #[test]
    fn family_filter_rejects_unknown_values_and_deduplicates() {
        let families = resolve_families(Some(&[
            "claude".to_string(),
            "claude".to_string(),
            "antigravity".to_string(),
        ]))
        .expect("known families");
        assert_eq!(families, vec!["claude", "antigravity"]);
        assert!(resolve_families(Some(&["unknown".to_string()])).is_err());
    }
}
