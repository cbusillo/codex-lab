use super::Turn;
use super::shared::v2_enum_from_core;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

v2_enum_from_core!(
    pub enum ReviewDelivery from codex_protocol::protocol::ReviewDelivery {
        Inline, Detached
    }
);
v2_enum_from_core!(
    pub enum BackgroundAutoReviewStatus from codex_protocol::protocol::BackgroundAutoReviewStatus {
        Pending, Running, Completed, Failed, Cancelled, Superseded, Skipped
    }
);

v2_enum_from_core!(
    pub enum BackgroundAutoReviewControlAction from codex_protocol::protocol::BackgroundAutoReviewControlAction {
        Cancel, Supersede
    }
);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", export_to = "v2/")]
pub enum BackgroundAutoReviewControlReason {
    UserRequested,
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    SupersededByRun {
        run_id: String,
    },
    ForegroundWorkStarted,
    ThreadClosing,
}

impl BackgroundAutoReviewControlReason {
    pub fn to_core(self) -> codex_protocol::protocol::BackgroundAutoReviewControlReason {
        match self {
            Self::UserRequested => {
                codex_protocol::protocol::BackgroundAutoReviewControlReason::UserRequested
            }
            Self::SupersededByRun { run_id } => {
                codex_protocol::protocol::BackgroundAutoReviewControlReason::SupersededByRun {
                    run_id,
                }
            }
            Self::ForegroundWorkStarted => {
                codex_protocol::protocol::BackgroundAutoReviewControlReason::ForegroundWorkStarted
            }
            Self::ThreadClosing => {
                codex_protocol::protocol::BackgroundAutoReviewControlReason::ThreadClosing
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct BackgroundAutoReviewStatusChangedNotification {
    pub thread_id: String,
    pub run_id: String,
    pub status: BackgroundAutoReviewStatus,
    pub review_target: ReviewTarget,
    pub error_summary: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ReviewStartParams {
    pub thread_id: String,
    pub target: ReviewTarget,

    /// Where to run the review: inline (default) on the current thread or
    /// detached on a new thread (returned in `reviewThreadId`).
    #[serde(default)]
    #[ts(optional = nullable)]
    pub delivery: Option<ReviewDelivery>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ReviewStartResponse {
    pub turn: Turn,
    /// Identifies the thread where the review runs.
    ///
    /// For inline reviews, this is the original thread id.
    /// For detached reviews, this is the id of the new review thread.
    pub review_thread_id: String,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct BackgroundAutoReviewControlParams {
    pub thread_id: String,
    pub run_id: String,
    pub action: BackgroundAutoReviewControlAction,
    pub reason: BackgroundAutoReviewControlReason,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct BackgroundAutoReviewControlResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoReviewSummaryReadParams {
    pub thread_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoReviewSummaryReadResponse {
    pub latest: Option<AutoReviewRunSummary>,
    pub current: Option<AutoReviewRunSummary>,
    pub status_counts: Vec<AutoReviewStatusCount>,
    pub diagnostics: Option<AutoReviewDiagnosticsSummary>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoReviewDiagnosticsSummary {
    pub recent_runs: usize,
    pub in_flight_runs: usize,
    pub terminal_runs: usize,
    pub skipped_runs: usize,
    pub duplicate_skipped_runs: usize,
    pub superseded_runs: usize,
    pub failed_runs: usize,
    pub cancelled_runs: usize,
    pub lost_runs: usize,
    pub suppressed_stale_runs: usize,
    pub compact: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoReviewRunSummary {
    pub run_id: String,
    pub status: BackgroundAutoReviewStatus,
    pub source: AutoReviewRunSource,
    pub freshness: AutoReviewFreshness,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number | null")]
    pub completed_at: Option<i64>,
    pub model: Option<String>,
    pub error_summary: Option<String>,
    pub rendered_findings: usize,
    pub omitted_findings: usize,
    pub truncated: bool,
    pub content: String,
    pub budget: Option<AutoReviewBudget>,
    pub usage: AutoReviewUsage,
    pub terminal_reason: Option<AutoReviewTerminalReason>,
    pub finding_disposition: Option<AutoReviewFindingDispositionRecord>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoReviewBudget {
    pub max_scope_bytes: usize,
    #[ts(type = "number")]
    pub max_elapsed_ms: u64,
    #[ts(type = "number")]
    pub max_total_tokens: u64,
    pub max_output_bytes: usize,
    pub max_findings: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoReviewUsage {
    pub scope_bytes: Option<usize>,
    #[ts(type = "number | null")]
    pub elapsed_ms: Option<u64>,
    #[ts(type = "number | null")]
    pub total_tokens: Option<u64>,
    pub output_bytes: Option<usize>,
    pub finding_count: Option<usize>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum AutoReviewTerminalReason {
    BudgetScope,
    BudgetElapsed,
    BudgetTotalTokens,
    BudgetOutput,
    BudgetFindingCount,
    EmptyOutput,
    StaleTarget,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum AutoReviewFindingDisposition {
    NeedsAttention,
    Repairing,
    Deferred,
    Obsolete,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum AutoReviewDispositionActor {
    User,
    Agent,
    System,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoReviewFindingDispositionRecord {
    pub disposition: AutoReviewFindingDisposition,
    pub actor: AutoReviewDispositionActor,
    pub reason: Option<String>,
    #[ts(type = "number")]
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoReviewStatusCount {
    pub status: BackgroundAutoReviewStatus,
    pub source: AutoReviewRunSource,
    pub freshness: AutoReviewFreshness,
    pub target_matches: bool,
    pub count: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum AutoReviewRunSource {
    Manual,
    Background,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum AutoReviewFreshness {
    Current,
    Stale,
    Detached,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoReviewFindingDetailReadParams {
    pub thread_id: String,
    pub run_id: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub finding_id: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub max_bytes: Option<usize>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum AutoReviewDetailKind {
    Run,
    Finding,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoReviewFindingDetailReadResponse {
    pub run_id: String,
    pub detail_kind: AutoReviewDetailKind,
    pub finding_id: Option<String>,
    pub finding_count: usize,
    pub omitted_findings: usize,
    pub bytes: usize,
    pub original_bytes: usize,
    pub max_bytes: usize,
    pub truncated: bool,
    pub content: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum AutoReviewDispositionAction {
    Repair,
    Defer,
    Obsolete,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoReviewDispositionWriteParams {
    pub thread_id: String,
    pub run_id: String,
    pub action: AutoReviewDispositionAction,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub reason: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoReviewDispositionWriteResponse {
    pub run_id: String,
    pub finding_disposition: AutoReviewFindingDispositionRecord,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", export_to = "v2/")]
pub enum ReviewTarget {
    /// Review the working tree: staged, unstaged, and untracked files.
    UncommittedChanges,

    /// Review the changes made by a completed turn.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    CurrentTurnDiff { fingerprint: String },

    /// Review changes between the current branch and the given base branch.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    BaseBranch { branch: String },

    /// Review the changes introduced by a specific commit.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Commit {
        sha: String,
        /// Optional human-readable label (e.g., commit subject) for UIs.
        title: Option<String>,
    },

    /// Arbitrary instructions, equivalent to the old free-form prompt.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Custom { instructions: String },
}

impl From<codex_protocol::protocol::ReviewTarget> for ReviewTarget {
    fn from(value: codex_protocol::protocol::ReviewTarget) -> Self {
        match value {
            codex_protocol::protocol::ReviewTarget::UncommittedChanges => {
                ReviewTarget::UncommittedChanges
            }
            codex_protocol::protocol::ReviewTarget::CurrentTurnDiff { fingerprint } => {
                ReviewTarget::CurrentTurnDiff { fingerprint }
            }
            codex_protocol::protocol::ReviewTarget::BaseBranch { branch } => {
                ReviewTarget::BaseBranch { branch }
            }
            codex_protocol::protocol::ReviewTarget::Commit { sha, title } => {
                ReviewTarget::Commit { sha, title }
            }
            codex_protocol::protocol::ReviewTarget::Custom { instructions } => {
                ReviewTarget::Custom { instructions }
            }
        }
    }
}
