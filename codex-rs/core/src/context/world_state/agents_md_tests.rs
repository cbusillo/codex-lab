use super::super::PreviousSectionState;
use super::super::WorldState;
use super::super::test_support::render_section_cases;
use super::*;
use codex_utils_output_truncation::approx_token_count;
use pretty_assertions::assert_eq;

fn state_for_rendered_byte_count_with_markers(
    rendered_byte_count: usize,
    head: &str,
    tail: &str,
) -> AgentsMdState {
    let probe = LoadedAgentsMd::from_text_for_testing("x");
    let probe_state = AgentsMdState::new(Some(&probe));
    let envelope_byte_count = probe_state
        .render_diff(PreviousSectionState::Absent)
        .expect("probe instructions should render")
        .render()
        .len()
        .saturating_sub(1);
    let instruction_byte_count = rendered_byte_count.saturating_sub(envelope_byte_count);
    assert!(head.len().saturating_add(tail.len()) <= instruction_byte_count);
    let loaded = LoadedAgentsMd::from_text_for_testing(format!(
        "{head}{}{tail}",
        "x".repeat(
            instruction_byte_count
                .saturating_sub(head.len())
                .saturating_sub(tail.len())
        )
    ));
    AgentsMdState::new(Some(&loaded))
}

fn render_full(state: AgentsMdState) -> String {
    let mut world_state = WorldState::default();
    world_state.add_section(state);
    world_state
        .render_full()
        .into_iter()
        .next()
        .expect("AGENTS.md state should render")
        .render()
}

#[test]
fn snapshots() {
    use PreviousSectionState::Absent;
    use PreviousSectionState::Known;
    use PreviousSectionState::Unknown;

    let empty = AgentsMdState::default();
    let project_formatter = LoadedAgentsMd::from_text_for_testing("use the project formatter");
    let project_formatter = AgentsMdState::new(Some(&project_formatter));
    let old = LoadedAgentsMd::from_text_for_testing("old instructions");
    let old = AgentsMdState::new(Some(&old));
    let new = LoadedAgentsMd::from_text_for_testing("new instructions");
    let new = AgentsMdState::new(Some(&new));

    insta::assert_snapshot!(render_section_cases(&[
        (Absent, Absent),
        (Absent, Known(&empty)),
        (Absent, Known(&project_formatter)),
        (Known(&project_formatter), Known(&project_formatter)),
        (Known(&old), Known(&new)),
        (Known(&new), Known(&empty)),
        (Unknown, Known(&new)),
        (Unknown, Known(&empty)),
    ]));
}

#[test]
fn retained_matcher_recognizes_rendered_agents_md() {
    let loaded = LoadedAgentsMd::from_text_for_testing("use the project formatter");
    let state = AgentsMdState::new(Some(&loaded));
    let fragment = state
        .render_diff(PreviousSectionState::Absent)
        .expect("AGENTS.md state should render");

    assert!(AgentsMdState::has_retained_fragment_matcher());
    assert!(AgentsMdState::matches_retained_fragment(
        fragment.role(),
        &fragment.render()
    ));
}

#[test]
fn rendered_budget_preserves_exact_fit_and_structurally_truncates_over_cap() {
    let head = "agents prefix survives\n";
    let tail = "\nagents suffix survives";
    let exact_fit = render_full(state_for_rendered_byte_count_with_markers(
        AGENTS_MD_RENDERED_MAX_BYTES,
        head,
        tail,
    ));
    let over_cap = render_full(state_for_rendered_byte_count_with_markers(
        AGENTS_MD_RENDERED_MAX_BYTES + 1,
        head,
        tail,
    ));

    assert_eq!(exact_fit.len(), AGENTS_MD_RENDERED_MAX_BYTES);
    assert!(!exact_fit.contains("world-state content truncated"));
    assert!(exact_fit.contains(head));
    assert!(exact_fit.contains(tail));
    assert_eq!(over_cap.len(), AGENTS_MD_RENDERED_MAX_BYTES);
    assert!(over_cap.contains("<bounded_world_state_section "));
    assert!(over_cap.contains("world-state content truncated"));
    assert!(over_cap.contains(head));
    assert!(over_cap.contains(tail));
    assert!(over_cap.starts_with("# AGENTS.md instructions"));
    assert!(over_cap.ends_with("</INSTRUCTIONS>"));
}

#[test]
fn rendered_budget_stays_below_context_item_token_limit() {
    let estimated_tokens = approx_token_count(&"x".repeat(AGENTS_MD_RENDERED_MAX_BYTES));

    assert_eq!(estimated_tokens, 8_192);
    assert!(estimated_tokens <= 10_000);
}
