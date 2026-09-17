use super::*;
use crate::context::ContextualUserFragment;
use crate::context::world_state::ReviewAgentsMdFragment;
use crate::context::world_state::ReviewAgentsMdPart;
use crate::context::world_state::WorldState;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use serde_json::json;

fn packed_parts(text: &str) -> Vec<ReviewAgentsMdSnapshot> {
    pack_parts(
        vec![source_block(1, "/repo", "file:///repo/AGENTS.md", text)],
        &[],
    )
    .expect("test instructions fit the fixed part capacity")
}

fn sized_text(unit: &str, desired_bytes: usize) -> String {
    let repetitions = desired_bytes / unit.len() + 1;
    let text = unit.repeat(repetitions);
    text[..desired_bytes.min(text.len())].to_string()
}

fn prompt_for_parts(parts: &[ReviewAgentsMdSnapshot]) -> Prompt {
    Prompt {
        input: parts
            .iter()
            .cloned()
            .map(|part| ReviewAgentsMdFragment(part).render_fragment().into())
            .collect(),
        ..Prompt::default()
    }
}

fn add_part(world_state: &mut WorldState, part: ReviewAgentsMdSnapshot) {
    match part.ordinal {
        0 => world_state.add_section(ReviewAgentsMdPart::<0>::new(part)),
        1 => world_state.add_section(ReviewAgentsMdPart::<1>::new(part)),
        2 => world_state.add_section(ReviewAgentsMdPart::<2>::new(part)),
        3 => world_state.add_section(ReviewAgentsMdPart::<3>::new(part)),
        4 => world_state.add_section(ReviewAgentsMdPart::<4>::new(part)),
        5 => world_state.add_section(ReviewAgentsMdPart::<5>::new(part)),
        6 => world_state.add_section(ReviewAgentsMdPart::<6>::new(part)),
        7 => world_state.add_section(ReviewAgentsMdPart::<7>::new(part)),
        _ => panic!("test part ordinal is outside the fixed capacity"),
    }
}

fn remove_content_item_kind(item: &mut ResponseItem) {
    if let ResponseItem::Message {
        internal_chat_message_metadata_passthrough,
        ..
    } = item
    {
        *internal_chat_message_metadata_passthrough = Some(
            codex_protocol::models::InternalChatMessageMetadataPassthrough {
                content_item_kinds: Some(vec![ContentItemKind("wrong.kind".to_string())]),
                ..Default::default()
            },
        );
    }
}

fn replace_text(item: &mut ResponseItem, replacement: &str) {
    if let ResponseItem::Message { content, .. } = item {
        content[0] = ContentItem::InputText {
            text: replacement.to_string(),
        };
    }
}

#[test]
fn packer_preserves_utf8_and_marks_every_part_with_one_digest() {
    let sources = vec![PreparedSource {
        path: "file:///repo/nested/AGENTS.md".to_string(),
        digest: digest("é"),
    }];
    let parts = pack_parts(
        vec![source_block(
            1,
            "/repo/nested",
            "file:///repo/nested/AGENTS.md",
            &"é".repeat(10_000),
        )],
        &sources,
    )
    .unwrap();
    assert!(parts.len() > 1);
    assert!(parts.len() <= REVIEW_AGENTS_MD_PARTS);
    assert_eq!(
        parts
            .iter()
            .flat_map(|part| part.text.chars())
            .filter(|character| *character == 'é')
            .count(),
        10_000
    );
    assert!(parts.iter().all(|part| {
        ReviewAgentsMdFragment(part.clone()).render().len() <= 9 * 1024
            && part.digest == parts[0].digest
            && part.total == parts.len()
    }));
}

#[test]
fn packer_prefers_newline_boundaries() {
    let block = format!("short line\n{}", "x".repeat(32));
    let pieces = split_utf8(&block, /*max_bytes*/ 32);
    assert_eq!(pieces, vec!["short line\n".to_string(), "x".repeat(32)]);
    assert_eq!(pieces.concat(), block);
}

#[test]
fn oversized_source_headers_refuse_without_stalling_utf8_splitting() {
    let result = pack_parts(
        vec![ReviewSourceBlock {
            header: "h".repeat(REVIEW_PART_BODY_BYTES),
            contents: "界".to_string(),
        }],
        &[],
    );
    assert!(result.is_err());
}

