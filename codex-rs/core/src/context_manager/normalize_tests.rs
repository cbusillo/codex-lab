use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

use super::strip_audio_when_unsupported;
use super::strip_images_when_unsupported;

fn text_message() -> ResponseItemEnvelope {
    ResponseItemEnvelope::new(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "plain text".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    })
}

#[test]
fn unsupported_media_normalization_preserves_unaffected_messages() {
    let original = text_message();

    let mut image_items = vec![original.clone()];
    strip_images_when_unsupported(&[], &mut image_items);
    assert_eq!(image_items, vec![original.clone()]);

    let mut audio_items = vec![original.clone()];
    strip_audio_when_unsupported(&[], &mut audio_items);
    assert_eq!(audio_items, vec![original]);
}
