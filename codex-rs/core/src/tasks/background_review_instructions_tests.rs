use super::*;
use crate::context::ContextualUserFragment;
use pretty_assertions::assert_eq;

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
    let block = format!(
        "{}\nLATE_NESTED_RULE",
        "x".repeat(REVIEW_PART_BODY_BYTES - 1)
    );
    let parts = pack_parts(
        vec![source_block(1, "/repo", "file:///repo/AGENTS.md", &block)],
        &[],
    )
    .unwrap();
    assert_eq!(parts.len(), 2);
    assert!(parts[0].text.ends_with('\n'));
    assert!(parts[1].text.contains("LATE_NESTED_RULE"));
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
fn omission_reason_keeps_utf8_boundaries() {
    let path =
        codex_utils_path_uri::PathUri::parse(&format!("file:///{}", "界".repeat(300))).unwrap();
    let reason = omission_reason("too many sources", Some(&path), 1);
    assert!(reason.len() <= 512);
    assert!(std::str::from_utf8(reason.as_bytes()).is_ok());
}
