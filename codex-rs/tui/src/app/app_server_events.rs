//! App-server event stream handling for the TUI app.

use super::App;
use super::app_server_event_targets::ServerNotificationThreadTarget;
use super::app_server_event_targets::server_notification_thread_target;
use super::app_server_event_targets::server_request_thread_id;
use crate::app_command::AppCommand;
use crate::app_event::AppEvent;
use crate::app_event::ConnectorsSnapshot;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::status_account_display_from_auth_mode;
use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::AccountLoginCompletedNotification;
use codex_app_server_protocol::AccountUpdatedNotification;
use codex_app_server_protocol::AuthMode;
use codex_app_server_protocol::BackgroundAutoReviewStatus;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_protocol::ThreadId;

impl App {
    fn refresh_mcp_startup_expected_servers_from_config(&mut self) {
        let enabled_config_mcp_servers: Vec<String> = self
            .chat_widget
            .config_ref()
            .mcp_servers
            .get()
            .iter()
            .filter_map(|(name, server)| server.enabled.then_some(name.clone()))
            .collect();
        self.chat_widget
            .set_mcp_startup_expected_servers(enabled_config_mcp_servers);
    }

    pub(super) async fn handle_app_server_event(
        &mut self,
        app_server_client: &mut AppServerSession,
        event: AppServerEvent,
    ) {
        match event {
            AppServerEvent::Lagged { skipped } => {
                tracing::warn!(
                    skipped,
                    "app-server event consumer lagged; dropping ignored events"
                );
                self.refresh_mcp_startup_expected_servers_from_config();
                self.chat_widget.finish_mcp_startup_after_lag();
            }
            AppServerEvent::ServerNotification(notification) => {
                self.handle_server_notification_event(app_server_client, notification)
                    .await;
            }
            AppServerEvent::ServerRequest(request) => {
                self.handle_server_request_event(app_server_client, request)
                    .await;
            }
            AppServerEvent::Disconnected { message } => {
                tracing::warn!("app-server event stream disconnected: {message}");
                self.chat_widget.add_error_message(message.clone());
                self.app_event_tx.send(AppEvent::FatalExitRequest(message));
            }
        }
    }

    async fn handle_server_notification_event(
        &mut self,
        app_server_client: &mut AppServerSession,
        notification: ServerNotification,
    ) {
        match &notification {
            ServerNotification::ServerRequestResolved(notification) => {
                if let Some(request) = self
                    .pending_app_server_requests
                    .resolve_notification(&notification.request_id)
                {
                    self.chat_widget.dismiss_app_server_request(&request);
                }
            }
            ServerNotification::McpServerStatusUpdated(_) => {
                self.refresh_mcp_startup_expected_servers_from_config();
            }
            ServerNotification::AccountRateLimitsUpdated(notification) => {
                self.chat_widget
                    .on_rolling_rate_limit_snapshot(notification.rate_limits.clone());
                return;
            }
            ServerNotification::AccountLoginCompleted(notification) => {
                self.handle_auth_profile_login_completed(notification);
                return;
            }
            ServerNotification::AccountUpdated(notification) => {
                self.chat_widget.update_account_state(
                    status_account_display_from_auth_mode(
                        notification.auth_mode,
                        notification.plan_type,
                    ),
                    notification.plan_type,
                    notification
                        .auth_mode
                        .is_some_and(AuthMode::has_chatgpt_account),
                );
                self.maybe_record_completed_auth_profile_login(notification);
                return;
            }
            ServerNotification::ExternalAgentConfigImportCompleted(_) => {
                let cwd = self.chat_widget.config_ref().cwd.to_path_buf();
                if let Err(err) = self.refresh_in_memory_config_from_disk().await {
                    tracing::warn!(
                        error = %err,
                        "failed to refresh config after external agent config import"
                    );
                }
                self.chat_widget.refresh_plugin_mentions();
                self.chat_widget.submit_op(AppCommand::reload_user_config());
                self.fetch_plugins_list(app_server_client, cwd);
                return;
            }
            ServerNotification::AppListUpdated(notification) => {
                self.chat_widget.on_connectors_loaded(
                    Ok(ConnectorsSnapshot {
                        connectors: notification.data.clone(),
                    }),
                    /*is_final*/ false,
                );
                return;
            }
            _ => {}
        }

        let auto_review_summary_target = match &notification {
            ServerNotification::BackgroundAutoReviewStatusChanged(notification)
                if notification.status == BackgroundAutoReviewStatus::Completed =>
            {
                ThreadId::from_string(&notification.thread_id)
                    .ok()
                    .map(|thread_id| (thread_id, notification.run_id.clone()))
            }
            _ => None,
        };

        match server_notification_thread_target(&notification) {
            ServerNotificationThreadTarget::Thread(thread_id) => {
                let result = if self.primary_thread_id == Some(thread_id)
                    || self.primary_thread_id.is_none()
                {
                    self.enqueue_primary_thread_notification(notification).await
                } else {
                    self.enqueue_thread_notification(thread_id, notification)
                        .await
                };

                if let Err(err) = result {
                    tracing::warn!("failed to enqueue app-server notification: {err}");
                }
                if let Some((summary_thread_id, run_id)) = auto_review_summary_target
                    && summary_thread_id == thread_id
                {
                    self.fetch_auto_review_summary(app_server_client, thread_id, run_id);
                }
                return;
            }
            ServerNotificationThreadTarget::InvalidThreadId(thread_id) => {
                tracing::warn!(
                    thread_id,
                    "ignoring app-server notification with invalid thread_id"
                );
                return;
            }
            ServerNotificationThreadTarget::Global => {}
        }

        self.chat_widget
            .handle_server_notification(notification, /*replay_kind*/ None);
    }

