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
                ProviderEligibility::Available(None)
            } else {
                ProviderEligibility::Unavailable("not installed".to_string())
            }
        },
    )
}

#[test]
fn independent_review_prefers_claude() {
    let decision = route_with_available(
        /*explicit_agent_type*/ None,
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
    assert_eq!(decision.summary.requested, None);
    assert_eq!(decision.summary.effective, CLAUDE_SELECTOR);
}

#[test]
fn high_risk_work_prefers_antigravity() {
    let decision = route_with_available(
        /*explicit_agent_type*/ None,
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
        /*explicit_agent_type*/ None,
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
        /*explicit_agent_type*/ None,
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
    let decision = route_with_available(
        /*explicit_agent_type*/ None,
        AgentTaskKind::ProductRisk,
        AgentTaskSize::Normal,
        &[],
    );

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
    assert_eq!(decision.summary.requested.as_deref(), Some(CLAUDE_SELECTOR));
    assert_eq!(decision.summary.effective, CLAUDE_SELECTOR);
}

#[test]
fn redacted_summary_preserves_routing_kind_and_effective_selector() {
    let decision = route_with_available(
        /*explicit_agent_type*/ None,
        AgentTaskKind::IndependentReview,
        AgentTaskSize::Normal,
        &[CLAUDE_SELECTOR],
    );

    let summary = decision.redacted_summary();
    assert_eq!(summary.kind, ProviderRoutingKind::AutomaticExternal);
    assert!(!summary.reason.contains(CLAUDE_SELECTOR));
    assert!(summary.reason.contains("eligible external agent"));
    assert_eq!(summary.requested, None);
    assert_eq!(summary.effective, CLAUDE_SELECTOR);
}
