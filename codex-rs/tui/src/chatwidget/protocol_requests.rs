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
                // TODO(anp): Remove this native-path localization error path once core permission
                // paths remain PathUri after crossing the app-server boundary.
                match request_permissions_from_params(params) {
                    Ok(event) => self.on_request_permissions(event),
                    Err(err) => {
                        self.add_error_message(format!(
                            "failed to localize requested filesystem paths: {err}"
                        ));
                    }
                }
            }
            ServerRequest::ToolRequestUserInput { params, .. } => {
                self.on_request_user_input(params);
            }
            ServerRequest::DynamicToolCall { .. }
            | ServerRequest::AttestationGenerate { .. }
            | ServerRequest::CurrentTimeRead { .. }
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
        let updates_snapshot = self.background_auto_review_status_updates_snapshot(&notification);
        if updates_snapshot {
            self.review.current_background_review = Some(BackgroundAutoReviewSnapshot {
                run_id: notification.run_id.clone(),
                status: notification.status,
                review_target: notification.review_target.clone(),
                error_summary: notification.error_summary.clone(),
            });
            let label = match notification.status {
                codex_app_server_protocol::BackgroundAutoReviewStatus::Pending => {
                    Some("Background review queued".to_string())
                }
                codex_app_server_protocol::BackgroundAutoReviewStatus::Running => {
                    Some("Background review running".to_string())
                }
                codex_app_server_protocol::BackgroundAutoReviewStatus::Completed
                | codex_app_server_protocol::BackgroundAutoReviewStatus::Failed
                | codex_app_server_protocol::BackgroundAutoReviewStatus::Cancelled
                | codex_app_server_protocol::BackgroundAutoReviewStatus::Superseded
                | codex_app_server_protocol::BackgroundAutoReviewStatus::Skipped => None,
            };
            self.bottom_pane.set_background_review_label(label);
        }
        if background_auto_review_status_is_terminal(notification.status) {
            self.review
                .pending_terminal_background_reviews
                .insert(notification.run_id.clone(), notification);
        }
        self.request_redraw();
    }

    pub(crate) fn handle_auto_review_summary_loaded(
        &mut self,
        run_id: String,
        result: Result<codex_app_server_protocol::AutoReviewSummaryReadResponse, String>,
    ) {
        let fallback = self
            .review
            .pending_terminal_background_reviews
            .remove(&run_id);
        if self
            .review
            .rendered_terminal_background_reviews
            .contains(&run_id)
        {
            return;
        }
        match result {
            Ok(response) => {
                let current_matches = response.current.as_ref().is_some_and(|summary| {
                    summary.run_id == run_id
                        && background_auto_review_status_is_terminal(summary.status)
                });
                if current_matches {
                    self.add_to_history(history_cell::new_auto_review_summary_cell(&response));
                } else if let Some(latest) = response.latest.as_ref().filter(|summary| {
                    summary.run_id == run_id
                        && background_auto_review_status_is_terminal(summary.status)
                }) {
                    if latest.status
                        == codex_app_server_protocol::BackgroundAutoReviewStatus::Completed
                        && latest.freshness
                            != codex_app_server_protocol::AutoReviewFreshness::Current
                    {
                        self.add_to_history(history_cell::new_hidden_auto_review_run_summary_cell(
                            latest,
                            response.diagnostics.as_ref(),
                        ));
                    } else {
                        self.add_to_history(history_cell::new_auto_review_run_summary_cell(
                            latest,
                            response.diagnostics.as_ref(),
                        ));
                    }
                } else if let Some(fallback) = fallback.as_ref() {
                    self.add_to_history(history_cell::new_auto_review_status_cell(fallback));
                } else {
                    return;
                }
            }
            Err(err) => {
                if let Some(fallback) = fallback.as_ref() {
                    self.add_to_history(history_cell::new_auto_review_status_cell(fallback));
                } else if self.current_background_review_matches_run(&run_id) {
                    self.add_to_history(history_cell::new_auto_review_summary_error_cell(err));
                } else {
                    return;
                }
            }
        }
        self.review
            .rendered_terminal_background_reviews
            .insert(run_id);
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
        // TODO(anp): Remove this native-path localization error path once core permission paths
        // remain PathUri after crossing the app-server boundary.
        let action = match action.try_into() {
            Ok(action) => action,
            Err(err) => {
                self.add_error_message(format!(
                    "failed to localize guardian filesystem paths: {err}"
                ));
                return;
            }
        };
        let (completed_at_ms, decision_source) = match completion {
            Some((completed_at_ms, decision_source)) => {
                (Some(completed_at_ms), Some(decision_source))
            }
            None => (None, None),
        };

        self.on_guardian_assessment(GuardianAssessmentEvent {
            id,
            target_item_id: None,
            plugin_id: None,
            script_path: None,
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
            action,
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
    fn background_auto_review_status_updates_snapshot(
        &self,
        notification: &codex_app_server_protocol::BackgroundAutoReviewStatusChangedNotification,
    ) -> bool {
        let Some(current) = &self.review.current_background_review else {
            return true;
        };
        current.run_id == notification.run_id
            || !background_auto_review_status_is_terminal(notification.status)
    }

    fn current_background_review_matches_run(&self, run_id: &str) -> bool {
        self.review
            .current_background_review
            .as_ref()
            .is_some_and(|review| review.run_id == run_id)
    }
}

fn background_auto_review_status_is_terminal(
    status: codex_app_server_protocol::BackgroundAutoReviewStatus,
) -> bool {
    match status {
        codex_app_server_protocol::BackgroundAutoReviewStatus::Completed
        | codex_app_server_protocol::BackgroundAutoReviewStatus::Failed
        | codex_app_server_protocol::BackgroundAutoReviewStatus::Cancelled
        | codex_app_server_protocol::BackgroundAutoReviewStatus::Superseded
        | codex_app_server_protocol::BackgroundAutoReviewStatus::Skipped => true,
        codex_app_server_protocol::BackgroundAutoReviewStatus::Pending
        | codex_app_server_protocol::BackgroundAutoReviewStatus::Running => false,
    }
}
