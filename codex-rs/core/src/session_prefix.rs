use codex_protocol::AgentPath;
use codex_protocol::protocol::AgentStatus;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::truncate_text;

use crate::context::ContextualUserFragment;
use crate::context::InterAgentCompletionMessage;
use crate::context::SubagentNotification;

const COMPLETION_MESSAGE_MAX_TOKENS: usize = 1_000;
const COMPLETION_MESSAGE_ENVELOPE_TOKEN_RESERVE: usize = 100;
/// Budget for the agent-authored part of a completion envelope, leaving room for the fixed
/// envelope text (task name, sender, next-action guidance) inside
/// [`COMPLETION_MESSAGE_MAX_TOKENS`].
pub(crate) const COMPLETION_PAYLOAD_MAX_TOKENS: usize =
    COMPLETION_MESSAGE_MAX_TOKENS - COMPLETION_MESSAGE_ENVELOPE_TOKEN_RESERVE;
const ERROR_NEXT_ACTION: &str = "This agent's turn failed. If you still need this agent, use the available collaboration tools to give it another task.";

// Helpers for model-visible session state markers that are stored in user-role
// messages but are not user intent.

// TODO(jif) unify with structured schema
pub(crate) fn format_subagent_notification_message(
    agent_reference: &str,
    status: &AgentStatus,
) -> String {
    SubagentNotification::new(agent_reference, bounded_status(status)).render()
}

/// Apply the completion-message budget to every agent-authored string an [`AgentStatus`] can
/// carry. Both the completion message and the error text originate from an agent (or an external
/// agent process) and are otherwise unbounded before entering model-visible history.
pub(crate) fn bounded_status(status: &AgentStatus) -> AgentStatus {
    match status {
        AgentStatus::Completed(Some(message)) => {
            AgentStatus::Completed(Some(bounded_completion_payload(message)))
        }
        AgentStatus::Errored(error) => AgentStatus::Errored(bounded_completion_payload(error)),
        AgentStatus::Completed(None)
        | AgentStatus::Shutdown
        | AgentStatus::NotFound
        | AgentStatus::PendingInit
        | AgentStatus::Running
        | AgentStatus::Interrupted => status.clone(),
    }
}

pub(crate) fn bounded_completion_payload(payload: &str) -> String {
    truncate_text(
        payload,
        TruncationPolicy::Tokens(COMPLETION_PAYLOAD_MAX_TOKENS),
    )
}

pub(crate) fn format_inter_agent_completion_message(
    task_name: AgentPath,
    sender: AgentPath,
    status: &AgentStatus,
) -> Option<String> {
    let payload = match status {
        AgentStatus::Completed(Some(message)) => bounded_completion_payload(message),
        AgentStatus::Completed(None) => String::new(),
        AgentStatus::Errored(error) => {
            let error = bounded_completion_payload(error);
            format!("Agent errored: {error}\n\n{ERROR_NEXT_ACTION}")
        }
        AgentStatus::Shutdown => "Agent shut down.".to_string(),
        AgentStatus::NotFound => "Agent was not found.".to_string(),
        AgentStatus::PendingInit | AgentStatus::Running | AgentStatus::Interrupted => return None,
    };
    Some(InterAgentCompletionMessage::new(task_name, sender, payload).render())
}

#[cfg(test)]
#[path = "session_prefix_tests.rs"]
mod tests;

pub(crate) fn format_subagent_context_line(
    agent_reference: &str,
    agent_nickname: Option<&str>,
) -> String {
    match agent_nickname.filter(|nickname| !nickname.is_empty()) {
        Some(agent_nickname) => format!("- {agent_reference}: {agent_nickname}"),
        None => format!("- {agent_reference}"),
    }
}
