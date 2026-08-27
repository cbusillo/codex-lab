use super::*;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

const OBSERVED_AGENTS_BYTES: usize = 26_473;

fn world_state(text: impl Into<String>) -> WorldState {
    let loaded = LoadedAgentsMd::from_text_for_testing(text);
    let mut world_state = WorldState::default();
    add_agents_md_sections(&mut world_state, Some(&loaded));
    world_state
}

fn rendered_texts(world_state: &WorldState) -> Vec<String> {
    world_state
        .render_full()
        .into_iter()
        .map(|fragment| {
            assert!(fragment.requires_separate_message());
            fragment.render()
        })
        .collect()
}

fn instruction_text(rendered: &str) -> &str {
    let start = rendered
        .find("<INSTRUCTIONS>\n")
        .expect("instruction start")
        + "<INSTRUCTIONS>\n".len();
    let end = rendered.rfind("\n</INSTRUCTIONS").expect("instruction end");
    &rendered[start..end]
}

fn reconstruct(rendered: &[String]) -> String {
    rendered.iter().map(|text| instruction_text(text)).collect()
}

fn snapshot_hashes(world_state: &WorldState) -> Vec<String> {
    let snapshot = serde_json::to_value(world_state.snapshot()).expect("serialize snapshot");
    let sections = snapshot.as_object().expect("world-state snapshot object");
    sections
        .values()
        .filter_map(|value| value["document_hash"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn observed_document_renders_as_ordered_standalone_bounded_shards() {
    let text = format!(
        "observed head\n{}\nobserved tail",
        "x".repeat(OBSERVED_AGENTS_BYTES - 28)
    );
    let rendered = rendered_texts(&world_state(&text));

    assert_eq!(rendered.len(), 4);
    assert_eq!(reconstruct(&rendered), text);
    assert!(
        rendered
            .iter()
            .all(|text| text.len() <= AGENTS_MD_SHARD_RENDERED_MAX_BYTES)
    );
    assert!(
        rendered
            .iter()
            .all(|text| !text.contains("world-state content truncated"))
    );
    assert!(rendered[0].starts_with("# AGENTS.md instructions\n"));
    for (index, text) in rendered.iter().enumerate().skip(1) {
        assert!(text.starts_with(&format!(
            "# AGENTS.md instructions (continuation part {} of 4)",
            index + 1
        )));
    }
}

#[test]
fn token_dense_and_multibyte_documents_split_on_valid_utf8_boundaries() {
    let cases = [
        "!@#$%^&*()_+-=[]{}|;:',.<>?/".repeat(900),
        "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo0123456789+/=".repeat(540),
        format!("{}終🙂{}", "a".repeat(8_050), "界🚀".repeat(3_000)),
    ];

    for text in cases {
        let rendered = rendered_texts(&world_state(&text));
        assert_eq!(reconstruct(&rendered), text);
        assert!(rendered.len() <= AGENTS_MD_SHARD_COUNT);
        assert!(
            rendered
                .iter()
                .all(|text| text.len() <= AGENTS_MD_SHARD_RENDERED_MAX_BYTES)
        );
    }
}

#[test]
fn shard_matchers_are_pairwise_unique() {
    let rendered = rendered_texts(&world_state("x".repeat(OBSERVED_AGENTS_BYTES)));

    for (candidate_index, candidate) in rendered.iter().enumerate() {
        for part in 1..=AGENTS_MD_SHARD_COUNT {
            assert_eq!(
                AgentsMdFragment::matches_part(part, candidate),
                part == candidate_index + 1,
                "part {part} matcher against candidate {}",
                candidate_index + 1
            );
        }
    }
}

#[test]
fn document_changes_replace_all_shards_atomically_once() {
    let original = "a".repeat(OBSERVED_AGENTS_BYTES);
    let previous = world_state(&original);
    let previous_snapshot = previous.snapshot();
    let previous_hashes = snapshot_hashes(&previous);

    for index in [0, original.len() / 2, original.len() - 1] {
        let mut changed = original.clone().into_bytes();
        changed[index] = b'b';
        let current = world_state(String::from_utf8(changed).expect("ASCII document"));
        let hashes = snapshot_hashes(&current);
        let rendered = current
            .render_diff(&previous_snapshot)
            .into_iter()
            .map(|fragment| fragment.render())
            .collect::<Vec<_>>();

        assert_eq!(rendered.len(), AGENTS_MD_SHARD_COUNT);
        assert_eq!(hashes.len(), AGENTS_MD_SHARD_COUNT);
        assert!(hashes.windows(2).all(|pair| pair[0] == pair[1]));
        assert_ne!(hashes[0], previous_hashes[0]);
        assert_eq!(
            rendered
                .iter()
                .filter(|text| text.contains(REPLACEMENT_NOTICE))
                .count(),
            1
        );
    }
}

#[test]
fn growth_shrink_removal_and_unchanged_state_manage_the_complete_shard_set() {
    let one = world_state("small instructions");
    assert_eq!(one.render_full().len(), 1);
    assert!(one.render_diff(&one.snapshot()).is_empty());

    let four = world_state("x".repeat(OBSERVED_AGENTS_BYTES));
    let growth = four.render_diff(&one.snapshot());
    assert_eq!(growth.len(), AGENTS_MD_SHARD_COUNT);

    let shrink_patch = one
        .snapshot()
        .merge_patch_from(&four.snapshot())
        .expect("shrink patch");
    assert_eq!(shrink_patch["agents_md_2"], Value::Null);
    assert_eq!(shrink_patch["agents_md_3"], Value::Null);
    assert_eq!(shrink_patch["agents_md_4"], Value::Null);
    assert_eq!(one.render_diff(&four.snapshot()).len(), 1);

    let mut removed = WorldState::default();
    add_agents_md_sections(&mut removed, None);
    let removal = removed.render_diff(&four.snapshot());
    assert_eq!(removal.len(), 1);
    assert!(removal[0].render().contains(REMOVAL_NOTICE));
    let removal_patch = removed
        .snapshot()
        .merge_patch_from(&four.snapshot())
        .expect("removal patch");
    assert_eq!(removal_patch["agents_md_2"], Value::Null);
    assert_eq!(removal_patch["agents_md_3"], Value::Null);
    assert_eq!(removal_patch["agents_md_4"], Value::Null);
}

#[test]
fn old_unsharded_snapshot_migrates_with_one_replacement_notice() {
    let legacy: AgentsMdSnapshot = serde_json::from_value(json!({
        "directory": "/old/project",
        "text": "old instructions"
    }))
    .expect("legacy snapshot should deserialize");

    let [part_1, part_2, part_3, part_4] = build_parts(Some(
        &LoadedAgentsMd::from_text_for_testing("x".repeat(OBSERVED_AGENTS_BYTES)),
    ));
    let primary = AgentsMdState::<1> { part: part_1 };
    let primary = primary
        .render_diff(PreviousSectionState::Known(&legacy))
        .expect("legacy state should be replaced")
        .render();
    assert!(primary.contains(REPLACEMENT_NOTICE));
    assert!(
        [part_2, part_3, part_4]
            .into_iter()
            .flatten()
            .all(|part| !part.text.contains(REPLACEMENT_NOTICE))
    );
}

#[test]
fn missing_retained_shard_is_rehydrated_without_matcher_aliasing() {
    let state = world_state("x".repeat(OBSERVED_AGENTS_BYTES));
    let snapshot = state.snapshot();
    let mut retained = state
        .render_full()
        .into_iter()
        .map(ContextualUserFragment::into_boxed_response_item)
        .collect::<Vec<ResponseItem>>();
    retained.remove(2);

    let rehydrated = state.render_history_diff(Some(&snapshot), &retained);
    assert_eq!(rehydrated.len(), 1);
    assert!(AgentsMdFragment::matches_part(3, &rehydrated[0].render()));
}
