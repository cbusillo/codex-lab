use super::super::PreviousSectionState;
use super::super::WorldState;
use super::super::test_support::render_section_cases;
use super::*;
use pretty_assertions::assert_eq;

const TEST_AGENTS_MD_SECTION_BYTES: usize = 2 * 1024;

fn state_for_rendered_byte_count(rendered_byte_count: usize) -> AgentsMdState {
    let probe = LoadedAgentsMd::from_text_for_testing("x");
    let probe_state = AgentsMdState::new(Some(&probe), TEST_AGENTS_MD_SECTION_BYTES);
    let envelope_byte_count = probe_state
        .render_diff(PreviousSectionState::Absent)
        .expect("probe instructions should render")
        .render()
        .len()
        .saturating_sub(1);
    let loaded = LoadedAgentsMd::from_text_for_testing(
        "x".repeat(rendered_byte_count.saturating_sub(envelope_byte_count)),
    );
    AgentsMdState::new(Some(&loaded), TEST_AGENTS_MD_SECTION_BYTES)
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
    let project_formatter = AgentsMdState::new(Some(&project_formatter), AGENTS_MD_MAX_BYTES);
    let old = LoadedAgentsMd::from_text_for_testing("old instructions");
    let old = AgentsMdState::new(Some(&old), AGENTS_MD_MAX_BYTES);
    let new = LoadedAgentsMd::from_text_for_testing("new instructions");
    let new = AgentsMdState::new(Some(&new), AGENTS_MD_MAX_BYTES);

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
    let state = AgentsMdState::new(Some(&loaded), AGENTS_MD_MAX_BYTES);
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
fn configured_budget_preserves_exact_fit_and_bounds_over_cap() {
    let exact_fit = render_full(state_for_rendered_byte_count(TEST_AGENTS_MD_SECTION_BYTES));
    let over_cap = render_full(state_for_rendered_byte_count(
        TEST_AGENTS_MD_SECTION_BYTES + 1,
    ));

    assert_eq!(exact_fit.len(), TEST_AGENTS_MD_SECTION_BYTES);
    assert!(!exact_fit.contains("world-state content truncated"));
    assert_eq!(over_cap.len(), TEST_AGENTS_MD_SECTION_BYTES);
    assert!(over_cap.contains("<bounded_world_state_section "));
    assert!(over_cap.contains("world-state content truncated"));
}

#[test]
fn configured_budget_is_clamped_to_the_dedicated_agents_md_cap() {
    let loaded = LoadedAgentsMd::from_text_for_testing("x".repeat(AGENTS_MD_MAX_BYTES * 2));
    let state = AgentsMdState::new(Some(&loaded), AGENTS_MD_MAX_BYTES * 2);

    assert_eq!(state.max_rendered_bytes(), AGENTS_MD_MAX_BYTES);
    let rendered = render_full(state);
    assert_eq!(rendered.len(), AGENTS_MD_MAX_BYTES);
    assert!(rendered.contains("world-state content truncated"));
}
