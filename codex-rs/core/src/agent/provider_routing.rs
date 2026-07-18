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
    pub(crate) reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderRoutingDecision {
    agent_type: String,
    role_name: Option<String>,
    is_external: bool,
    summary: ProviderRoutingSummary,
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
            reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProviderEligibility {
    Available,
    Unavailable(String),
}

pub(crate) fn select_provider_route(
    config: &Config,
    explicit_agent_type: Option<&str>,
    task_kind: AgentTaskKind,
    task_size: AgentTaskSize,
) -> ProviderRoutingDecision {
    select_provider_route_with(
        explicit_agent_type,
        task_kind,
        task_size,
        |agent_type| role_uses_external_backend(config, agent_type),
        |agent_type| external_role_eligibility(config, agent_type),
    )
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
            summary: ProviderRoutingSummary {
                kind: ProviderRoutingKind::Explicit,
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
            ProviderEligibility::Available => {
                return ProviderRoutingDecision {
                    agent_type: (*candidate).to_string(),
                    role_name: Some((*candidate).to_string()),
                    is_external: true,
                    summary: ProviderRoutingSummary {
                        kind: ProviderRoutingKind::AutomaticExternal,
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
        summary: ProviderRoutingSummary { kind, reason },
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

fn external_role_eligibility(config: &Config, agent_type: &str) -> ProviderEligibility {
    let Some(role) = resolve_role_config_owned(config, agent_type) else {
        return ProviderEligibility::Unavailable("role is not configured".to_string());
    };
    let Some(AgentRoleBackendConfig::ExternalCommand(backend)) = role.backend else {
        return ProviderEligibility::Unavailable(
            "role does not use an external command backend".to_string(),
        );
    };
    let Some(command) =
        shlex::split(backend.command.trim()).and_then(|parts| parts.into_iter().next())
    else {
        return ProviderEligibility::Unavailable("command is empty or invalid".to_string());
    };
    if which::which(&command).is_ok() {
        ProviderEligibility::Available
    } else {
        ProviderEligibility::Unavailable(format!("command `{command}` was not found"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn route_with_available(
        explicit_agent_type: Option<&str>,
        task_kind: AgentTaskKind,
        task_size: AgentTaskSize,
        available: &[&str],
    ) -> ProviderRoutingDecision {
        let available = available.iter().copied().collect::<HashSet<_>>();
        select_provider_route_with(
            explicit_agent_type,
            task_kind,
            task_size,
            |agent_type| agent_type != DEFAULT_ROLE_NAME,
            |agent_type| {
                if available.contains(agent_type) {
                    ProviderEligibility::Available
                } else {
                    ProviderEligibility::Unavailable("not installed".to_string())
                }
            },
        )
    }

    #[test]
    fn independent_review_prefers_claude() {
        let decision = route_with_available(
            None,
            AgentTaskKind::IndependentReview,
            AgentTaskSize::Normal,
            &[CLAUDE_SELECTOR, ANTIGRAVITY_SELECTOR],
        );

        assert_eq!(decision.agent_type(), CLAUDE_SELECTOR);
        assert!(decision.is_external());
        assert_eq!(
            decision.summary.kind,
            ProviderRoutingKind::AutomaticExternal
        );
    }

    #[test]
    fn high_risk_work_prefers_antigravity() {
        let decision = route_with_available(
            None,
            AgentTaskKind::Security,
            AgentTaskSize::Large,
            &[CLAUDE_SELECTOR, ANTIGRAVITY_SELECTOR],
        );

        assert_eq!(decision.agent_type(), ANTIGRAVITY_SELECTOR);
        assert!(decision.is_external());
    }

    #[test]
    fn high_risk_work_uses_next_eligible_external_agent() {
        let decision = route_with_available(
            None,
            AgentTaskKind::Infrastructure,
            AgentTaskSize::Normal,
            &[CLAUDE_SELECTOR],
        );

        assert_eq!(decision.agent_type(), CLAUDE_SELECTOR);
        assert!(decision.summary.reason.contains("eligible external agent"));
    }

    #[test]
    fn tiny_high_risk_work_stays_native() {
        let decision = route_with_available(
            None,
            AgentTaskKind::Security,
            AgentTaskSize::Tiny,
            &[ANTIGRAVITY_SELECTOR],
        );

        assert_eq!(decision.agent_type(), DEFAULT_ROLE_NAME);
        assert_eq!(decision.summary.kind, ProviderRoutingKind::NativeDefault);
        assert!(!decision.is_external());
    }

    #[test]
    fn unavailable_external_agents_produce_attributable_native_fallback() {
        let decision =
            route_with_available(None, AgentTaskKind::ProductRisk, AgentTaskSize::Normal, &[]);

        assert_eq!(decision.agent_type(), DEFAULT_ROLE_NAME);
        assert_eq!(decision.summary.kind, ProviderRoutingKind::NativeFallback);
        assert!(
            decision
                .summary
                .reason
                .contains("`antigravity`: not installed")
        );
        assert!(
            decision
                .summary
                .reason
                .contains("`claude-sonnet-4.6`: not installed")
        );
    }

    #[test]
    fn explicit_agent_type_always_wins() {
        let decision = route_with_available(
            Some(DEFAULT_ROLE_NAME),
            AgentTaskKind::Release,
            AgentTaskSize::Large,
            &[ANTIGRAVITY_SELECTOR],
        );

        assert_eq!(decision.agent_type(), DEFAULT_ROLE_NAME);
        assert_eq!(decision.role_name(), None);
        assert_eq!(decision.summary.kind, ProviderRoutingKind::Explicit);
    }

    #[test]
    fn explicit_external_agent_type_always_wins() {
        let decision = route_with_available(
            Some(CLAUDE_SELECTOR),
            AgentTaskKind::Security,
            AgentTaskSize::Tiny,
            &[ANTIGRAVITY_SELECTOR],
        );

        assert_eq!(decision.agent_type(), CLAUDE_SELECTOR);
        assert_eq!(decision.role_name(), Some(CLAUDE_SELECTOR));
        assert!(decision.is_external());
        assert_eq!(decision.summary.kind, ProviderRoutingKind::Explicit);
    }

    #[test]
    fn redacted_summary_preserves_routing_kind_without_selector() {
        let decision = route_with_available(
            None,
            AgentTaskKind::IndependentReview,
            AgentTaskSize::Normal,
            &[CLAUDE_SELECTOR],
        );

        let summary = decision.redacted_summary();
        assert_eq!(summary.kind, ProviderRoutingKind::AutomaticExternal);
        assert!(!summary.reason.contains(CLAUDE_SELECTOR));
        assert!(summary.reason.contains("eligible external agent"));
    }
}
