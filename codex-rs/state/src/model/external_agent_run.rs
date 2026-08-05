use codex_protocol::ThreadId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalAgentRun {
    pub child_thread_id: ThreadId,
    pub parent_thread_id: ThreadId,
    pub agent_path: Option<String>,
    pub routing_kind: String,
    pub requested_selector: Option<String>,
    pub effective_selector: String,
    pub routing_reason: String,
    pub skipped_candidates_json: String,
    pub provider_family: Option<String>,
    pub command: String,
    pub cli_version: Option<String>,
    pub capability_source: String,
    pub capability_freshness: Option<String>,
    pub protocol: String,
    pub mode: String,
    pub workspace: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub duration_ms: Option<u64>,
    pub terminal_state: Option<String>,
    pub failure_kind: Option<String>,
    pub failure_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalAgentRunStart {
    pub child_thread_id: ThreadId,
    pub parent_thread_id: ThreadId,
    pub agent_path: Option<String>,
    pub routing_kind: String,
    pub requested_selector: Option<String>,
    pub effective_selector: String,
    pub routing_reason: String,
    pub skipped_candidates_json: String,
    pub provider_family: Option<String>,
    pub command: String,
    pub cli_version: Option<String>,
    pub capability_source: String,
    pub capability_freshness: Option<String>,
    pub protocol: String,
    pub mode: String,
    pub workspace: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub started_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalAgentRunOutcome {
    pub completed_at_ms: i64,
    pub duration_ms: u64,
    pub terminal_state: String,
    pub failure_kind: Option<String>,
    pub failure_message: Option<String>,
}
