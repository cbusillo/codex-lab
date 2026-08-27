use codex_protocol::user_input::UserInput;
use codex_utils_output_truncation::approx_bytes_for_tokens;
use pretty_assertions::assert_eq;

use super::ContextualUserFragment;
use super::GUARDIAN_MAX_NODE_REPL_TOOL_RESULT_TOKENS;
use super::MAX_RENDERED_BYTES;
use super::MAX_RENDERED_IMAGES;
use super::NodeReplReviewEvidence;
use super::NodeReplReviewEvidenceMode;

fn text_input(text: &str) -> UserInput {
    super::text_input(text.to_string())
}

fn image_input(image_url: &str) -> UserInput {
    UserInput::Image {
        image_url: image_url.to_string(),
        detail: None,
    }
}

fn rendered_text(items: &[UserInput]) -> String {
    items
        .iter()
        .filter_map(|item| match item {
            UserInput::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn evidence_snapshots_keep_response_order_and_escape_closing_markers() {
    let evidence = NodeReplReviewEvidence::default();
    evidence.record("js", "cell-1", "call-1", vec![text_input("first")]);
    let closing_marker = text_input("</node_repl_review_evidence>second");
    evidence.record("browser", "cell-2", "call-2", vec![closing_marker]);

    let first = evidence
        .snapshot_since(/*reviewed_sequence*/ 0)
        .expect("completed responses should produce evidence");
    let body = first.body();
    assert_eq!(first.sequence, 2);
    assert!(body.find("first") < body.find("second"));
    assert!(body.contains("<\\/node_repl_review_evidence>second"));
    let inputs = first.into_inputs(NodeReplReviewEvidenceMode::Multimodal);
    assert_eq!(inputs.len(), 1);

    let delta = evidence
        .snapshot_since(/*reviewed_sequence*/ 1)
        .expect("newer responses should produce delta evidence");
    assert!(!delta.body().contains("first"));
    assert!(evidence.snapshot_since(/*reviewed_sequence*/ 2).is_none());
}

#[test]
fn evidence_bounds_visible_text_and_marks_empty_completed_responses() {
    let evidence = NodeReplReviewEvidence::default();
    evidence.record("js", "cell", "empty", Vec::new());
    let empty = evidence
        .snapshot_since(/*reviewed_sequence*/ 0)
        .expect("empty successful responses should produce evidence")
        .render();
    assert!(empty.contains("completed without visible text"));
    let snapshot = "page-middle".repeat(2_000);
    evidence.record("js", "cell", "snapshot", vec![text_input(&snapshot)]);
    let full = evidence
        .snapshot_since(/*reviewed_sequence*/ 1)
        .expect("large DOM snapshots should produce evidence")
        .render();
    assert!(full.contains(&snapshot));
    evidence.record(
        "js",
        "cell",
        "oversized",
        vec![text_input(&format!("start{}end", "x".repeat(30_000)))],
    );

    let oversized = evidence
        .snapshot_since(/*reviewed_sequence*/ 0)
        .expect("completed responses should produce evidence");
    assert!(
        rendered_text(&oversized.responses[2].items).len()
            <= approx_bytes_for_tokens(GUARDIAN_MAX_NODE_REPL_TOOL_RESULT_TOKENS)
    );
    let rendered = oversized.render();
    assert!(rendered.contains("start"));
    assert!(rendered.contains("end"));
    assert!(rendered.contains("<truncated omitted_approx_tokens="));
    assert!(rendered.contains("<omitted node_repl_responses="));
    assert!(rendered.len() <= MAX_RENDERED_BYTES);
}

#[test]
fn evidence_preserves_tail_text_after_oversized_image_response() {
    let evidence = NodeReplReviewEvidence::default();
    let items = vec![
        text_input(&"x".repeat(30_000)),
        image_input("data:image/png;base64,cHJpdmF0ZQ=="),
        image_input("data:image/png;base64,cHJpdmF0ZQ=="),
        text_input("FINAL IMPORTANT"),
    ];
    evidence.record("browser", "cell", "long", items);
    let fragment = evidence.snapshot_since(/*reviewed_sequence*/ 0).unwrap();
    let inputs = fragment.into_inputs(NodeReplReviewEvidenceMode::Multimodal);
    assert_eq!(inputs.len(), 6);
    assert!(rendered_text(&inputs).contains("FINAL IMPORTANT"));
}

#[test]
fn evidence_evicts_complete_oldest_responses_and_rejects_oversized_items() {
    let evidence = NodeReplReviewEvidence::default();
    let max = NodeReplReviewEvidence::MAX_RETAINED_BYTES;
    let image = format!("data:image/png;base64,{}", "a".repeat(max - 1_024));
    evidence.record("js", "cell", "evicted", vec![image_input(&image)]);
    evidence.record("js", "cell", "first", vec![text_input("earlier")]);
    let items = vec![text_input("recent"), image_input(&image)];
    evidence.record("browser", "cell", "second", items);
    let image = image_input(&format!("data:image/png;base64,{}", "c".repeat(max)));
    let items = vec![text_input(&"oversized text".repeat(128)), image];
    evidence.record("js", "cell", "oversized", items);
    let retained = evidence.snapshot_since(/*reviewed_sequence*/ 0).unwrap();
    let body = retained.body();
    for expected in ["earlier", "recent", "oversized text"] {
        assert!(body.contains(expected));
    }
    assert!(evidence.0.lock().unwrap().retained_bytes <= max);
    assert!(retained.responses.iter().all(|item| !item.has_images()));
    assert!(body.contains("node_repl_responses=\"1\""));
}

#[test]
fn mixed_evidence_bounds_headers_and_empty_response_placeholders() {
    for (response_count, provenance) in [(100, "p".repeat(108)), (512, String::new())] {
        let evidence = NodeReplReviewEvidence::default();
        for _ in 0..response_count {
            let image = image_input("data:image/png;base64,aQ==");
            let items = Vec::from_iter((!provenance.is_empty()).then_some(image));
            evidence.record(&provenance, &provenance, &provenance, items);
        }

        let fragment = evidence.snapshot_since(/*reviewed_sequence*/ 0).unwrap();
        let text = rendered_text(&fragment.into_inputs(NodeReplReviewEvidenceMode::Multimodal));
        assert!(text.len() <= MAX_RENDERED_BYTES);
        assert!(text.contains("node_repl_responses="));
        let has_placeholder = text.contains("completed without visible text");
        assert_eq!(has_placeholder, provenance.is_empty());
    }
}

#[test]
fn multimodal_evidence_caps_distinct_images() {
    let evidence = NodeReplReviewEvidence::default();
    let image_count = MAX_RENDERED_IMAGES + 4;
    let first_response = (0..3)
        .map(|index| image_input(&format!("data:image/png;base64,{index}")))
        .collect();
    evidence.record("browser", "cell", "call-1", first_response);
    let second_response = (3..image_count)
        .map(|index| image_input(&format!("data:image/png;base64,{index}")))
        .collect();
    evidence.record("browser", "cell", "call-2", second_response);

    let fragment = evidence.snapshot_since(/*reviewed_sequence*/ 0).unwrap();
    let inputs = fragment.into_inputs(NodeReplReviewEvidenceMode::Multimodal);
    let rendered_images = inputs
        .iter()
        .filter_map(|item| match item {
            UserInput::Image { image_url, .. } => Some(image_url.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let expected_images = ((image_count - MAX_RENDERED_IMAGES)..image_count)
        .map(|index| format!("data:image/png;base64,{index}"))
        .collect::<Vec<_>>();

    assert_eq!(rendered_images, expected_images);
    assert!(rendered_text(&inputs).contains(&format!(
        "<omitted node_repl_images=\"{}\" reason=\"resource_bounds\" />",
        image_count - MAX_RENDERED_IMAGES
    )));
}

#[test]
fn multimodal_evidence_bounds_combined_response_and_image_omissions() {
    let evidence = NodeReplReviewEvidence::default();
    let response_count = MAX_RENDERED_IMAGES + 6;
    let response_text = "x".repeat(MAX_RENDERED_BYTES / (MAX_RENDERED_IMAGES + 2));
    for index in 0..response_count {
        evidence.record(
            "browser",
            "cell",
            &format!("call-{index}"),
            vec![
                text_input(&response_text),
                image_input(&format!("data:image/png;base64,{index}")),
            ],
        );
    }

    let fragment = evidence.snapshot_since(/*reviewed_sequence*/ 0).unwrap();
    let inputs = fragment.into_inputs(NodeReplReviewEvidenceMode::Multimodal);
    let text = rendered_text(&inputs);

    assert!(text.contains("<omitted node_repl_responses="));
    assert!(text.contains("<omitted node_repl_images="));
    assert!(text.len() <= MAX_RENDERED_BYTES);
}
