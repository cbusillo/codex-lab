use crate::agent::external_diagnostics::ExternalAgentFailureDetail;
use crate::agent::external_diagnostics::ExternalAgentFailureKind;
use crate::agent::external_diagnostics::ExternalAgentProviderProvenance;
use crate::agent::external_diagnostics::permission_profile_is_read_only;
use crate::agent::external_preflight::preflight_external_agent_backend;
use crate::agent::role::DEFAULT_ROLE_NAME;
use crate::agent::role::resolve_role_config_owned;
use crate::config::AgentRoleBackendConfig;
use crate::config::Config;
use serde::Deserialize;
use serde::Serialize;

const CLAUDE_SELECTOR: &str = "claude-sonnet-4.6";
const ANTIGRAVITY_SELECTOR: &str = "antigravity";

const REVIEW_CANDIDATES: &[&str] = &[CLAUDE_SELECTOR, ANTIGRAVITY_SELECTOR];
const HIGH_RISK_CANDIDATES: &[&str] = &[ANTIGRAVITY_SELECTOR, CLAUDE_SELECTOR];

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentTaskKind {
    Implementation,
    Research,
    IndependentReview,
    Security,
    Infrastructure,
    Release,
    ProductRisk,
    #[default]
    Other,
}

impl AgentTaskKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Implementation => "implementation",
            Self::Research => "research",
            Self::IndependentReview => "independent_review",
            Self::Security => "security",
            Self::Infrastructure => "infrastructure",
            Self::Release => "release",
            Self::ProductRisk => "product_risk",
            Self::Other => "other",
        }
    }

    fn provider_candidates(self) -> &'static [&'static str] {
        match self {
            Self::IndependentReview => REVIEW_CANDIDATES,
            Self::Security | Self::Infrastructure | Self::Release | Self::ProductRisk => {
                HIGH_RISK_CANDIDATES
            }
            Self::Implementation | Self::Research | Self::Other => &[],
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentTaskSize {
    Tiny,
    #[default]
    Normal,
    Large,
}

impl AgentTaskSize {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Tiny => "tiny",
            Self::Normal => "normal",
            Self::Large => "large",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProviderRoutingKind {
    Explicit,
    AutomaticExternal,
    NativeDefault,
    NativeFallback,
}

impl ProviderRoutingKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::AutomaticExternal => "automatic_external",
            Self::NativeDefault => "native_default",
            Self::NativeFallback => "native_fallback",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ProviderRoutingSummary {
    pub(crate) kind: ProviderRoutingKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) requested: Option<String>,
    pub(crate) effective: String,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderRoutingDecision {
    agent_type: String,
    role_name: Option<String>,
    is_external: bool,
    provider: Option<ExternalAgentProviderProvenance>,
    summary: ProviderRoutingSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderRoutingFailure {
    agent_type: String,
    detail: ExternalAgentFailureDetail,
}

impl ProviderRoutingFailure {
    pub(crate) fn message(&self) -> String {
        let detail = self.detail.message.as_deref().unwrap_or("preflight failed");
        format!(
            "Explicit external agent `{}` failed `{}` preflight: {detail}",
            self.agent_type,
            self.detail.kind.as_str()
        )
    }
}

impl ProviderRoutingDecision {
    pub(crate) fn agent_type(&self) -> &str {
        &self.agent_type
    }

    pub(crate) fn role_name(&self) -> Option<&str> {
        self.role_name.as_deref()
    }

    pub(crate) fn is_external(&self) -> bool {
        self.is_external
    }

    pub(crate) fn provider(&self) -> Option<&ExternalAgentProviderProvenance> {
        self.provider.as_ref()
    }

    pub(crate) fn kind(&self) -> ProviderRoutingKind {
        self.summary.kind
    }

    pub(crate) fn summary(&self) -> ProviderRoutingSummary {
        self.summary.clone()
    }

    pub(crate) fn redacted_summary(&self) -> ProviderRoutingSummary {
        let reason = match self.summary.kind {
            ProviderRoutingKind::Explicit => "Explicit agent selection was preserved.".to_string(),
            ProviderRoutingKind::AutomaticExternal => {
                "Codex Lab selected an eligible external agent for this task.".to_string()
            }
            ProviderRoutingKind::NativeDefault => {
                "Codex Lab kept this task on the native default agent.".to_string()
            }
            ProviderRoutingKind::NativeFallback => {
                "No eligible external agent was available; Codex Lab used the native default agent."
                    .to_string()
            }
        };
        ProviderRoutingSummary {
            kind: self.summary.kind,
            requested: self.summary.requested.clone(),
            effective: self.summary.effective.clone(),
            reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProviderEligibility {
    Available(Option<ExternalAgentProviderProvenance>),
    Unavailable(String),
}

pub(crate) async fn select_provider_route(
    config: &Config,
    explicit_agent_type: Option<&str>,
    task_kind: AgentTaskKind,
    task_size: AgentTaskSize,
) -> Result<ProviderRoutingDecision, ProviderRoutingFailure> {
    let explicit_provider = if let Some(agent_type) = explicit_agent_type
        && role_uses_external_backend(config, agent_type)
    {
        Some(
            external_role_preflight(config, agent_type)
                .await
                .map_err(|detail| ProviderRoutingFailure {
                    agent_type: agent_type.to_string(),
                    detail,
                })?,
        )
    } else {
        None
    };

    let mut eligibility = std::collections::HashMap::new();
    if explicit_agent_type.is_none() && task_size != AgentTaskSize::Tiny {
        for candidate in task_kind.provider_candidates() {
            let result = external_role_preflight(config, candidate)
                .await
                .map(|provider| ProviderEligibility::Available(Some(provider)))
                .unwrap_or_else(|detail| {
                    let message = detail.message.as_deref().unwrap_or("preflight failed");
                    ProviderEligibility::Unavailable(format!("{}: {message}", detail.kind.as_str()))
                });
            eligibility.insert(*candidate, result);
        }
    }

    let mut decision = select_provider_route_with(
        explicit_agent_type,
        task_kind,
        task_size,
        |agent_type| role_uses_external_backend(config, agent_type),
        |agent_type| {
            eligibility.remove(agent_type).unwrap_or_else(|| {
                ProviderEligibility::Unavailable("preflight was not run".to_string())
            })
        },
    );
    if explicit_provider.is_some() {
        decision.provider = explicit_provider;
    }
    Ok(decision)
}

fn select_provider_route_with<IsExternal, Eligibility>(
    explicit_agent_type: Option<&str>,
    task_kind: AgentTaskKind,
    task_size: AgentTaskSize,
    is_external: IsExternal,
    mut eligibility: Eligibility,
) -> ProviderRoutingDecision
where
    IsExternal: Fn(&str) -> bool,
    Eligibility: FnMut(&str) -> ProviderEligibility,
{
    if let Some(agent_type) = explicit_agent_type {
        return ProviderRoutingDecision {
            agent_type: agent_type.to_string(),
            role_name: (agent_type != DEFAULT_ROLE_NAME).then(|| agent_type.to_string()),
            is_external: is_external(agent_type),
            provider: None,
            summary: ProviderRoutingSummary {
                kind: ProviderRoutingKind::Explicit,
                requested: Some(agent_type.to_string()),
                effective: agent_type.to_string(),
                reason: format!("`agent_type` explicitly selected `{agent_type}`."),
            },
        };
    }

    if task_size == AgentTaskSize::Tiny {
        return native_decision(
            ProviderRoutingKind::NativeDefault,
            format!(
                "Tiny `{}` task kept on the native default agent.",
                task_kind.as_str()
            ),
        );
    }

    let candidates = task_kind.provider_candidates();
    if candidates.is_empty() {
        return native_decision(
            ProviderRoutingKind::NativeDefault,
            format!(
                "`{}` tasks do not require provider-diverse routing.",
                task_kind.as_str()
            ),
        );
    }

    let mut unavailable = Vec::new();
    for candidate in candidates {
        match eligibility(candidate) {
            ProviderEligibility::Available(provider) => {
                return ProviderRoutingDecision {
                    agent_type: (*candidate).to_string(),
                    role_name: Some((*candidate).to_string()),
                    is_external: true,
                    provider,
                    summary: ProviderRoutingSummary {
                        kind: ProviderRoutingKind::AutomaticExternal,
                        requested: None,
                        effective: (*candidate).to_string(),
                        reason: format!(
                            "{} `{}` task selected eligible external agent `{candidate}`.",
                            task_size.as_str(),
                            task_kind.as_str()
                        ),
                    },
                };
            }
            ProviderEligibility::Unavailable(reason) => {
                unavailable.push(format!("`{candidate}`: {reason}"));
            }
        }
    }

    native_decision(
        ProviderRoutingKind::NativeFallback,
        format!(
            "No eligible external agent was found for {} `{}` work; using the native default agent. {}",
            task_size.as_str(),
            task_kind.as_str(),
            unavailable.join("; ")
        ),
    )
}

fn native_decision(kind: ProviderRoutingKind, reason: String) -> ProviderRoutingDecision {
    ProviderRoutingDecision {
        agent_type: DEFAULT_ROLE_NAME.to_string(),
        role_name: None,
        is_external: false,
        provider: None,
        summary: ProviderRoutingSummary {
            kind,
            requested: None,
            effective: DEFAULT_ROLE_NAME.to_string(),
            reason,
        },
    }
}

fn role_uses_external_backend(config: &Config, agent_type: &str) -> bool {
    resolve_role_config_owned(config, agent_type).is_some_and(|role| {
        matches!(
            role.backend,
            Some(AgentRoleBackendConfig::ExternalCommand(_))
        )
    })
}

async fn external_role_preflight(
    config: &Config,
    agent_type: &str,
) -> Result<ExternalAgentProviderProvenance, ExternalAgentFailureDetail> {
    let Some(role) = resolve_role_config_owned(config, agent_type) else {
        return Err(ExternalAgentFailureDetail::new(
            ExternalAgentFailureKind::LaunchFailed,
            "role is not configured",
        ));
    };
    let Some(AgentRoleBackendConfig::ExternalCommand(backend)) = role.backend else {
        return Err(ExternalAgentFailureDetail::new(
            ExternalAgentFailureKind::LaunchFailed,
            "role does not use an external command backend",
        ));
    };
    let is_read_only =
        permission_profile_is_read_only(&config.permissions.effective_permission_profile());
    preflight_external_agent_backend(
        Some(agent_type),
        &backend,
        config.cwd.as_path(),
        is_read_only,
    )
    .await
}

#[cfg(test)]
#[path = "provider_routing_tests.rs"]
mod tests;
