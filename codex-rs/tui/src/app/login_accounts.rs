use super::PendingDirectLoginAddAccount;
use super::PendingDirectLoginAddAccountCancellation;
use super::PendingDirectLoginAddAccountKind;
use super::*;
use crate::app_event::SecretDeviceCode;
use crate::bottom_pane::LoginAccountsFeedback;
use crate::bottom_pane::LoginAddAccountState;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::CLIENT_ID;
use codex_login::ServerOptions;
use tokio_util::sync::CancellationToken;

impl App {
    pub(super) fn show_login_accounts_view(&mut self) {
        self.chat_widget
            .show_login_accounts_view_with_feedback(None);
    }

    pub(super) fn show_login_accounts_feedback(&mut self, feedback: LoginAccountsFeedback) {
        self.chat_widget
            .show_login_accounts_view_with_feedback(Some(feedback));
    }

    pub(super) async fn start_login_add_account_chatgpt(&mut self) {
        self.cancel_login_add_account_chatgpt();
        if !self
            .chat_widget
            .update_login_add_account_view(LoginAddAccountState::Starting)
        {
            return;
        }

        let opts = ServerOptions {
            open_browser: false,
            ..self.default_store_login_options()
        };
        let server = match codex_login::run_login_server(opts) {
            Ok(server) => server,
            Err(err) => {
                self.chat_widget
                    .update_login_add_account_view(LoginAddAccountState::Failed(format!(
                        "Failed to start ChatGPT login: {err}"
                    )));
                return;
            }
        };

        let auth_url = server.auth_url.clone();
        let shutdown = server.cancel_handle();
        let completion_tx = self.app_event_tx.clone();
        self.direct_login_add_account_attempt_id =
            self.direct_login_add_account_attempt_id.wrapping_add(1);
        let attempt_id = self.direct_login_add_account_attempt_id;
        tokio::spawn(async move {
            let result = server
                .block_until_done()
                .await
                .map_err(|err| err.to_string());
            completion_tx.send(AppEvent::LoginAddAccountChatGptCompleted { attempt_id, result });
        });

        self.pending_direct_login_add_account = Some(PendingDirectLoginAddAccount {
            attempt_id,
            cancellation: PendingDirectLoginAddAccountCancellation::Browser(shutdown),
        });
        self.open_url_in_browser(auth_url.clone());
        if !self
            .chat_widget
            .update_login_add_account_view(LoginAddAccountState::Waiting {
                login_id: "default-store".to_string(),
                auth_url,
            })
            && let Some(pending) = self.pending_direct_login_add_account.take()
        {
            pending.cancellation.cancel();
        }
    }

    pub(super) fn save_login_add_account_api_key(&mut self, api_key: &str) {
        let trimmed_key = api_key.trim();
        if trimmed_key.is_empty() {
            self.chat_widget
                .update_login_add_account_view(LoginAddAccountState::ApiKeyFailed(
                    "API key cannot be empty".to_string(),
                ));
            return;
        }

        self.chat_widget
            .update_login_add_account_view(LoginAddAccountState::SavingApiKey);
        let state = match codex_login::login_with_api_key(
            &self.config.codex_home,
            trimmed_key,
            self.config.cli_auth_credentials_store_mode,
            self.config.auth_keyring_backend_kind(),
        ) {
            Ok(()) => LoginAddAccountState::Complete,
            Err(err) => {
                LoginAddAccountState::ApiKeyFailed(format!("Failed to store API key: {err}"))
            }
        };
        self.chat_widget.update_login_add_account_view(state);
    }

