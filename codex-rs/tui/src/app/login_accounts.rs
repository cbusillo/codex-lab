use super::PendingDirectLoginAddAccount;
use super::PendingDirectLoginAddAccountCancellation;
use super::PendingDirectLoginAddAccountKind;
use super::*;
use crate::app_event::SecretDeviceCode;
use crate::bottom_pane::LoginAccountsFeedback;
use crate::bottom_pane::LoginAddAccountState;
use crate::onboarding::maybe_open_auth_url_in_browser;
use codex_app_server_protocol::Account;
use codex_app_server_protocol::AccountHealth;
use codex_app_server_protocol::AccountListEntry;
use codex_app_server_protocol::AuthMode;
use codex_app_server_protocol::CancelLoginAccountParams;
use codex_app_server_protocol::CancelLoginAccountResponse;
use codex_app_server_protocol::LoginAccountParams;
use codex_app_server_protocol::LoginAccountResponse;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::CLIENT_ID;
use codex_login::ServerOptions;
use tokio_util::sync::CancellationToken;

fn current_account_list_entry(account: Account, account_id: Option<String>) -> AccountListEntry {
    let (auth_mode, label) = match account {
        Account::ApiKey {} => (AuthMode::ApiKey, Some("API key".to_string())),
        Account::Chatgpt { email, .. } => (AuthMode::Chatgpt, email),
        Account::AmazonBedrock { .. } => {
            (AuthMode::BedrockApiKey, Some("Amazon Bedrock".to_string()))
        }
    };
    AccountListEntry {
        account_id: account_id.unwrap_or_else(|| "current".to_string()),
        auth_mode,
        health: AccountHealth::Ok,
        label,
        created_at: None,
        last_used_at: None,
        is_active: true,
    }
}

impl App {
    fn use_default_store_login(&self) -> bool {
        false
    }

    fn preserve_existing_app_server_account(&self) -> bool {
        !matches!(self.app_server_target, crate::AppServerTarget::Embedded)
            || self.config.cli_auth_credentials_store_mode == AuthCredentialsStoreMode::File
    }

    pub(super) async fn show_login_accounts_view(&mut self, app_server: &mut AppServerSession) {
        if matches!(self.app_server_target, crate::AppServerTarget::Embedded) {
            self.chat_widget
                .show_login_accounts_view_with_feedback(/*feedback*/ None);
            return;
        }

        match app_server.list_accounts().await {
            Ok(response) => {
                let mut accounts = response.accounts;
                if !accounts.iter().any(|account| account.is_active)
                    && let Ok(current) = app_server.read_account().await
                    && let Some(account) = current.account
                {
                    accounts.push(current_account_list_entry(
                        account,
                        response.active_account_id,
                    ));
                }
                self.chat_widget
                    .show_login_accounts_view_with_loaded_accounts(
                        accounts, /*feedback*/ None,
                    );
            }
            Err(err) => self
                .chat_widget
                .show_login_accounts_view_with_loaded_accounts(
                    Vec::new(),
                    Some(LoginAccountsFeedback::Error(format!(
                        "Failed to read accounts from app server: {err}"
                    ))),
                ),
        }
    }

    pub(super) async fn start_login_add_account_chatgpt(
        &mut self,
        app_server: &mut AppServerSession,
    ) {
        if let Err(err) = self.cancel_login_add_account_chatgpt(app_server).await {
            self.chat_widget
                .update_login_add_account_view(LoginAddAccountState::Failed(err));
            return;
        }
        if !self
            .chat_widget
            .update_login_add_account_view(LoginAddAccountState::Starting)
        {
            return;
        }

        if self.use_default_store_login() {
            self.start_default_store_login_add_account_chatgpt().await;
            return;
        }

        let request_handle = app_server.request_handle();
        let response = request_handle
            .request_typed::<LoginAccountResponse>(ClientRequest::LoginAccount {
                request_id: app_server.next_request_id(),
                params: LoginAccountParams::Chatgpt {
                    codex_streamlined_login: false,
                    use_hosted_login_success_page: false,
                    app_brand: None,
                    preserve_existing_account: self.preserve_existing_app_server_account(),
                },
            })
            .await;

        match response {
            Ok(LoginAccountResponse::Chatgpt { login_id, auth_url }) => {
                self.pending_login_add_account_id = Some(login_id.clone());
                self.completed_login_add_account_id = None;
                maybe_open_auth_url_in_browser(&request_handle, &auth_url);
                if !self
                    .chat_widget
                    .update_login_add_account_view(LoginAddAccountState::Waiting {
                        login_id,
                        auth_url,
                    })
                    && let Err(err) = self.cancel_login_add_account_chatgpt(app_server).await
                {
                    tracing::warn!("{err}");
                }
            }
            Ok(other) => {
                self.chat_widget
                    .update_login_add_account_view(LoginAddAccountState::Failed(format!(
                        "Unexpected login response: {other:?}"
                    )));
            }
            Err(err) => {
                self.chat_widget
                    .update_login_add_account_view(LoginAddAccountState::Failed(format!(
                        "Failed to start ChatGPT login: {err}"
                    )));
            }
        }
    }

