use codex_protocol::user_input::UserInput;
use pretty_assertions::assert_eq;

use super::MAX_RENDERED_BYTES;
use super::MAX_RENDERED_IMAGES;
use super::NodeReplContext;
use super::NodeReplResponse;
use super::NodeReplReviewEvidenceMode;

fn image(image_url: String) -> UserInput {
    UserInput::Image {
        image_url,
        detail: None,
    }
}

fn rendered_text(items: &[UserInput]) -> String {
    items
        .iter()
        .filter_map(|item| match item {
            UserInput::Text { text, .. } => Some(text.as_str()),
            UserInput::Image { .. } => None,
        })
        .collect()
}

#[test]
fn multimodal_evidence_caps_distinct_images_at_the_newest_four() {
    let first_items = (0..3)
        .map(|index| image(format!("data:image/png;base64,{index}")))
        .collect::<Vec<_>>();
    let second_items = (3..8)
        .map(|index| image(format!("data:image/png;base64,{index}")))
        .collect::<Vec<_>>();
    let context = NodeReplContext {
        responses: vec![
            NodeReplResponse {
                sequence: 1,
                provenance: "tool=browser",
                items: &first_items,
            },
            NodeReplResponse {
                sequence: 2,
                provenance: "tool=browser",
                items: &second_items,
            },
        ],
        omitted_responses: 0,
        mode: NodeReplReviewEvidenceMode::Multimodal,
    };

    let inputs = context.render_inputs();
    let rendered_images = inputs
        .iter()
        .filter_map(|item| match item {
            UserInput::Image { image_url, .. } => Some(image_url.clone()),
            UserInput::Text { .. } => None,
        })
        .collect::<Vec<_>>();
    let expected_images = (4..8)
        .map(|index| format!("data:image/png;base64,{index}"))
        .collect::<Vec<_>>();

    assert_eq!(rendered_images, expected_images);
    assert!(rendered_text(&inputs).contains(&format!(
        "<omitted node_repl_images=\"{}\" reason=\"resource_bounds\" />",
        8 - MAX_RENDERED_IMAGES
    )));
}

#[test]
fn multimodal_evidence_bounds_combined_response_and_image_omissions() {
    let response_text = "x".repeat(MAX_RENDERED_BYTES / (MAX_RENDERED_IMAGES + 2));
    let items = (0..(MAX_RENDERED_IMAGES + 6))
        .map(|index| {
            vec![
                UserInput::Text {
                    text: response_text.clone(),
                    text_elements: Vec::new(),
                },
                image(format!("data:image/png;base64,{index}")),
            ]
        })
        .collect::<Vec<_>>();
    let responses = items
        .iter()
        .enumerate()
        .map(|(index, items)| NodeReplResponse {
            sequence: u64::try_from(index).unwrap_or(u64::MAX),
            provenance: "tool=browser",
            items,
        })
        .collect();
    let context = NodeReplContext {
        responses,
        omitted_responses: 0,
        mode: NodeReplReviewEvidenceMode::Multimodal,
    };

    let inputs = context.render_inputs();
    let text = rendered_text(&inputs);

    assert!(text.contains("<omitted node_repl_responses="));
    assert!(text.contains("<omitted node_repl_images="));
    assert!(text.len() <= MAX_RENDERED_BYTES);
}