#[test]
fn packer_refuses_more_than_the_fixed_core_part_capacity() {
    let error = pack_parts(
        vec![source_block(
            1,
            "/repo",
            "file:///repo/AGENTS.md",
            &"x".repeat(REVIEW_PART_BODY_BYTES * (REVIEW_AGENTS_MD_PARTS + 1)),
        )],
        &[],
    )
    .unwrap_err();
    assert!(error.starts_with("complete instructions exceed the Background Review part capacity"));
}

#[test]
fn source_blocks_keep_scope_and_rank_visible() {
    let block = source_block(2, "/repo/a", "file:///repo/a/AGENTS.md", "RULE");
    assert_eq!(
        block.header,
        "source rank 2; applies to files under `/repo/a`; source `file:///repo/a/AGENTS.md`"
    );
    assert_eq!(block.contents, "RULE");
}

#[test]
fn empty_complete_instruction_set_needs_no_part_in_the_request() {
    assert_eq!(validate_review_parts(&[], &Prompt::default()), Ok(()));
}

#[test]
fn validator_rejects_missing_wrong_copied_and_extra_parts() {
    let parts = packed_parts("FIRST\nSECOND");
    assert!(!parts.is_empty());

    let mut missing = prompt_for_parts(&parts);
    missing.input.clear();
    assert!(validate_review_parts(&parts, &missing).is_err());

    let mut wrong_kind = prompt_for_parts(&parts);
    remove_content_item_kind(&mut wrong_kind.input[0]);
    assert!(validate_review_parts(&parts, &wrong_kind).is_err());

    let mut copied = prompt_for_parts(&parts);
    if let ResponseItem::Message {
        internal_chat_message_metadata_passthrough,
        ..
    } = &mut copied.input[0]
    {
        *internal_chat_message_metadata_passthrough = None;
    }
    assert!(validate_review_parts(&parts, &copied).is_err());

    let mut copied = prompt_for_parts(&parts);
    replace_text(&mut copied.input[0], "copied instruction text");
    assert!(validate_review_parts(&parts, &copied).is_err());

    let mut extra = prompt_for_parts(&parts);
    let extra_part = ReviewAgentsMdSnapshot {
        ordinal: parts.len(),
        total: parts.len() + 1,
        digest: parts[0].digest.clone(),
        text: "unexpected extra part".to_string(),
    };
    extra
        .input
        .push(ReviewAgentsMdFragment(extra_part).render_fragment().into());
    assert!(validate_review_parts(&parts, &extra).is_err());
}

#[test]
fn empty_complete_instruction_set_rejects_a_stray_trusted_part() {
    let stray = ReviewAgentsMdSnapshot {
        ordinal: 0,
        total: 1,
        digest: digest("stray"),
        text: "stray".to_string(),
    };
    let prompt = prompt_for_parts(&[stray]);
    assert!(validate_review_parts(&[], &prompt).is_err());
}

#[test]
fn validator_accepts_reordered_parts_but_not_conflicting_duplicates() {
    let parts = packed_parts(&sized_text("x\n", 24 * 1024));
    assert!(parts.len() > 1);
    let mut reordered = parts.clone();
    reordered.reverse();
    assert_eq!(
        validate_review_parts(&parts, &prompt_for_parts(&reordered)),
        Ok(())
    );

    let mut duplicate = prompt_for_parts(&parts);
    duplicate.input.push(duplicate.input[0].clone());
    assert_eq!(validate_review_parts(&parts, &duplicate), Ok(()));
    replace_text(&mut duplicate.input[parts.len()], "conflicting duplicate");
    assert!(validate_review_parts(&parts, &duplicate).is_err());
}

#[test]
fn world_state_pressure_does_not_silently_shorten_review_parts() {
    let parts = packed_parts(&sized_text("instruction\n", 24 * 1024));
    let mut world_state = WorldState::default();
    for part in parts.iter().cloned() {
        add_part(&mut world_state, part);
    }
    for id in [
        "pressure_0",
        "pressure_1",
        "pressure_2",
        "pressure_3",
        "pressure_4",
        "pressure_5",
        "pressure_6",
        "pressure_7",
    ] {
        world_state.add_extension_section(codex_extension_api::WorldStateSectionContribution::new(
            id,
            json!({"pressure": id}),
            move |_| {
                Some(codex_extension_api::RenderedWorldStateFragment::new(
                    "developer",
                    ("<pressure>", "</pressure>"),
                    "p".repeat(codex_extension_api::MAX_WORLD_STATE_SECTION_BYTES),
                ))
            },
        ));
    }
    let rendered = world_state.render_full();
    let prompt = Prompt {
        input: rendered
            .into_iter()
            .map(|fragment| fragment.into_boxed_response_item())
            .collect(),
        ..Prompt::default()
    };
    assert!(validate_review_parts(&parts, &prompt).is_err());
}

