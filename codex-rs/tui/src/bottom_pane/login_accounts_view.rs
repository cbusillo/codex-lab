use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AccountHealth as ApiAccountHealth;
use codex_app_server_protocol::AccountListEntry;
use codex_app_server_protocol::AuthMode as ApiAuthMode;
use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::AuthKeyringBackendKind;
use codex_login::StoredAccount;
use codex_protocol::auth::AuthMode;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Alignment;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
use std::path::Path;
use std::path::PathBuf;
use textwrap::wrap;

use crate::account_label::account_display_label;
use crate::app_event::AppEvent;
use crate::app_event::AuthAccountSelection;
use crate::app_event::RemoveAuthAccountSelection;
use crate::app_event::SecretApiKey;
use crate::app_event_sender::AppEventSender;
use crate::render::renderable::Renderable;

use super::BottomPaneView;
use super::ViewCompletion;

#[path = "login_accounts_view/account_pool.rs"]
mod account_pool;

use account_pool::AccountPoolBehavior;
use account_pool::CURRENT_AUTH_ACCOUNT_ID;
use account_pool::CURRENT_ONLY_POOL_NOTICE;
use account_pool::account_matches_auth;
use account_pool::auth_mode_to_api;
use account_pool::current_auth_account_row;

pub(crate) const LOGIN_ACCOUNTS_VIEW_ID: &str = "login-accounts";
pub(crate) const LOGIN_ADD_ACCOUNT_VIEW_ID: &str = "login-add-account";

