//! App-server request and notification dispatch for `ChatWidget`.
//!
//! This module translates protocol requests into the focused chat-widget flows
//! that render approvals, permissions, tool input, and guardian reviews.

use super::*;

impl ChatWidget {
    pub(crate) fn handle_server_request(
        &mut self,
        request: ServerRequest,
        replay_kind: Option<ReplayKind>,
    ) {
        let id = request.id().to_string();
        match request {
            ServerRequest::CommandExecutionRequestApproval { params, .. } => {
                let fallback_cwd = self.config.cwd.clone();
                self.on_exec_approval_request(
                    id,
                    exec_approval_request_from_params(params, &fallback_cwd),
                );
            }
            ServerRequest::FileChangeRequestApproval { params, .. } => {
                self.on_apply_patch_approval_request(
                    id,
                    patch_approval_request_from_params(params),
                );
            }
            ServerRequest::McpServerElicitationRequest { request_id, params } => {
                self.on_elicitation_request(request_id, params);
            }
            ServerRequest::PermissionsRequestApproval { params, .. } => {
                self.on_request_permissions(request_permissions_from_params(params));
            }
            ServerRequest::ToolRequestUserInput { params, .. } => {
                self.on_request_user_input(params);
            }
            ServerRequest::DynamicToolCall { .. }
            | ServerRequest::AttestationGenerate { .. }
            | ServerRequest::ChatgptAuthTokensRefresh { .. }
            | ServerRequest::ApplyPatchApproval { .. }
            | ServerRequest::ExecCommandApproval { .. } => {
                if replay_kind.is_none() {
                    self.add_error_message(TUI_STUB_MESSAGE.to_string());
                }
            }
        }
    }

    pub(crate) fn handle_skills_list_response(&mut self, response: SkillsListResponse) {
        self.on_list_skills(response);
    }

    pub(super) fn on_patch_apply_output_delta(&mut self, _item_id: String, _delta: String) {}

    pub(super) fn on_background_auto_review_status_changed(
        &mut self,
        notification: codex_app_server_protocol::BackgroundAutoReviewStatusChangedNotification,
    ) {
        let should_render_status = background_auto_review_status_needs_history(&notification);
        self.review.current_background_review = Some(BackgroundAutoReviewSnapshot {
            run_id: notification.run_id.clone(),
            status: notification.status,
            review_target: notification.review_target.clone(),
            error_summary: notification.error_summary.clone(),
        });
        if should_render_status {
            self.add_to_history(history_cell::new_auto_review_status_cell(&notification));
        }
        self.request_redraw();
    }

    pub(crate) fn handle_auto_review_summary_loaded(
        &mut self,
        run_id: String,
        result: Result<codex_app_server_protocol::AutoReviewSummaryReadResponse, String>,
    ) {
        match result {
            Ok(response) if auto_review_summary_contains_run(&response, &run_id) => {
                self.add_to_history(history_cell::new_auto_review_summary_cell(&response));
            }
            Ok(_) => return,
            Err(err) if self.current_background_review_matches_run(&run_id) => {
                self.add_to_history(history_cell::new_auto_review_summary_error_cell(err));
            }
            Err(_) => return,
        }
        self.request_redraw();
    }

