//! Code-review flow state for `ChatWidget`.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::auto_review_denials::RecentAutoReviewDenials;
use crate::token_usage::TokenUsageInfo;
use codex_app_server_protocol::BackgroundAutoReviewStatus;
use codex_app_server_protocol::ReviewTarget;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct BackgroundAutoReviewSnapshot {
    pub(super) run_id: String,
    pub(super) status: BackgroundAutoReviewStatus,
    pub(super) review_target: ReviewTarget,
    pub(super) error_summary: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct ReviewState {
    pub(super) recent_auto_review_denials: RecentAutoReviewDenials,
    pub(super) current_background_review: Option<BackgroundAutoReviewSnapshot>,
    pub(super) pending_terminal_background_reviews:
        HashMap<String, codex_app_server_protocol::BackgroundAutoReviewStatusChangedNotification>,
    pub(super) rendered_terminal_background_reviews: HashSet<String>,
    /// Simple review mode flag; used to adjust layout and banners.
    pub(super) is_review_mode: bool,
    /// Snapshot of token usage to restore after review mode exits.
    pub(super) pre_review_token_info: Option<Option<TokenUsageInfo>>,
}
