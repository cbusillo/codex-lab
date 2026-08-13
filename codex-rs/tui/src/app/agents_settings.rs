use super::*;
use codex_app_server_protocol::ExternalAgentCapabilitiesReadParams;
use codex_app_server_protocol::ExternalAgentCapabilitiesRefreshParams;
use codex_app_server_protocol::ExternalAgentCapabilitiesRefreshStatus;
use codex_app_server_protocol::ExternalAgentCapabilitiesUpdatedNotification;
use codex_app_server_protocol::ExternalAgentProviderCapabilities;
use uuid::Uuid;

#[derive(Default)]
pub(super) struct AgentSettingsState {
    providers: Vec<ExternalAgentProviderCapabilities>,
    refresh_id: Option<String>,
    refresh_status: Option<ExternalAgentCapabilitiesRefreshStatus>,
    cancel_pending: bool,
    loading: bool,
    open: bool,
}

impl AgentSettingsState {
    fn apply_update(&mut self, notification: ExternalAgentCapabilitiesUpdatedNotification) -> bool {
        if self.refresh_id.as_deref() != Some(&notification.refresh_id) {
            return false;
        }
        self.refresh_id = None;
        self.cancel_pending = false;
        self.refresh_status = Some(notification.status);
        self.providers = notification.providers;
        true
    }
}

impl App {
    pub(super) async fn open_agents_settings(&mut self, app_server: &mut AppServerSession) {
        if !self.agent_settings.open {
            self.agent_settings.refresh_status = None;
        }
        self.agent_settings.open = true;
        self.agent_settings.loading = true;
        self.render_agents_settings();
        let params = ExternalAgentCapabilitiesReadParams {
            cwd: Some(self.config.cwd.to_string_lossy().into_owned()),
            families: None,
            refresh: None,
        };
        match app_server.external_agent_capabilities_read(params).await {
            Ok(response) => {
                self.agent_settings.loading = false;
                self.agent_settings.providers = response.providers;
                self.render_agents_settings();
            }
            Err(err) => {
                self.agent_settings.loading = false;
                self.agent_settings.open = false;
                tracing::warn!(error = %err, "failed to read external-agent capabilities");
                self.chat_widget
                    .add_error_message(format!("Could not load live agent capabilities: {err}"));
                self.chat_widget.open_agents_settings_popup();
            }
        }
    }

    pub(super) async fn refresh_agent_capabilities(&mut self, app_server: &mut AppServerSession) {
        if self.agent_settings.refresh_id.is_some() {
            return;
        }
        let refresh_id = Uuid::new_v4().to_string();
        let params = ExternalAgentCapabilitiesReadParams {
            cwd: Some(self.config.cwd.to_string_lossy().into_owned()),
            families: None,
            refresh: Some(ExternalAgentCapabilitiesRefreshParams {
                refresh_id: refresh_id.clone(),
            }),
        };
        match app_server.external_agent_capabilities_read(params).await {
            Ok(response) => {
                self.agent_settings.providers = response.providers;
                self.agent_settings.refresh_id = response.refresh.map(|refresh| refresh.refresh_id);
                self.agent_settings.refresh_status = None;
                self.agent_settings.cancel_pending = false;
                if self.agent_settings.refresh_id.is_none() {
                    self.chat_widget.add_error_message(
                        "Agent capability refresh is busy; try again shortly.".to_string(),
                    );
                }
                self.render_agents_settings();
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to start external-agent capability refresh");
                self.chat_widget
                    .add_error_message(format!("Could not refresh agent capabilities: {err}"));
            }
        }
    }

    pub(super) async fn cancel_agent_capabilities_refresh(
        &mut self,
        app_server: &mut AppServerSession,
    ) {
        let Some(refresh_id) = self.agent_settings.refresh_id.clone() else {
            return;
        };
        match app_server
            .external_agent_capabilities_refresh_cancel(refresh_id)
            .await
        {
            Ok(response) if response.cancelled => {
                self.agent_settings.cancel_pending = true;
                self.render_agents_settings();
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(error = %err, "failed to cancel external-agent capability refresh");
                self.chat_widget.add_error_message(format!(
                    "Could not cancel the agent capability refresh: {err}"
                ));
            }
        }
    }

    pub(super) async fn close_agents_settings(&mut self, app_server: &mut AppServerSession) {
        self.agent_settings.open = false;
        self.agent_settings.loading = false;
        self.agent_settings.cancel_pending = false;
        self.agent_settings.refresh_status = None;
        if let Some(refresh_id) = self.agent_settings.refresh_id.take() {
            let _ = app_server
                .external_agent_capabilities_refresh_cancel(refresh_id)
                .await;
        }
    }

    pub(super) fn handle_agent_capabilities_updated(
        &mut self,
        notification: ExternalAgentCapabilitiesUpdatedNotification,
    ) {
        if !self.agent_settings.apply_update(notification) {
            return;
        }
        if self.agent_settings.open {
            self.render_agents_settings();
        }
    }