    async fn start_default_store_login_add_account_chatgpt(&mut self) {
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

    pub(super) async fn save_login_add_account_api_key(
        &mut self,
        app_server: &mut AppServerSession,
        api_key: &str,
    ) {
        if let Err(err) = self.cancel_login_add_account_chatgpt(app_server).await {
            self.chat_widget
                .update_login_add_account_view(LoginAddAccountState::ApiKeyFailed(err));
            return;
        }
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

        if self.use_default_store_login() {
            self.save_default_store_login_add_account_api_key(trimmed_key);
            return;
        }

        let response = app_server
            .request_handle()
            .request_typed::<LoginAccountResponse>(ClientRequest::LoginAccount {
                request_id: app_server.next_request_id(),
                params: LoginAccountParams::ApiKey {
                    api_key: trimmed_key.to_string(),
                },
            })
            .await;
        let state = match response {
            Ok(LoginAccountResponse::ApiKey {}) => LoginAddAccountState::Complete,
            Ok(other) => {
                LoginAddAccountState::ApiKeyFailed(format!("Unexpected login response: {other:?}"))
            }
            Err(err) => {
                LoginAddAccountState::ApiKeyFailed(format!("Failed to store API key: {err}"))
            }
        };
        self.chat_widget.update_login_add_account_view(state);
    }

    fn save_default_store_login_add_account_api_key(&mut self, trimmed_key: &str) {
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

    pub(super) async fn start_login_add_account_device_code(
        &mut self,
        app_server: &mut AppServerSession,
    ) {
        if let Err(err) = self.cancel_login_add_account_chatgpt(app_server).await {
            self.chat_widget
                .update_login_add_account_view(LoginAddAccountState::DeviceCodeFailed(err));
            return;
        }
        if !self
            .chat_widget
            .update_login_add_account_view(LoginAddAccountState::DeviceCodeStarting)
        {
            return;
        }

        if self.use_default_store_login() {
            self.start_default_store_login_add_account_device_code();
            return;
        }

        let response = app_server
            .request_handle()
            .request_typed::<LoginAccountResponse>(ClientRequest::LoginAccount {
                request_id: app_server.next_request_id(),
                params: LoginAccountParams::ChatgptDeviceCode {
                    preserve_existing_account: self.preserve_existing_app_server_account(),
                },
            })
            .await;

        match response {
            Ok(LoginAccountResponse::ChatgptDeviceCode {
                login_id,
                verification_url,
                user_code,
            }) => {
                self.pending_login_add_account_id = Some(login_id.clone());
                self.completed_login_add_account_id = None;
                if !self.chat_widget.update_login_add_account_view(
                    LoginAddAccountState::DeviceCodeWaiting {
                        login_id: login_id.clone(),
                        verification_url,
                        user_code,
                    },
                ) && let Err(err) = self.cancel_login_add_account_chatgpt(app_server).await
                {
                    tracing::warn!(login_id, "{err}");
                }
            }
            Ok(other) => {
                self.chat_widget.update_login_add_account_view(
                    LoginAddAccountState::DeviceCodeFailed(format!(
                        "Unexpected login response: {other:?}"
                    )),
                );
            }
            Err(err) => {
                self.chat_widget.update_login_add_account_view(
                    LoginAddAccountState::DeviceCodeFailed(format!(
                        "Failed to start code login: {err}"
                    )),
                );
            }
        }
    }

    fn start_default_store_login_add_account_device_code(&mut self) {
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

    pub(super) async fn cancel_login_add_account_chatgpt(
        &mut self,
        app_server: &mut AppServerSession,
    ) -> Result<(), String> {
        if let Some(pending) = self.pending_direct_login_add_account.take() {
            pending.cancellation.cancel();
            return Ok(());
        }

        let Some(login_id) = self.pending_login_add_account_id.clone() else {
            return Ok(());
        };
        let request_handle = app_server.request_handle();
        request_handle
            .request_typed::<CancelLoginAccountResponse>(ClientRequest::CancelLoginAccount {
                request_id: app_server.next_request_id(),
                params: CancelLoginAccountParams { login_id },
            })
            .await
            .map_err(|err| format!("Failed to cancel add-account ChatGPT login: {err}"))?;
        self.pending_login_add_account_id = None;
        self.completed_login_add_account_id = None;
        Ok(())
    }

    pub(super) fn cancel_direct_login_add_account(&mut self) {
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