    async fn handle_server_request_event(
        &mut self,
        app_server_client: &AppServerSession,
        request: ServerRequest,
    ) {
        if let Some(unsupported) = self
            .pending_app_server_requests
            .note_server_request(&request)
        {
            tracing::warn!(
                request_id = ?unsupported.request_id,
                message = unsupported.message,
                "rejecting unsupported app-server request"
            );
            self.chat_widget
                .add_error_message(unsupported.message.clone());
            if let Err(err) = self
                .reject_app_server_request(
                    app_server_client,
                    unsupported.request_id,
                    unsupported.message,
                )
                .await
            {
                tracing::warn!("{err}");
            }
            return;
        }

        let Some(thread_id) = server_request_thread_id(&request) else {
            tracing::warn!("ignoring threadless app-server request");
            return;
        };

        let result =
            if self.primary_thread_id == Some(thread_id) || self.primary_thread_id.is_none() {
                self.enqueue_primary_thread_request(request).await
            } else {
                self.enqueue_thread_request(thread_id, request).await
            };
        if let Err(err) = result {
            tracing::warn!("failed to enqueue app-server request: {err}");
        }
    }

    fn handle_auth_profile_login_completed(
        &mut self,
        notification: &AccountLoginCompletedNotification,
    ) {
        let Some(login_id) = notification.login_id.as_deref() else {
            return;
        };
        let Some(pending) = self.pending_auth_profile_login.as_ref() else {
            return;
        };
        if pending.login_id != login_id {
            return;
        }
        if !notification.success {
            let pending = self.pending_auth_profile_login.take().unwrap();
            self.chat_widget.add_error_message(format!(
                "Login for auth profile {} failed.",
                pending.profile_label
            ));
            if let Some(error) = notification.error.as_ref() {
                self.chat_widget
                    .add_info_message(error.clone(), /*hint*/ None);
            }
        }
    }

    fn maybe_record_completed_auth_profile_login(
        &mut self,
        notification: &AccountUpdatedNotification,
    ) {
        if !notification
            .auth_mode
            .is_some_and(AuthMode::has_chatgpt_account)
        {
            return;
        }
        let Some(pending) = self.pending_auth_profile_login.take() else {
            return;
        };
        let auth_home =
            match codex_login::profile_home(&self.config.codex_home, &pending.profile_name) {
                Ok(auth_home) => auth_home,
                Err(err) => {
                    self.chat_widget.add_error_message(format!(
                        "Logged in, but failed to resolve auth profile {}: {err}",
                        pending.profile_label
                    ));
                    return;
                }
            };
        let (account_id, email) = match codex_login::load_auth_dot_json(
            &auth_home,
            self.config.cli_auth_credentials_store_mode,
        ) {
            Ok(Some(auth)) => (auth.account_id(), auth.account_email()),
            Ok(None) => (None, None),
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Logged in, but failed to read auth profile {}: {err}",
                    pending.profile_label
                ));
                return;
            }
        };

        if let Err(err) = codex_login::record_auth_profile_login(
            &self.config.codex_home,
            &pending.profile_name,
            account_id,
            email,
        ) {
            self.chat_widget.add_error_message(format!(
                "Logged in, but failed to update auth profile {}: {err}",
                pending.profile_label
            ));
            return;
        }

        self.chat_widget.add_info_message(
            format!("Logged in to auth profile {}.", pending.profile_label),
            /*hint*/ None,
        );
    }
}