    pub(super) async fn update_agent_selector_model(
        &mut self,
        app_server: &mut AppServerSession,
        selector: String,
        model: Option<String>,
    ) {
        let edit = crate::config_update::agent_selector_model_edit(&selector, model.as_deref());
        let response =
            match crate::config_update::write_config_batch(app_server.request_handle(), vec![edit])
                .await
            {
                Ok(response) => response,
                Err(err) => {
                    self.chat_widget.add_error_message(format!(
                        "Failed to save `{selector}` model default: {err}"
                    ));
                    return;
                }
            };
        if response.status == WriteStatus::OkOverridden {
            self.chat_widget.add_error_message(format!(
                "`{selector}` model default was saved but a higher-precedence setting remains effective."
            ));
            self.open_agents_settings(app_server).await;
            return;
        }
        self.config
            .agent_selector_overrides
            .entry(selector.clone())
            .or_default()
            .model = model.clone();
        self.chat_widget.set_agent_selector_model(&selector, model);
        self.open_agents_settings(app_server).await;
    }

    pub(super) async fn update_agent_selector_effort(
        &mut self,
        app_server: &mut AppServerSession,
        selector: String,
        effort: Option<String>,
    ) {
        let edit = crate::config_update::agent_selector_effort_edit(&selector, effort.as_deref());
        let response =
            match crate::config_update::write_config_batch(app_server.request_handle(), vec![edit])
                .await
            {
                Ok(response) => response,
                Err(err) => {
                    self.chat_widget.add_error_message(format!(
                        "Failed to save `{selector}` effort default: {err}"
                    ));
                    return;
                }
            };
        if response.status == WriteStatus::OkOverridden {
            self.chat_widget.add_error_message(format!(
                "`{selector}` effort default was saved but a higher-precedence setting remains effective."
            ));
            self.open_agents_settings(app_server).await;
            return;
        }
        self.config
            .agent_selector_overrides
            .entry(selector.clone())
            .or_default()
            .effort = effort.clone();
        self.chat_widget
            .set_agent_selector_effort(&selector, effort);
        self.open_agents_settings(app_server).await;
    }

    fn render_agents_settings(&mut self) {
        self.chat_widget.open_agents_capabilities_popup(
            &self.agent_settings.providers,
            self.agent_settings.loading,
            self.agent_settings.refresh_id.is_some(),
            self.agent_settings.cancel_pending,
            self.agent_settings.refresh_status,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::make_test_app;
    use crate::start_embedded_app_server_for_picker;
    use codex_app_server_client::AppServerEvent;
    use codex_app_server_protocol::ServerNotification;

    #[test]
    fn capability_updates_ignore_stale_refresh_ids() {
        let mut state = AgentSettingsState {
            refresh_id: Some("current".to_string()),
            ..Default::default()
        };
        assert!(
            !state.apply_update(ExternalAgentCapabilitiesUpdatedNotification {
                refresh_id: "stale".to_string(),
                status: ExternalAgentCapabilitiesRefreshStatus::Completed,
                providers: Vec::new(),
            })
        );
        assert_eq!(state.refresh_id.as_deref(), Some("current"));
        assert!(state.refresh_status.is_none());
    }

    #[test]
    fn capability_updates_finish_the_matching_refresh() {
        let mut state = AgentSettingsState {
            refresh_id: Some("current".to_string()),
            ..Default::default()
        };
        assert!(
            state.apply_update(ExternalAgentCapabilitiesUpdatedNotification {
                refresh_id: "current".to_string(),
                status: ExternalAgentCapabilitiesRefreshStatus::DeadlineExceeded,
                providers: Vec::new(),
            })
        );
        assert!(state.refresh_id.is_none());
        assert_eq!(
            state.refresh_status,
            Some(ExternalAgentCapabilitiesRefreshStatus::DeadlineExceeded)
        );
    }

    #[test]
    fn capability_updates_finish_a_pending_cancellation() {
        let mut state = AgentSettingsState {
            refresh_id: Some("current".to_string()),
            cancel_pending: true,
            ..Default::default()
        };
        assert!(
            state.apply_update(ExternalAgentCapabilitiesUpdatedNotification {
                refresh_id: "current".to_string(),
                status: ExternalAgentCapabilitiesRefreshStatus::Cancelled,
                providers: Vec::new(),
            })
        );
        assert!(state.refresh_id.is_none());
        assert!(!state.cancel_pending);
        assert_eq!(
            state.refresh_status,
            Some(ExternalAgentCapabilitiesRefreshStatus::Cancelled)
        );
    }

    #[tokio::test]
    async fn app_server_capability_update_finishes_the_matching_refresh() {
        let mut app = Box::pin(make_test_app()).await;
        let app_server = start_embedded_app_server_for_picker(app.chat_widget.config_ref())
            .await
            .expect("embedded app server");
        app.agent_settings.refresh_id = Some("current".to_string());
        app.agent_settings.cancel_pending = true;

        app.handle_app_server_event(
            &app_server,
            AppServerEvent::ServerNotification(Box::new(
                ServerNotification::ExternalAgentCapabilitiesUpdated(
                    ExternalAgentCapabilitiesUpdatedNotification {
                        refresh_id: "current".to_string(),
                        status: ExternalAgentCapabilitiesRefreshStatus::Completed,
                        providers: Vec::new(),
                    },
                ),
            )),
        )
        .await;

        assert!(app.agent_settings.refresh_id.is_none());
        assert!(!app.agent_settings.cancel_pending);
        assert_eq!(
            app.agent_settings.refresh_status,
            Some(ExternalAgentCapabilitiesRefreshStatus::Completed)
        );
    }
}
