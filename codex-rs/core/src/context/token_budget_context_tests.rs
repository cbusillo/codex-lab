use codex_protocol::AgentPath;
use pretty_assertions::assert_eq;
use serde_json::json;
use uuid::Uuid;

use super::ContextualUserFragment;
use super::MAX_THREAD_HINT_BYTES;
use super::THREAD_HINT_TRUNCATION_NOTICE;
use super::TokenBudgetContext;
use super::join_thread_hint_content;

fn thread_hint_content(texts: &[String]) -> Vec<serde_json::Value> {
    texts
        .iter()
        .map(|text| json!({ "type": "text", "text": text }))
        .collect()
}

fn context_with_hint(hint: Option<String>) -> TokenBudgetContext {
    TokenBudgetContext::new(
        AgentPath::root(),
        Uuid::nil(),
        /*previous_window_id*/ None,
        Uuid::nil(),
        hint,
    )
}

#[test]
fn thread_hint_content_without_text_items_is_dropped() {
    assert_eq!(
        join_thread_hint_content(&[json!({ "type": "image" }), json!({ "text": "" })]),
        None
    );
}

#[test]
fn thread_hint_content_joins_text_items_with_newlines() {
    assert_eq!(
        join_thread_hint_content(&thread_hint_content(&[
            "first".to_string(),
            "second".to_string(),
        ])),
        Some("first\nsecond".to_string())
    );
}

#[test]
fn oversized_thread_hint_item_is_capped() {
    let hint = join_thread_hint_content(&thread_hint_content(&[
        "a".repeat(MAX_THREAD_HINT_BYTES * 4)
    ]))
    .expect("oversized hint should still produce bounded text");

    assert!(hint.len() <= MAX_THREAD_HINT_BYTES, "{} bytes", hint.len());
    assert!(hint.ends_with(THREAD_HINT_TRUNCATION_NOTICE));
}

#[test]
fn many_thread_hint_items_are_capped() {
    let texts = (0..10_000)
        .map(|index| format!("note-{index}"))
        .collect::<Vec<_>>();
    let hint = join_thread_hint_content(&thread_hint_content(&texts))
        .expect("high-cardinality hint should still produce bounded text");

    assert!(hint.len() <= MAX_THREAD_HINT_BYTES, "{} bytes", hint.len());
    assert!(hint.starts_with("note-0\nnote-1\n"));
}

#[test]
fn multibyte_thread_hint_is_capped_on_a_char_boundary() {
    // "🙂" is 4 bytes, so the cap lands mid-character unless boundaries are respected.
    let hint =
        join_thread_hint_content(&thread_hint_content(&["🙂".repeat(MAX_THREAD_HINT_BYTES)]))
            .expect("multibyte hint should still produce bounded text");

    assert!(hint.len() <= MAX_THREAD_HINT_BYTES, "{} bytes", hint.len());
    let kept = hint
        .strip_suffix(THREAD_HINT_TRUNCATION_NOTICE)
        .expect("multibyte hint should be truncated");
    assert!(kept.chars().all(|character| character == '🙂'));
}

#[test]
fn rendered_context_bounds_a_hint_supplied_directly_to_the_constructor() {
    let rendered = context_with_hint(Some("b".repeat(MAX_THREAD_HINT_BYTES * 4))).render();
    let baseline = context_with_hint(/*hint*/ None).render();

    assert!(
        rendered.len() <= baseline.len() + MAX_THREAD_HINT_BYTES + 1,
        "{} bytes",
        rendered.len()
    );
    assert!(rendered.contains(THREAD_HINT_TRUNCATION_NOTICE));
}