#[test]
fn history_reinjects_only_missing_review_part_and_accepts_out_of_order_history() {
    let parts = packed_parts(&sized_text("line\n", 24 * 1024));
    assert!(parts.len() > 1);
    let mut world_state = WorldState::default();
    for part in parts.iter().cloned() {
        add_part(&mut world_state, part);
    }
    let snapshot = world_state.snapshot();
    let missing = parts.last().expect("missing final part");
    let mut retained = world_state
        .render_full()
        .into_iter()
        .enumerate()
        .filter(|(ordinal, _)| *ordinal != missing.ordinal)
        .map(|(_, fragment)| fragment.into_boxed_response_item())
        .collect::<Vec<ResponseItem>>();
    retained.reverse();
    let rendered = world_state.render_history_diff(Some(&snapshot), &retained);
    assert_eq!(rendered.len(), 1);
    assert_eq!(
        rendered[0].render(),
        ReviewAgentsMdFragment(missing.clone()).render()
    );

    retained.push(
        world_state
            .render_full()
            .into_iter()
            .nth(missing.ordinal)
            .expect("missing part render")
            .into_boxed_response_item(),
    );
    assert!(
        world_state
            .render_history_diff(Some(&snapshot), &retained)
            .is_empty()
    );

    if let ResponseItem::Message {
        internal_chat_message_metadata_passthrough,
        ..
    } = retained.last_mut().unwrap()
    {
        *internal_chat_message_metadata_passthrough = None;
    }
    assert_eq!(
        world_state
            .render_history_diff(Some(&snapshot), &retained)
            .len(),
        1
    );
}

#[test]
fn omission_reason_keeps_utf8_boundaries() {
    let path =
        codex_utils_path_uri::PathUri::parse(&format!("file:///{}", "界".repeat(300))).unwrap();
    let reason = omission_reason("too many sources", Some(&path), 1);
    assert!(reason.len() <= 512);
    assert!(std::str::from_utf8(reason.as_bytes()).is_ok());
}

#[tokio::test]
async fn missing_review_scope_cannot_authorize_an_empty_instruction_set() {
    let (_session, turn) = crate::session::tests::make_session_and_context().await;
    let step = StepContext::for_test(Arc::new(turn));
    let gate = BackgroundReviewInstructionsGate::default();
    assert!(
        gate.authorize_request(&step, &Prompt::default())
            .await
            .is_err()
    );
    assert!(
        gate.failure()
            .unwrap()
            .contains("the exact changed paths are unavailable")
    );
}

#[tokio::test]
async fn frozen_review_instruction_sources_are_revalidated_before_request() {
    let repo = tempfile::tempdir().expect("create review repo");
    let agents = repo.path().join("AGENTS.md");
    let changed = repo.path().join("changed.rs");
    std::fs::write(&agents, "FROZEN_REVIEW_RULE").expect("write initial instructions");
    std::fs::write(&changed, "pub fn changed() {}").expect("write changed target");
    let cwd = codex_utils_absolute_path::AbsolutePathBuf::try_from(repo.path().to_path_buf())
        .expect("absolute review cwd");
    let changed = codex_utils_absolute_path::AbsolutePathBuf::try_from(changed)
        .expect("absolute changed target");
    let config_cwd = cwd.clone();
    let (_session, turn, _events) =
        crate::session::tests::make_session_and_context_with_auth_and_config_and_rx(
            codex_login::CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            move |config| config.cwd = config_cwd,
        )
        .await;
    let step = crate::session::step_context::StepContext::for_test(turn);
    let gate =
        BackgroundReviewInstructionsGate::new(vec![codex_utils_path_uri::PathUri::from_abs_path(
            &changed,
        )]);
    let parts = gate
        .prepared_parts(&step)
        .await
        .expect("freeze instructions");
    let prompt = prompt_for_parts(&parts);
    gate.authorize_request(&step, &prompt)
        .await
        .expect("unchanged frozen instructions authorize");

    std::fs::write(&agents, "MUTATED_REVIEW_RULE").expect("mutate instructions after freeze");
    assert!(gate.authorize_request(&step, &prompt).await.is_err());
    assert!(
        gate.failure()
            .expect("persistent failure")
            .contains("applicable instruction sources changed after review preparation")
    );
}
