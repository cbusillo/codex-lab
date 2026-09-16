use super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RealtimeInteractiveRequestKind {
    Approval,
    Input,
}

/// A bounded realtime notice that leaves interactive request details in the app.
pub(crate) struct RealtimeInteractiveRequestNotice {
    kind: RealtimeInteractiveRequestKind,
}

impl RealtimeInteractiveRequestNotice {
    pub(crate) fn new(kind: RealtimeInteractiveRequestKind) -> Self {
        Self { kind }
    }
}

impl ContextualUserFragment for RealtimeInteractiveRequestNotice {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("realtime_conversation.interactive_request_notice".to_string())
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (
            "<realtime_interactive_request_notice>",
            "</realtime_interactive_request_notice>",
        )
    }

    fn body(&self) -> String {
        match self.kind {
            RealtimeInteractiveRequestKind::Approval => {
                "I need your approval to continue. Please review the request in the app."
                    .to_string()
            }
            RealtimeInteractiveRequestKind::Input => {
                "I need your input. Please respond in the app.".to_string()
            }
        }
    }
}
