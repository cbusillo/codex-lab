use super::super::PreviousSectionState;
use super::super::test_support::render_section_cases;
use super::*;

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
