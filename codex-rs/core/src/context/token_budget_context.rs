use super::ContextualUserFragment;
use super::world_state::PreviousSectionState;
use super::world_state::WorldStateSection;
use codex_protocol::AgentPath;
use codex_protocol::models::ContentItemKind;
use codex_protocol::protocol::CONTEXT_WINDOW_CLOSE_TAG;
use codex_protocol::protocol::CONTEXT_WINDOW_GUIDANCE_CLOSE_TAG;
use codex_protocol::protocol::CONTEXT_WINDOW_GUIDANCE_OPEN_TAG;
use codex_protocol::protocol::CONTEXT_WINDOW_OPEN_TAG;
use serde_json::Value;
use uuid::Uuid;

/// Hard rendered-byte cap for the MCP `notes/thread_hint` text carried by
/// [`TokenBudgetContext`].
///
/// `TokenBudgetContext` is emitted into the permanent model-visible prefix, so the hint text is
/// paid for on every request of the thread and can never be compacted away. The MCP server is an
/// untrusted, arbitrarily verbose source, so the hint gets a hard byte cap instead of a
/// best-effort token budget: 2 KiB is roughly 512 approximate tokens, comfortably below the 1K
/// token threshold that requires manual review of a context fragment.
pub(crate) const MAX_THREAD_HINT_BYTES: usize = 2 * 1024;
const THREAD_HINT_TRUNCATION_NOTICE: &str = "\n[thread hint truncated]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenBudgetContext {
    agent_path: AgentPath,
    first_window_id: Uuid,
    previous_window_id: Option<Uuid>,
    window_id: Uuid,
    thread_hint: Option<String>,
}

impl TokenBudgetContext {
    pub(crate) fn new(
        agent_path: AgentPath,
        first_window_id: Uuid,
        previous_window_id: Option<Uuid>,
        window_id: Uuid,
        thread_hint: Option<String>,
    ) -> Self {
        Self {
            agent_path,
            first_window_id,
            previous_window_id,
            window_id,
            // Bound here rather than at the call site: this constructor is the only way to build
            // the fragment, so no caller can introduce an unbounded bypass.
            thread_hint: thread_hint.and_then(|hint| bounded_thread_hint(&hint)),
        }
    }
}

/// Join the text content items of an MCP `notes/thread_hint` result into a single bounded hint.
///
/// Accumulation stops once the cap is reached so a result with many items, or a single oversized
/// item, never materializes in full before being truncated.
pub(crate) fn join_thread_hint_content(content: &[Value]) -> Option<String> {
    let mut joined = String::new();
    for text in content
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .filter(|text| !text.is_empty())
    {
        if !joined.is_empty() {
            joined.push('\n');
        }
        joined.push_str(text);
        if joined.len() > MAX_THREAD_HINT_BYTES {
            break;
        }
    }
    bounded_thread_hint(&joined)
}

/// Truncate `hint` so the rendered text never exceeds [`MAX_THREAD_HINT_BYTES`] bytes.
fn bounded_thread_hint(hint: &str) -> Option<String> {
    if hint.is_empty() {
        return None;
    }
    if hint.len() <= MAX_THREAD_HINT_BYTES {
        return Some(hint.to_string());
    }

    let mut boundary = MAX_THREAD_HINT_BYTES.saturating_sub(THREAD_HINT_TRUNCATION_NOTICE.len());
    while boundary > 0 && !hint.is_char_boundary(boundary) {
        boundary -= 1;
    }
    Some(format!(
        "{}{THREAD_HINT_TRUNCATION_NOTICE}",
        &hint[..boundary]
    ))
}

impl ContextualUserFragment for TokenBudgetContext {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("token_budget.context_window".to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (CONTEXT_WINDOW_OPEN_TAG, CONTEXT_WINDOW_CLOSE_TAG)
    }

    fn body(&self) -> String {
        let agent_path = &self.agent_path;
        let first_window_id = self.first_window_id;
        let window_id = self.window_id;
        let mut lines = vec![
            format!("Agent name: {agent_path}"),
            format!("First context window id: {first_window_id}"),
            format!("Current context window id: {window_id}"),
        ];
        if let Some(previous_window_id) = self.previous_window_id {
            lines.push(format!("Previous context window id: {previous_window_id}"));
        }
        if let Some(thread_hint) = &self.thread_hint {
            lines.push(thread_hint.clone());
        }
        format!("\n{}\n", lines.join("\n"))
    }
}

impl WorldStateSection for TokenBudgetContext {
    const ID: &'static str = "context_window";
    type Snapshot = AgentPath;

    fn snapshot(&self) -> Self::Snapshot {
        self.agent_path.clone()
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> Option<Box<dyn ContextualUserFragment>> {
        matches!(previous, PreviousSectionState::Known(agent_path) if agent_path != &self.agent_path)
            .then(|| Box::new(self.clone()) as Box<dyn ContextualUserFragment>)
    }
}

#[cfg(test)]
#[path = "token_budget_context_tests.rs"]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextWindowGuidance {
    message: String,
}

impl ContextWindowGuidance {
    pub(crate) fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}

impl ContextualUserFragment for ContextWindowGuidance {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("token_budget.context_window_guidance".to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (
            CONTEXT_WINDOW_GUIDANCE_OPEN_TAG,
            CONTEXT_WINDOW_GUIDANCE_CLOSE_TAG,
        )
    }

    fn body(&self) -> String {
        format!("\n{}\n", self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenBudgetRemainingContext {
    tokens_left: Option<i64>,
}

impl TokenBudgetRemainingContext {
    pub(crate) fn new(tokens_left: i64) -> Self {
        Self {
            tokens_left: Some(tokens_left),
        }
    }

    pub(crate) fn unknown() -> Self {
        Self { tokens_left: None }
    }
}

impl ContextualUserFragment for TokenBudgetRemainingContext {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("token_budget.remaining_tokens".to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> String {
        match self.tokens_left {
            Some(tokens_left) => {
                format!("You have {tokens_left} tokens left in this context window.")
            }
            None => "You have unknown tokens left in this context window.".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenBudgetReminder {
    message: String,
}

impl TokenBudgetReminder {
    pub(crate) fn new(message_template: &str, n_remaining: i64) -> Self {
        Self {
            message: message_template.replace("{n_remaining}", &n_remaining.to_string()),
        }
    }
}

impl ContextualUserFragment for TokenBudgetReminder {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("token_budget.reminder".to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> String {
        self.message.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutoCompactFallbackPrompt {
    message: String,
}

impl AutoCompactFallbackPrompt {
    pub(crate) fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}

impl ContextualUserFragment for AutoCompactFallbackPrompt {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("compaction.auto_fallback_prompt".to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> String {
        self.message.clone()
    }
}