    pub(super) fn start_login_add_account_device_code(&mut self) {
        self.cancel_login_add_account_chatgpt();
        if !self
            .chat_widget
            .update_login_add_account_view(LoginAddAccountState::DeviceCodeStarting)
        {
            return;
        }

        let opts = self.default_store_login_options();
        self.direct_login_add_account_attempt_id =
            self.direct_login_add_account_attempt_id.wrapping_add(1);
        let attempt_id = self.direct_login_add_account_attempt_id;
        let completion_tx = self.app_event_tx.clone();
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        self.pending_direct_login_add_account = Some(PendingDirectLoginAddAccount {
            attempt_id,
            cancellation: PendingDirectLoginAddAccountCancellation::DeviceCode(cancellation),
        });
        tokio::spawn(async move {
            let device_code = tokio::select! {
                _ = task_cancellation.cancelled() => return,
                result = codex_login::request_device_code(&opts) => match result {
                    Ok(device_code) => device_code,
                    Err(err) => {
                        completion_tx.send(AppEvent::LoginAddAccountChatGptCompleted {
                            attempt_id,
                            result: Err(format!("Failed to start code login: {err}")),
                        });
                        return;
                    }
                },
            };
            completion_tx.send(AppEvent::LoginAddAccountDeviceCodeReady {
                attempt_id,
                verification_url: device_code.verification_url.clone(),
                user_code: SecretDeviceCode::new(device_code.user_code.clone()),
            });
            let result = tokio::select! {
                _ = task_cancellation.cancelled() => Err("Login was not completed".to_string()),
                result = codex_login::complete_device_code_login(opts, device_code) => {
                    result.map_err(|err| err.to_string())
                }
            };
            completion_tx.send(AppEvent::LoginAddAccountChatGptCompleted { attempt_id, result });
        });
    }

    pub(super) fn show_login_add_account_device_code(
        &mut self,
        attempt_id: u64,
        verification_url: String,
        user_code: String,
    ) {
        if self
            .pending_direct_login_add_account
            .as_ref()
            .map(|pending| pending.attempt_id)
            != Some(attempt_id)
        {
            return;
        }

        let login_id = format!("default-device-code-{attempt_id}");
        if !self.chat_widget.update_login_add_account_view(
            LoginAddAccountState::DeviceCodeWaiting {
                login_id,
                verification_url,
                user_code,
            },
        ) && let Some(pending) = self.pending_direct_login_add_account.take()
        {
            pending.cancellation.cancel();
        }
    }

    pub(super) fn cancel_login_add_account_chatgpt(&mut self) {
        if let Some(pending) = self.pending_direct_login_add_account.take() {
            pending.cancellation.cancel();
        }
    }

    pub(super) fn complete_login_add_account_chatgpt(
        &mut self,
        attempt_id: u64,
        result: Result<(), String>,
    ) {
        if self
            .pending_direct_login_add_account
            .as_ref()
            .map(|pending| pending.attempt_id)
            != Some(attempt_id)
        {
            return;
        }

        let Some(pending) = self.pending_direct_login_add_account.take() else {
            return;
        };
        let login_kind = pending.cancellation.kind();
        let (state, feedback) = match result {
            Ok(()) => (
                LoginAddAccountState::Complete,
                LoginAccountsFeedback::Info("Login added successfully.".to_string()),
            ),
            Err(err) => {
                let message = format!("ChatGPT login did not complete: {err}");
                let state = match login_kind {
                    PendingDirectLoginAddAccountKind::Browser => {
                        LoginAddAccountState::Failed(message.clone())
                    }
                    PendingDirectLoginAddAccountKind::DeviceCode => {
                        LoginAddAccountState::DeviceCodeFailed(message.clone())
                    }
                };
                (state, LoginAccountsFeedback::Error(message))
            }
        };
        if !self.chat_widget.update_login_add_account_view(state) {
            self.chat_widget
                .show_login_accounts_view_with_feedback(Some(feedback));
        }
    }

    fn default_store_login_options(&self) -> ServerOptions {
        if self.config.cli_auth_credentials_store_mode == AuthCredentialsStoreMode::File {
            ServerOptions::new_for_add_account(
                self.config.codex_home.to_path_buf(),
                CLIENT_ID.to_string(),
                self.config.forced_chatgpt_workspace_id.clone(),
                self.config.cli_auth_credentials_store_mode,
                self.config.auth_keyring_backend_kind(),
                self.config.auth_route_config(),
            )
        } else {
            ServerOptions::new(
                self.config.codex_home.to_path_buf(),
                CLIENT_ID.to_string(),
                self.config.forced_chatgpt_workspace_id.clone(),
                self.config.cli_auth_credentials_store_mode,
                self.config.auth_keyring_backend_kind(),
                self.config.auth_route_config(),
            )
        }
    }
}

#[cfg(test)]
#[path = "login_accounts_tests.rs"]
mod tests;
