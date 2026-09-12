use super::*;
use crate::context::ContextualUserFragment;
use crate::context::UserInstructions;
use crate::context::world_state::WorldState;
use pretty_assertions::assert_eq;

fn render_prompt(state: &WorldState) -> Prompt {
    Prompt {
        input: state
            .render_full()
            .into_iter()
            .map(|fragment| fragment.render_fragment().into())
            .collect(),
        ..Prompt::default()
    }
}

#[test]
fn canonical_byte_boundary_accepts_exact_fit_and_rejects_one_byte_over() {
    for alphabet in ["a", "é", "界"] {
        let empty = LoadedAgentsMd::from_text_for_testing("x");
        let state = AgentsMdState::new(Some(&empty));
        let overhead = state
            .render_diff(PreviousSectionState::Absent)
            .unwrap()
            .render()
            .len()
            - 1;
        let bytes = state.max_rendered_bytes() - overhead;
        let text = format!(
            "{}{}",
            alphabet.repeat(bytes / alphabet.len()),
            "a".repeat(bytes % alphabet.len())
        );
        for extra in ["", "a"] {
            let loaded = LoadedAgentsMd::from_text_for_testing(format!("{text}{extra}"));
            let mut world = WorldState::default();
            world.add_section(AgentsMdState::new(Some(&loaded)));
            let result = validate_rendered_instructions(Some(&loaded), &render_prompt(&world));
            assert_eq!(
                result,
                if extra.is_empty() {
                    Ok(())
                } else {
                    Err("the complete instruction fragment exceeds its context limit")
                }
            );
        }
    }
}

#[test]
fn copied_instruction_text_in_user_input_does_not_prove_context_delivery() {
    let loaded = LoadedAgentsMd::from_text_for_testing("Observe the final nested rule.");
    let state = AgentsMdState::new(Some(&loaded));
    let mut copied: ResponseItem = state
        .render_diff(PreviousSectionState::Absent)
        .unwrap()
        .render_fragment()
        .into();
    if let ResponseItem::Message {
        internal_chat_message_metadata_passthrough,
        ..
    } = &mut copied
    {
        *internal_chat_message_metadata_passthrough = None;
    }
    let prompt = Prompt {
        input: vec![copied],
        ..Prompt::default()
    };
    assert_eq!(
        validate_rendered_instructions(Some(&loaded), &prompt),
        Err("the complete instruction fragment is absent from the final request")
    );
}

#[test]
fn replacement_must_match_the_current_snapshot_without_truncation() {
    let loaded = LoadedAgentsMd::from_text_for_testing("Apply the replacement rule.");
    let state = AgentsMdState::new(Some(&loaded));
    let prompt = Prompt {
        input: vec![
            state
                .render_diff(PreviousSectionState::Unknown)
                .unwrap()
                .render_fragment()
                .into(),
        ],
        ..Prompt::default()
    };
    assert_eq!(
        validate_rendered_instructions(Some(&loaded), &prompt),
        Ok(())
    );
    let changed = LoadedAgentsMd::from_text_for_testing("Apply a newer rule.");
    assert_eq!(
        validate_rendered_instructions(Some(&changed), &prompt),
        Err("the complete instruction fragment is absent from the final request")
    );
}

struct PressureSection<const N: usize>;

impl<const N: usize> WorldStateSection for PressureSection<N> {
    const ID: &'static str = [
        "pressure0",
        "pressure1",
        "pressure2",
        "pressure3",
        "pressure4",
        "pressure5",
        "pressure6",
        "pressure7",
    ][N];
    type Snapshot = String;

    fn snapshot(&self) -> String {
        "pressure".repeat(1_200)
    }

    fn render_diff(
        &self,
        _previous: PreviousSectionState<'_, String>,
    ) -> Option<Box<dyn ContextualUserFragment>> {
        Some(Box::new(UserInstructions {
            directory: None,
            text: self.snapshot(),
        }))
    }
}

#[test]
fn aggregate_world_state_budget_cannot_silently_shorten_review_instructions() {
    let loaded = LoadedAgentsMd::from_text_for_testing("rule ".repeat(1_700));
    let mut world = WorldState::default();
    world.add_section(AgentsMdState::new(Some(&loaded)));
    world.add_section(PressureSection::<0>);
    world.add_section(PressureSection::<1>);
    world.add_section(PressureSection::<2>);
    world.add_section(PressureSection::<3>);
    world.add_section(PressureSection::<4>);
    world.add_section(PressureSection::<5>);
    world.add_section(PressureSection::<6>);
    world.add_section(PressureSection::<7>);
    assert_eq!(
        validate_rendered_instructions(Some(&loaded), &render_prompt(&world)),
        Err("the complete instruction fragment is absent from the final request")
    );
}
