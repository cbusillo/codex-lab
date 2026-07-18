use super::ContextualUserFragment;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeIdentity;

impl RuntimeIdentity {
    pub(crate) fn matches_content_item(content_item: &ContentItem) -> bool {
        matches!(
            content_item,
            ContentItem::InputText { text } if text == &RuntimeIdentity.render()
        )
    }

    pub(crate) fn matches_response_item(item: &ResponseItem) -> bool {
        let ResponseItem::Message { role, content, .. } = item else {
            return false;
        };
        role == "developer" && content.iter().any(Self::matches_content_item)
    }
}

impl ContextualUserFragment for RuntimeIdentity {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<runtime_identity>", "</runtime_identity>")
    }

    fn body(&self) -> String {
        concat!(
            "\nAgent identity: Codex\n",
            "Runtime harness: Codex Lab\n",
            "When asked which harness is active, answer `Codex Lab`.\n"
        )
        .to_string()
    }
}