    pub(super) fn on_guardian_review_notification(
        &mut self,
        id: String,
        turn_id: String,
        started_at_ms: i64,
        review: codex_app_server_protocol::GuardianApprovalReview,
        completion: Option<(i64, codex_app_server_protocol::AutoReviewDecisionSource)>,
        action: GuardianApprovalReviewAction,
    ) {
        let (completed_at_ms, decision_source) = match completion {
            Some((completed_at_ms, decision_source)) => {
                (Some(completed_at_ms), Some(decision_source))
            }
            None => (None, None),
        };

        self.on_guardian_assessment(GuardianAssessmentEvent {
            id,
            target_item_id: None,
            turn_id,
            started_at_ms,
            completed_at_ms,
            status: match review.status {
                codex_app_server_protocol::GuardianApprovalReviewStatus::InProgress => {
                    GuardianAssessmentStatus::InProgress
                }
                codex_app_server_protocol::GuardianApprovalReviewStatus::Approved => {
                    GuardianAssessmentStatus::Approved
                }
                codex_app_server_protocol::GuardianApprovalReviewStatus::Denied => {
                    GuardianAssessmentStatus::Denied
                }
                codex_app_server_protocol::GuardianApprovalReviewStatus::TimedOut => {
                    GuardianAssessmentStatus::TimedOut
                }
                codex_app_server_protocol::GuardianApprovalReviewStatus::Aborted => {
                    GuardianAssessmentStatus::Aborted
                }
            },
            risk_level: review.risk_level.map(|risk_level| match risk_level {
                codex_app_server_protocol::GuardianRiskLevel::Low => {
                    codex_protocol::approvals::GuardianRiskLevel::Low
                }
                codex_app_server_protocol::GuardianRiskLevel::Medium => {
                    codex_protocol::approvals::GuardianRiskLevel::Medium
                }
                codex_app_server_protocol::GuardianRiskLevel::High => {
                    codex_protocol::approvals::GuardianRiskLevel::High
                }
                codex_app_server_protocol::GuardianRiskLevel::Critical => {
                    codex_protocol::approvals::GuardianRiskLevel::Critical
                }
            }),
            user_authorization: review.user_authorization.map(|user_authorization| {
                match user_authorization {
                    codex_app_server_protocol::GuardianUserAuthorization::Unknown => {
                        codex_protocol::approvals::GuardianUserAuthorization::Unknown
                    }
                    codex_app_server_protocol::GuardianUserAuthorization::Low => {
                        codex_protocol::approvals::GuardianUserAuthorization::Low
                    }
                    codex_app_server_protocol::GuardianUserAuthorization::Medium => {
                        codex_protocol::approvals::GuardianUserAuthorization::Medium
                    }
                    codex_app_server_protocol::GuardianUserAuthorization::High => {
                        codex_protocol::approvals::GuardianUserAuthorization::High
                    }
                }
            }),
            rationale: review.rationale,
            decision_source: decision_source.map(|source| match source {
                codex_app_server_protocol::AutoReviewDecisionSource::Agent => {
                    GuardianAssessmentDecisionSource::Agent
                }
            }),
            action: action.into(),
        });
    }

    pub(super) fn on_shutdown_complete(&mut self) {
        self.request_immediate_exit();
    }

    pub(super) fn on_turn_diff(&mut self, unified_diff: String) {
        debug!("TurnDiffEvent: {unified_diff}");
        self.refresh_status_line();
    }

    pub(super) fn on_deprecation_notice(&mut self, summary: String, details: Option<String>) {
        self.add_to_history(history_cell::new_deprecation_notice(summary, details));
        self.request_redraw();
    }
}

impl ChatWidget {
    fn current_background_review_matches_run(&self, run_id: &str) -> bool {
        self.review
            .current_background_review
            .as_ref()
            .is_some_and(|review| review.run_id == run_id)
    }
}

fn auto_review_summary_contains_run(
    response: &codex_app_server_protocol::AutoReviewSummaryReadResponse,
    run_id: &str,
) -> bool {
    response
        .current
        .as_ref()
        .is_some_and(|summary| summary.run_id == run_id)
        || response
            .latest
            .as_ref()
            .is_some_and(|summary| summary.run_id == run_id)
}

fn background_auto_review_status_needs_history(
    notification: &codex_app_server_protocol::BackgroundAutoReviewStatusChangedNotification,
) -> bool {
    // Progress and normal completion live in status state; transcript history is
    // reserved for terminal statuses that carry a reason the user should see.
    match notification.status {
        codex_app_server_protocol::BackgroundAutoReviewStatus::Failed
        | codex_app_server_protocol::BackgroundAutoReviewStatus::Cancelled
        | codex_app_server_protocol::BackgroundAutoReviewStatus::Superseded
        | codex_app_server_protocol::BackgroundAutoReviewStatus::Skipped => notification
            .error_summary
            .as_deref()
            .is_some_and(|summary| !summary.trim().is_empty()),
        codex_app_server_protocol::BackgroundAutoReviewStatus::Pending
        | codex_app_server_protocol::BackgroundAutoReviewStatus::Running
        | codex_app_server_protocol::BackgroundAutoReviewStatus::Completed => false,
    }
}