/// Interactive view shown for `/login` to manage stored accounts.
pub(crate) struct LoginAccountsView {
    app_event_tx: AppEventSender,
    codex_home: PathBuf,
    default_auth_home_is_current: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_keyring_backend_kind: AuthKeyringBackendKind,
    pool_behavior: AccountPoolBehavior,
    accounts: Vec<AccountRow>,
    remote_loaded: bool,
    selected: usize,
    error: Option<String>,
    feedback: Option<LoginAccountsFeedback>,
    mode: LoginAccountsMode,
    is_complete: bool,
    completion: Option<ViewCompletion>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AccountRow {
    id: String,
    label: String,
    detail: Option<String>,
    mode: ApiAuthMode,
    health: ApiAccountHealth,
    is_active: bool,
    is_pooled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LoginAccountsFeedback {
    Info(String),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LoginAccountsMode {
    List,
    ConfirmRemove { account_id: String, label: String },
}

impl LoginAccountsView {
    pub(crate) fn new_with_feedback(
        codex_home: &Path,
        app_event_tx: AppEventSender,
        default_auth_home_is_current: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        auth_keyring_backend_kind: AuthKeyringBackendKind,
        feedback: Option<LoginAccountsFeedback>,
    ) -> Self {
        let mut loaded = load_account_rows(
            codex_home,
            default_auth_home_is_current,
            auth_credentials_store_mode,
            auth_keyring_backend_kind,
            /*previously_selected_id*/ None,
        );

        Self {
            app_event_tx,
            codex_home: codex_home.to_path_buf(),
            default_auth_home_is_current,
            auth_credentials_store_mode,
            auth_keyring_backend_kind,
            pool_behavior: AccountPoolBehavior::for_store_mode(auth_credentials_store_mode),
            accounts: loaded.accounts,
            remote_loaded: false,
            selected: loaded.selected,
            error: loaded.error.take(),
            feedback,
            mode: LoginAccountsMode::List,
            is_complete: false,
            completion: None,
        }
    }

    pub(crate) fn new_with_loaded_accounts(
        app_event_tx: AppEventSender,
        accounts: Vec<AccountListEntry>,
        feedback: Option<LoginAccountsFeedback>,
    ) -> Self {
        let mut rows = accounts
            .into_iter()
            .map(AccountRow::from_list_entry)
            .collect::<Vec<_>>();
        sort_account_rows(&mut rows);
        let selected = rows
            .iter()
            .position(|account| account.is_active)
            .unwrap_or(0);

        Self {
            app_event_tx,
            codex_home: PathBuf::new(),
            default_auth_home_is_current: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::default(),
            auth_keyring_backend_kind: AuthKeyringBackendKind::default(),
            pool_behavior: AccountPoolBehavior::Pooled,
            accounts: rows,
            remote_loaded: true,
            selected,
            error: None,
            feedback,
            mode: LoginAccountsMode::List,
            is_complete: false,
            completion: None,
        }
    }

    fn account_count(&self) -> usize {
        self.accounts.len()
    }

    fn add_row_index(&self) -> usize {
        self.account_count()
    }

    fn selected_account(&self) -> Option<&AccountRow> {
        self.accounts.get(self.selected)
    }

    fn select_previous(&mut self) {
        let max = self.add_row_index();
        if self.selected == 0 {
            self.selected = max;
        } else {
            self.selected -= 1;
        }
    }

    fn select_next(&mut self) {
        let max = self.add_row_index();
        if self.selected >= max {
            self.selected = 0;
        } else {
            self.selected += 1;
        }
    }

    fn finish(&mut self, completion: ViewCompletion) {
        self.is_complete = true;
        self.completion = Some(completion);
    }

    fn handle_enter(&mut self) {
        if let LoginAccountsMode::ConfirmRemove { account_id, label } = self.mode.clone() {
            self.app_event_tx.send(AppEvent::RemoveAuthAccount {
                selection: RemoveAuthAccountSelection { account_id, label },
            });
            self.finish(ViewCompletion::Accepted);
            return;
        }

        if self.selected == self.add_row_index() {
            self.finish(ViewCompletion::Accepted);
            self.app_event_tx.send(AppEvent::ShowLoginAddAccount);
            return;
        }

        let Some(account) = self.selected_account() else {
            return;
        };
        if account.health == ApiAccountHealth::ReauthRequired {
            self.app_event_tx.send(AppEvent::ShowLoginAddAccount);
            self.finish(ViewCompletion::Accepted);
            return;
        }
        if account.is_active || !account.activation_supported() {
            return;
        }

        self.app_event_tx.send(AppEvent::SwitchAuthAccount {
            selection: AuthAccountSelection {
                account_id: account.id.clone(),
                label: account.label.clone(),
            },
        });
        self.finish(ViewCompletion::Accepted);
    }

    fn handle_disconnect(&mut self) {
        let Some(account) = self.selected_account() else {
            return;
        };
        if !account.is_pooled {
            self.feedback = Some(LoginAccountsFeedback::Info(
                "This login is not pooled. Use /logout to disconnect it.".to_string(),
            ));
            return;
        }
        self.mode = LoginAccountsMode::ConfirmRemove {
            account_id: account.id.clone(),
            label: account.label.clone(),
        };
    }

    fn reload_accounts(&mut self) {
        if self.remote_loaded {
            self.finish(ViewCompletion::Accepted);
            self.app_event_tx.send(AppEvent::ShowLoginAccounts);
            return;
        }

        let previously_selected_id = self
            .accounts
            .get(self.selected)
            .map(|account| account.id.clone());
        let loaded = load_account_rows(
            &self.codex_home,
            self.default_auth_home_is_current,
            self.auth_credentials_store_mode,
            self.auth_keyring_backend_kind,
            previously_selected_id,
        );
        self.accounts = loaded.accounts;
        self.selected = loaded.selected;
        self.error = loaded.error;
        self.feedback = None;
    }

    fn cancel_confirm_remove(&mut self) {
        self.mode = LoginAccountsMode::List;
    }

    fn wrapped_line_count(message: &str, width: u16) -> usize {
        let width = width.max(1) as usize;
        wrap(message, width).len().max(1)
    }

    fn content_line_count(&self, width: u16) -> usize {
        let content_width = width.saturating_sub(4).max(1);
        let mut lines = 0;
        if let Some(error) = &self.error {
            lines += Self::wrapped_line_count(error, content_width) + 1;
        }
        if let Some(feedback) = &self.feedback {
            let message = match feedback {
                LoginAccountsFeedback::Info(message) | LoginAccountsFeedback::Error(message) => {
                    message
                }
            };
            lines += Self::wrapped_line_count(message, content_width) + 1;
        }
        lines += 2;
        if self.pool_behavior == AccountPoolBehavior::CurrentOnly {
            lines += Self::wrapped_line_count(CURRENT_ONLY_POOL_NOTICE, content_width) + 1;
        }
        lines += self.accounts.len().max(1);
        lines += 3;
        let hint = "up/down Navigate  Enter Select  d Disconnect  Esc Close";
        lines += Self::wrapped_line_count(hint, content_width);
        if let LoginAccountsMode::ConfirmRemove { label, .. } = &self.mode {
            let confirmation = format!("Disconnect {label}?");
            lines += 1;
            lines += Self::wrapped_line_count(&confirmation, content_width);
            lines += Self::wrapped_line_count(
                "Press Enter to disconnect or Esc to cancel.",
                content_width,
            );
        }
        lines
    }
}

impl BottomPaneView for LoginAccountsView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if matches!(self.mode, LoginAccountsMode::ConfirmRemove { .. }) {
            match key_event.code {
                KeyCode::Esc | KeyCode::Char('n') => self.cancel_confirm_remove(),
                KeyCode::Enter | KeyCode::Char('y') => self.handle_enter(),
                _ => {}
            }
            return;
        }

        match key_event.code {
            KeyCode::Esc | KeyCode::Char('q') => self.finish(ViewCompletion::Cancelled),
            KeyCode::Up => self.select_previous(),
            KeyCode::Down => self.select_next(),
            KeyCode::Char('d') => self.handle_disconnect(),
            KeyCode::Char('r') => self.reload_accounts(),
            KeyCode::Enter => self.handle_enter(),
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.is_complete
    }

    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }

    fn view_id(&self) -> Option<&'static str> {
        Some(LOGIN_ACCOUNTS_VIEW_ID)
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LoginAddAccountState {
    Choose,
    ApiKey {
        value: String,
        error: Option<String>,
    },
    SavingApiKey,
    Starting,
    Waiting {
        login_id: String,
        auth_url: String,
    },
    DeviceCodeStarting,
    DeviceCodeWaiting {
        login_id: String,
        verification_url: String,
        user_code: String,
    },
    DeviceCodeFailed(String),
    ApiKeyFailed(String),
    Failed(String),
    Complete,
}

pub(crate) struct LoginAddAccountView {
    app_event_tx: AppEventSender,
    state: LoginAddAccountState,
    selected: usize,
    is_complete: bool,
    completion: Option<ViewCompletion>,
    pool_behavior: AccountPoolBehavior,
}

impl LoginAddAccountView {
    #[cfg(test)]
    pub(crate) fn new(app_event_tx: AppEventSender) -> Self {
        Self::new_for_store_mode(app_event_tx, AuthCredentialsStoreMode::File)
    }

    pub(crate) fn new_for_store_mode(
        app_event_tx: AppEventSender,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Self {
        Self {
            app_event_tx,
            state: LoginAddAccountState::Choose,
            selected: 0,
            is_complete: false,
            completion: None,
            pool_behavior: AccountPoolBehavior::for_store_mode(auth_credentials_store_mode),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_state(app_event_tx: AppEventSender, state: LoginAddAccountState) -> Self {
        Self::with_state_for_store_mode(app_event_tx, state, AuthCredentialsStoreMode::File)
    }

    pub(crate) fn with_state_for_store_mode(
        app_event_tx: AppEventSender,
        state: LoginAddAccountState,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Self {
        Self {
            app_event_tx,
            state,
            selected: 0,
            is_complete: false,
            completion: None,
            pool_behavior: AccountPoolBehavior::for_store_mode(auth_credentials_store_mode),
        }
    }

    pub(crate) fn waiting_login_id(&self) -> Option<&str> {
        match &self.state {
            LoginAddAccountState::Waiting { login_id, .. }
            | LoginAddAccountState::DeviceCodeWaiting { login_id, .. } => Some(login_id),
            LoginAddAccountState::Choose
            | LoginAddAccountState::ApiKey { .. }
            | LoginAddAccountState::SavingApiKey
            | LoginAddAccountState::Starting
            | LoginAddAccountState::DeviceCodeStarting
            | LoginAddAccountState::DeviceCodeFailed(_)
            | LoginAddAccountState::ApiKeyFailed(_)
            | LoginAddAccountState::Failed(_)
            | LoginAddAccountState::Complete => None,
        }
    }

    fn finish(&mut self, completion: ViewCompletion) {
        if matches!(
            self.state,
            LoginAddAccountState::Starting
                | LoginAddAccountState::Waiting { .. }
                | LoginAddAccountState::DeviceCodeStarting
                | LoginAddAccountState::DeviceCodeWaiting { .. }
        ) {
            self.app_event_tx.send(AppEvent::LoginCancelChatGpt);
        }
        self.is_complete = true;
        self.completion = Some(completion);
    }

    fn finish_and_show_accounts(&mut self) {
        self.finish(ViewCompletion::Accepted);
        self.app_event_tx.send(AppEvent::ShowLoginAccounts);
    }

    fn return_to_accounts_on_cancel(&self) -> bool {
        matches!(
            self.state,
            LoginAddAccountState::Choose
                | LoginAddAccountState::ApiKey { .. }
                | LoginAddAccountState::ApiKeyFailed(_)
                | LoginAddAccountState::Starting
                | LoginAddAccountState::Waiting { .. }
                | LoginAddAccountState::DeviceCodeStarting
                | LoginAddAccountState::DeviceCodeWaiting { .. }
                | LoginAddAccountState::DeviceCodeFailed(_)
                | LoginAddAccountState::Failed(_)
                | LoginAddAccountState::Complete
        )
    }

    fn handle_enter(&mut self) {
        match &mut self.state {
            LoginAddAccountState::Choose | LoginAddAccountState::Failed(_) => {
                if self.selected == 0 {
                    self.state = LoginAddAccountState::Starting;
                    self.app_event_tx.send(AppEvent::LoginStartChatGpt);
                } else {
                    self.state = LoginAddAccountState::ApiKey {
                        value: String::new(),
                        error: None,
                    };
                }
            }
            LoginAddAccountState::ApiKey { value, error } => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    *error = Some("API key cannot be empty".to_string());
                } else {
                    self.app_event_tx.send(AppEvent::LoginAddAccountApiKey {
                        api_key: SecretApiKey::new(trimmed.to_string()),
                    });
                    self.state = LoginAddAccountState::SavingApiKey;
                }
            }
            LoginAddAccountState::ApiKeyFailed(_) => {
                self.state = LoginAddAccountState::ApiKey {
                    value: String::new(),
                    error: None,
                };
            }
            LoginAddAccountState::DeviceCodeFailed(_) => {
                self.state = LoginAddAccountState::DeviceCodeStarting;
                self.app_event_tx.send(AppEvent::LoginStartDeviceCode);
            }
            LoginAddAccountState::Complete => {
                self.finish_and_show_accounts();
            }
            LoginAddAccountState::SavingApiKey
            | LoginAddAccountState::Starting
            | LoginAddAccountState::Waiting { .. }
            | LoginAddAccountState::DeviceCodeStarting
            | LoginAddAccountState::DeviceCodeWaiting { .. } => {}
        }
    }

    fn handle_api_key_char(&mut self, ch: char) {
        if ch.is_control() {
            return;
        }
        if let LoginAddAccountState::ApiKey { value, error } = &mut self.state {
            value.push(ch);
            *error = None;
        }
    }

    fn handle_api_key_backspace(&mut self) {
        if let LoginAddAccountState::ApiKey { value, error } = &mut self.state {
            value.pop();
            *error = None;
        }
    }

    fn content_line_count(&self) -> usize {
        match &self.state {
            LoginAddAccountState::Choose => 7,
            LoginAddAccountState::ApiKey { error, .. } => 7 + usize::from(error.is_some()) * 2,
            LoginAddAccountState::SavingApiKey => 6,
            LoginAddAccountState::Starting => 6,
            LoginAddAccountState::Waiting { .. } => 8,
            LoginAddAccountState::DeviceCodeStarting => 6,
            LoginAddAccountState::DeviceCodeWaiting { .. } => 10,
            LoginAddAccountState::DeviceCodeFailed(_) => 8,
            LoginAddAccountState::ApiKeyFailed(_) => 8,
            LoginAddAccountState::Failed(_) => 8,
            LoginAddAccountState::Complete => 6,
        }
    }
}

impl BottomPaneView for LoginAddAccountView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match (&self.state, key_event.code) {
            (_, KeyCode::Esc) if self.return_to_accounts_on_cancel() => {
                self.finish_and_show_accounts();
            }
            (_, KeyCode::Esc) => self.finish(ViewCompletion::Cancelled),
            (LoginAddAccountState::ApiKey { .. }, KeyCode::Char('q'))
                if text_input_modifiers(key_event.modifiers) =>
            {
                self.handle_api_key_char('q');
            }
            (_, KeyCode::Char('q')) if self.return_to_accounts_on_cancel() => {
                self.finish_and_show_accounts();
            }
            (_, KeyCode::Char('q')) => self.finish(ViewCompletion::Cancelled),
            (LoginAddAccountState::Choose, KeyCode::Up | KeyCode::Down) => {
                self.selected = if self.selected == 0 { 1 } else { 0 };
            }
            (LoginAddAccountState::Waiting { .. }, KeyCode::Char('c' | 'C'))
                if key_event.modifiers.is_empty() =>
            {
                self.state = LoginAddAccountState::DeviceCodeStarting;
                self.app_event_tx.send(AppEvent::LoginStartDeviceCode);
            }
            (LoginAddAccountState::ApiKey { .. }, KeyCode::Backspace | KeyCode::Delete) => {
                self.handle_api_key_backspace();
            }
            (LoginAddAccountState::ApiKey { .. }, KeyCode::Char(ch))
                if text_input_modifiers(key_event.modifiers) =>
            {
                self.handle_api_key_char(ch);
            }
            (_, KeyCode::Enter) => self.handle_enter(),
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.is_complete
    }

    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }

    fn view_id(&self) -> Option<&'static str> {
        Some(LOGIN_ADD_ACCOUNT_VIEW_ID)
    }

    fn active_login_add_account_id(&self) -> Option<&str> {
        self.waiting_login_id()
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        if let LoginAddAccountState::ApiKey { value, error } = &mut self.state {
            *value = pasted.trim().to_string();
            *error = None;
            return true;
        }
        false
    }
}

impl Renderable for LoginAddAccountView {
    fn desired_height(&self, _width: u16) -> u16 {
        (self.content_line_count() + 2).max(9) as u16
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .style(Style::default().fg(Color::White))
            .title(self.pool_behavior.add_view_title())
            .title_alignment(Alignment::Center);
        let inner = block.inner(area);
        block.render(area, buf);

        let mut lines = Vec::new();
        let hyperlink_url = match &self.state {
            LoginAddAccountState::Waiting { auth_url, .. } => Some(auth_url.as_str()),
            LoginAddAccountState::DeviceCodeWaiting {
                verification_url, ..
            } => Some(verification_url.as_str()),
            LoginAddAccountState::Choose
            | LoginAddAccountState::ApiKey { .. }
            | LoginAddAccountState::SavingApiKey
            | LoginAddAccountState::Starting
            | LoginAddAccountState::DeviceCodeStarting
            | LoginAddAccountState::DeviceCodeFailed(_)
            | LoginAddAccountState::ApiKeyFailed(_)
            | LoginAddAccountState::Failed(_)
            | LoginAddAccountState::Complete => None,
        };
        match &self.state {
            LoginAddAccountState::Choose => {
                lines.push(Line::from(vec![Span::styled(
                    "Choose a sign-in method",
                    Style::default().add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(""));
                lines.push(render_selectable_line(
                    "ChatGPT sign-in",
                    self.selected == 0,
                    /*active*/ false,
                ));
                lines.push(render_selectable_line(
                    "API key",
                    self.selected == 1,
                    /*active*/ false,
                ));
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("Start", "Back"));
            }
            LoginAddAccountState::ApiKey { value, error } => {
                lines.push(Line::from(vec![Span::styled(
                    "Paste your OpenAI API key",
                    Style::default().add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(mask_api_key_input(value)));
                if let Some(error) = error {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        error.clone(),
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    )));
                }
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("Save", "Back"));
            }
            LoginAddAccountState::SavingApiKey => {
                lines.push(Line::from(vec![Span::styled(
                    "Saving API key...",
                    Style::default().add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    if self.pool_behavior.supports_pooling() {
                        "The account list will refresh when the key is saved."
                    } else {
                        "The current login will be replaced when the key is saved."
                    },
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("", "Cancel"));
            }
            LoginAddAccountState::Starting => {
                lines.push(Line::from(vec![Span::styled(
                    "Opening ChatGPT sign-in...",
                    Style::default().add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Waiting for the authentication page.",
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("", "Cancel"));
            }
            LoginAddAccountState::Waiting { auth_url, .. } => {
                lines.push(Line::from(vec![Span::styled(
                    "Finish signing in with ChatGPT in your browser",
                    Style::default().add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(""));
                lines.push(Line::from("If your browser did not open, visit:"));
                lines.push(Line::from(Span::styled(
                    auth_url.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::UNDERLINED),
                )));
                lines.push(Line::from(vec![
                    Span::styled(
                        "Not seeing a browser? ",
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled("Press C to use a code.", Style::default().fg(Color::Cyan)),
                ]));
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("", "Cancel"));
            }
            LoginAddAccountState::DeviceCodeStarting => {
                lines.push(Line::from(vec![Span::styled(
                    "Generating sign-in code...",
                    Style::default().add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Waiting for a one-time ChatGPT code.",
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("", "Cancel"));
            }
            LoginAddAccountState::DeviceCodeWaiting {
                verification_url,
                user_code,
                ..
            } => {
                lines.push(Line::from(vec![Span::styled(
                    "Complete sign-in using this code",
                    Style::default().add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from("Visit this link on any device:"));
                lines.push(Line::from(Span::styled(
                    verification_url.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::UNDERLINED),
                )));
                lines.push(Line::from(vec![
                    Span::styled("Code: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        user_code.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Keep this code private. It expires after 15 minutes.",
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("", "Cancel"));
            }
            LoginAddAccountState::DeviceCodeFailed(message)
            | LoginAddAccountState::ApiKeyFailed(message) => {
                lines.push(Line::from(vec![Span::styled(
                    if self.pool_behavior.supports_pooling() {
                        "Add account failed"
                    } else {
                        "Replace login failed"
                    },
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    message.clone(),
                    Style::default().fg(Color::Red),
                )));
                lines.push(Line::from(""));
                lines.push(render_selectable_line(
                    "Try again",
                    /*selected*/ true,
                    /*active*/ false,
                ));
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("Retry", "Back"));
            }
            LoginAddAccountState::Failed(message) => {
                lines.push(Line::from(vec![Span::styled(
                    "ChatGPT sign-in failed",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    message.clone(),
                    Style::default().fg(Color::Red),
                )));
                lines.push(Line::from(""));
                lines.push(render_selectable_line(
                    "Try again",
                    /*selected*/ true,
                    /*active*/ false,
                ));
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("Retry", "Back"));
            }
            LoginAddAccountState::Complete => {
                lines.push(Line::from(vec![Span::styled(
                    if self.pool_behavior.supports_pooling() {
                        "Account added"
                    } else {
                        "Login replaced"
                    },
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    if self.pool_behavior.supports_pooling() {
                        "The account list will refresh with your new ChatGPT account."
                    } else {
                        "The account list will refresh with the current login."
                    },
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("Continue", "Back"));
            }
        }

        let content_area = Rect {
            x: inner.x.saturating_add(1),
            y: inner.y,
            width: inner.width.saturating_sub(2),
            height: inner.height,
        };

        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .alignment(Alignment::Left)
            .style(Style::default().fg(Color::White))
            .render(content_area, buf);

        if let Some(url) = hyperlink_url {
            crate::terminal_hyperlinks::mark_url_hyperlink(buf, content_area, url);
        }
    }
}

impl Renderable for LoginAccountsView {
    fn desired_height(&self, width: u16) -> u16 {
        (self.content_line_count(width) + 2).max(9) as u16
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .style(Style::default().fg(Color::White))
            .title(" Manage Accounts ")
            .title_alignment(Alignment::Center);
        let inner = block.inner(area);
        block.render(area, buf);

        let mut lines = Vec::new();
        if let Some(error) = &self.error {
            lines.push(Line::from(vec![Span::styled(
                error.clone(),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(""));
        }
        if let Some(feedback) = &self.feedback {
            let (message, color) = match feedback {
                LoginAccountsFeedback::Info(message) => (message, Color::Green),
                LoginAccountsFeedback::Error(message) => (message, Color::Red),
            };
            lines.push(Line::from(vec![Span::styled(
                message.clone(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(""));
        }

        lines.push(Line::from(vec![Span::styled(
            "Connected Accounts",
            Style::default().add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(""));

        if self.pool_behavior == AccountPoolBehavior::CurrentOnly {
            lines.push(Line::from(Span::styled(
                CURRENT_ONLY_POOL_NOTICE,
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(""));
        }

        if self.accounts.is_empty() {
            lines.push(Line::from(Span::styled(
                "No accounts connected yet.",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (index, account) in self.accounts.iter().enumerate() {
                lines.push(account.render_line(index == self.selected));
            }
        }

        let add_selected = self.selected == self.add_row_index();
        lines.push(Line::from(""));
        lines.push(render_selectable_line(
            self.pool_behavior.account_action_label(),
            add_selected,
            /*active*/ false,
        ));
        lines.push(Line::from(""));
        let mut hint_spans = vec![
            Span::styled("up/down", Style::default().fg(Color::Cyan)),
            Span::styled(" Navigate  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::styled(" Select  ", Style::default().fg(Color::DarkGray)),
        ];
        hint_spans.push(Span::styled(
            "d",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        hint_spans.push(Span::styled(
            " Disconnect  ",
            Style::default().fg(Color::DarkGray),
        ));
        hint_spans.push(Span::styled(
            "Esc",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
        hint_spans.push(Span::styled(" Close", Style::default().fg(Color::DarkGray)));
        lines.push(Line::from(hint_spans));

        if let LoginAccountsMode::ConfirmRemove { label, .. } = &self.mode {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                format!("Disconnect {label}?"),
                Style::default().add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from("Press Enter to disconnect or Esc to cancel."));
        }

        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .alignment(Alignment::Left)
            .style(Style::default().fg(Color::White))
            .render(
                Rect {
                    x: inner.x.saturating_add(1),
                    y: inner.y,
                    width: inner.width.saturating_sub(2),
                    height: inner.height,
                },
                buf,
            );
    }
}

impl AccountRow {
    fn from(account: StoredAccount, active_id: Option<&str>) -> Self {
        let id = account.id.clone();
        let label = account_display_label(&account);
        let mode = account.mode;
        let detail = account_detail(&account);
        let is_active = active_id == Some(id.as_str());
        Self {
            id,
            label,
            detail,
            mode: auth_mode_to_api(mode),
            health: match account.health {
                codex_login::AccountHealth::Ok => ApiAccountHealth::Ok,
                codex_login::AccountHealth::ReauthRequired => ApiAccountHealth::ReauthRequired,
            },
            is_active,
            is_pooled: true,
        }
    }

    fn from_list_entry(entry: AccountListEntry) -> Self {
        let label = account_list_entry_label(&entry);
        let detail = account_list_entry_detail(&entry);
        let id = entry.account_id;
        Self {
            id,
            label,
            detail,
            mode: entry.auth_mode,
            health: entry.health,
            is_active: entry.is_active,
            is_pooled: true,
        }
    }

    fn activation_supported(&self) -> bool {
        self.health == ApiAccountHealth::Ok
            && self.is_pooled
            && matches!(
                self.mode,
                ApiAuthMode::ApiKey | ApiAuthMode::Chatgpt | ApiAuthMode::ChatgptAuthTokens
            )
    }

    fn render_line(&self, selected: bool) -> Line<'static> {
        let mut spans = render_selectable_spans(&self.label, selected, self.is_active);
        if self.is_active {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                "(current)",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if self.health == ApiAccountHealth::ReauthRequired {
            spans.push(" ".into());
            spans.push("(sign-in required)".red().bold());
        }
        if let Some(detail) = &self.detail {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                detail.clone(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if self.health == ApiAccountHealth::Ok && !self.is_active && !self.activation_supported() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                "activation unavailable",
                Style::default().fg(Color::DarkGray),
            ));
        }
        Line::from(spans)
    }
}

fn render_selectable_line(label: &str, selected: bool, active: bool) -> Line<'static> {
    Line::from(render_selectable_spans(label, selected, active))
}

fn add_account_hint_line(enter_label: &str, cancel_label: &str) -> Line<'static> {
    let mut spans = Vec::new();
    if !enter_label.is_empty() {
        spans.push(Span::styled("Enter", Style::default().fg(Color::Green)));
        spans.push(Span::styled(
            format!(" {enter_label}  "),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.push(Span::styled(
        "Esc",
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!(" {cancel_label}"),
        Style::default().fg(Color::DarkGray),
    ));
    Line::from(spans)
}

fn mask_api_key_input(value: &str) -> String {
    if value.is_empty() {
        "sk-...".to_string()
    } else {
        "*".repeat(value.chars().count())
    }
}

fn text_input_modifiers(modifiers: KeyModifiers) -> bool {
    !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

fn render_selectable_spans(label: &str, selected: bool, active: bool) -> Vec<Span<'static>> {
    let arrow_style = if selected {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let label_style = if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if active {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    vec![
        Span::styled(if selected { "> " } else { "  " }, arrow_style),
        Span::styled(label.to_string(), label_style),
    ]
}

fn account_detail(account: &StoredAccount) -> Option<String> {
    let mut details = Vec::new();
    match account.mode {
        AuthMode::ApiKey => details.push("API key".to_string()),
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => {
            if let Some(plan) = account
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.id_token.get_chatgpt_plan_type())
            {
                details.push(format!("{plan} Plan"));
            }
        }
        AuthMode::AgentIdentity => details.push("agent identity".to_string()),
        AuthMode::PersonalAccessToken => details.push("personal access token".to_string()),
        AuthMode::Headers => details.push("request headers".to_string()),
        AuthMode::BedrockApiKey => details.push("Bedrock API key".to_string()),
    }
    if let Some(created_at) = account.created_at {
        details.push(format!("connected {}", format_timestamp(created_at)));
    }
    (!details.is_empty()).then(|| details.join(" - "))
}

fn account_list_entry_label(entry: &AccountListEntry) -> String {
    if let Some(label) = entry.label.as_ref() {
        let trimmed = label.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    match entry.auth_mode {
        ApiAuthMode::ApiKey => "API key".to_string(),
        ApiAuthMode::Chatgpt | ApiAuthMode::ChatgptAuthTokens => "ChatGPT".to_string(),
        ApiAuthMode::Headers => "Request headers".to_string(),
        ApiAuthMode::AgentIdentity => "Agent identity".to_string(),
        ApiAuthMode::PersonalAccessToken => "Personal access token".to_string(),
        ApiAuthMode::BedrockApiKey => "Bedrock API key".to_string(),
    }
}

fn account_list_entry_detail(entry: &AccountListEntry) -> Option<String> {
    let mut details = Vec::new();
    let mode_detail = match entry.auth_mode {
        ApiAuthMode::ApiKey => "API key",
        ApiAuthMode::Chatgpt | ApiAuthMode::ChatgptAuthTokens => "ChatGPT",
        ApiAuthMode::Headers => "request headers",
        ApiAuthMode::AgentIdentity => "agent identity",
        ApiAuthMode::PersonalAccessToken => "personal access token",
        ApiAuthMode::BedrockApiKey => "Bedrock API key",
    };
    if entry
        .label
        .as_ref()
        .map(|label| label.trim())
        .is_some_and(|label| !label.is_empty() && !label.eq_ignore_ascii_case(mode_detail))
    {
        details.push(mode_detail.to_string());
    }
    if let Some(timestamp) = entry.created_at {
        details.push(format!("connected {}", format_epoch_timestamp(timestamp)));
    }
    (!details.is_empty()).then(|| details.join(" - "))
}

fn format_epoch_timestamp(timestamp: i64) -> String {
    DateTime::from_timestamp(timestamp, 0)
        .map(format_timestamp)
        .unwrap_or_else(|| "unknown time".to_string())
}

fn sort_account_rows(accounts: &mut [AccountRow]) {
    accounts.sort_by(|left, right| {
        api_account_mode_priority(left.mode)
            .cmp(&api_account_mode_priority(right.mode))
            .then_with(|| {
                left.label
                    .to_ascii_lowercase()
                    .cmp(&right.label.to_ascii_lowercase())
            })
            .then_with(|| left.label.cmp(&right.label))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn api_account_mode_priority(mode: ApiAuthMode) -> u8 {
    match mode {
        ApiAuthMode::Chatgpt | ApiAuthMode::ChatgptAuthTokens => 0,
        ApiAuthMode::ApiKey => 1,
        ApiAuthMode::AgentIdentity => 2,
        ApiAuthMode::PersonalAccessToken => 3,
        ApiAuthMode::Headers => 4,
        ApiAuthMode::BedrockApiKey => 5,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct LoadedLoginAccounts {
    accounts: Vec<AccountRow>,
    selected: usize,
    error: Option<String>,
}

fn load_account_rows(
    codex_home: &Path,
    default_auth_home_is_current: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_keyring_backend_kind: AuthKeyringBackendKind,
    previously_selected_id: Option<String>,
) -> LoadedLoginAccounts {
    let pool_behavior = AccountPoolBehavior::for_store_mode(auth_credentials_store_mode);
    let mut error = sync_account_store_from_auth(
        codex_home,
        default_auth_home_is_current,
        auth_credentials_store_mode,
        auth_keyring_backend_kind,
    );
    let current_auth = if default_auth_home_is_current && !pool_behavior.supports_pooling() {
        match codex_login::load_auth_dot_json(
            codex_home,
            auth_credentials_store_mode,
            auth_keyring_backend_kind,
        ) {
            Ok(auth) => auth,
            Err(err) => {
                if error.is_none() {
                    error = Some(format!("Failed to read current auth: {err}"));
                }
                None
            }
        }
    } else {
        None
    };
    let stored_accounts = if pool_behavior.supports_pooling() {
        match codex_login::list_accounts(codex_home, auth_credentials_store_mode) {
            Ok(accounts) => accounts,
            Err(err) => {
                if error.is_none() {
                    error = Some(format!("Failed to read accounts: {err}"));
                }
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let matching_current_account_id = current_auth.as_ref().and_then(|auth| {
        stored_accounts
            .iter()
            .find(|account| account_matches_auth(account, auth))
            .map(|account| account.id.clone())
    });
    let mut active_account_id = if pool_behavior.supports_pooling() {
        default_auth_home_is_current
            .then(|| {
                codex_login::get_active_account_id(codex_home, auth_credentials_store_mode)
                    .ok()
                    .flatten()
            })
            .flatten()
    } else {
        matching_current_account_id
    };
    let mut accounts = stored_accounts
        .into_iter()
        .map(|account| AccountRow::from(account, active_account_id.as_deref()))
        .collect::<Vec<_>>();
    if let Some(auth) = current_auth
        && active_account_id.is_none()
    {
        accounts.push(current_auth_account_row(&auth, auth_credentials_store_mode));
        active_account_id = Some(CURRENT_AUTH_ACCOUNT_ID.to_string());
    }

    sort_account_rows(&mut accounts);

    let selected = previously_selected_id
        .as_ref()
        .and_then(|id| accounts.iter().position(|account| &account.id == id))
        .or_else(|| {
            active_account_id
                .as_ref()
                .and_then(|id| accounts.iter().position(|account| &account.id == id))
        })
        .unwrap_or(0);

    LoadedLoginAccounts {
        accounts,
        selected,
        error,
    }
}

fn sync_account_store_from_auth(
    codex_home: &Path,
    default_auth_home_is_current: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_keyring_backend_kind: AuthKeyringBackendKind,
) -> Option<String> {
    if !default_auth_home_is_current {
        return None;
    }
    if auth_credentials_store_mode != AuthCredentialsStoreMode::File {
        return None;
    }

    let auth = match codex_login::load_auth_dot_json(
        codex_home,
        auth_credentials_store_mode,
        auth_keyring_backend_kind,
    ) {
        Ok(Some(auth)) => auth,
        Ok(None) => return None,
        Err(err) => return Some(format!("Failed to read current auth: {err}")),
    };

    if let Some(tokens) = auth.tokens {
        let last_refresh = auth.last_refresh.unwrap_or_else(Utc::now);
        let email = tokens.id_token.email.clone();
        return codex_login::upsert_chatgpt_account(
            codex_home,
            auth_credentials_store_mode,
            tokens,
            last_refresh,
            email,
            /*make_active*/ true,
        )
        .err()
        .map(|err| format!("Failed to record ChatGPT login: {err}"));
    }

    if let Some(api_key) = auth.openai_api_key {
        return codex_login::upsert_api_key_account(
            codex_home,
            auth_credentials_store_mode,
            api_key,
            /*label*/ None,
            /*make_active*/ true,
        )
        .err()
        .map(|err| format!("Failed to record API key login: {err}"));
    }

    None
}

fn format_timestamp(ts: DateTime<Utc>) -> String {
    ts.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

#[cfg(test)]
#[path = "login_accounts_view_tests.rs"]
mod tests;
