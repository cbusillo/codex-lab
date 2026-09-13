use super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;

/// Complete captured instructions for the declared files of one bounded external task.
/// The caller enforces byte limits and the Lab supplied-context token limit before launch.
pub(crate) struct BoundedWorkerInstructions {
    pub(crate) paths: Vec<String>,
    pub(crate) instructions: String,
    pub(crate) developer_instructions: Option<String>,
}

impl ContextualUserFragment for BoundedWorkerInstructions {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("bounded_worker.instructions".to_string())
    }

    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (
            "<bounded_worker_instructions>",
            "</bounded_worker_instructions>",
        )
    }

    fn body(&self) -> String {
        format!(
            "Work only on these declared files: {}. Do not delegate this task.\n\nApplicable instructions:\n{}\n\nRole instructions:\n{}\n",
            self.paths.join(", "),
            self.instructions,
            self.developer_instructions.as_deref().unwrap_or_default(),
        )
    }
}
