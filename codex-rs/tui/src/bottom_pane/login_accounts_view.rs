use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AuthMode;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::StoredAccount;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Alignment;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
use textwrap::wrap;

use crate::account_label::account_display_label;
use crate::account_label::account_mode_priority;
use crate::app_event::AppEvent;
use crate::app_event::AuthAccountSelection;
use crate::app_event::RemoveAuthAccountSelection;
use crate::app_event::SecretApiKey;
use crate::app_event_sender::AppEventSender;
use crate::render::renderable::Renderable;

use super::BottomPaneView;
use super::ViewCompletion;

pub(crate) const LOGIN_ACCOUNTS_VIEW_ID: &str = "login-accounts";
pub(crate) const LOGIN_ADD_ACCOUNT_VIEW_ID: &str = "login-add-account";

/// Interactive view shown for `/login` to manage stored accounts.
pub(crate) struct LoginAccountsView {
    app_event_tx: AppEventSender,
    accounts: Vec<AccountRow>,
    selected: usize,
    error: Option<String>,
    feedback: Option<LoginAccountsFeedback>,
    mode: LoginAccountsMode,
    is_complete: bool,
    completion: Option<ViewCompletion>,
}

#[derive(Clone, Debug)]
struct AccountRow {
    id: String,
    label: String,
    detail: Option<String>,
    mode: AuthMode,
    is_active: bool,
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
        codex_home: &std::path::Path,
        app_event_tx: AppEventSender,
        default_auth_home_is_current: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        feedback: Option<LoginAccountsFeedback>,
    ) -> Self {
        let mut error = sync_account_store_from_auth(
            codex_home,
            default_auth_home_is_current,
            auth_credentials_store_mode,
        );
        let active_account_id = default_auth_home_is_current
            .then(|| {
                codex_login::get_active_account_id(codex_home)
                    .ok()
                    .flatten()
            })
            .flatten();
        let (mut accounts, error) = match codex_login::list_accounts(codex_home) {
            Ok(accounts) => (
                accounts
                    .into_iter()
                    .map(|account| AccountRow::from(account, active_account_id.as_deref()))
                    .collect::<Vec<_>>(),
                error,
            ),
            Err(err) => {
                if error.is_none() {
                    error = Some(format!("Failed to read accounts: {err}"));
                }
                (Vec::new(), error)
            }
        };

        accounts.sort_by(|left, right| {
            account_mode_priority(left.mode)
                .cmp(&account_mode_priority(right.mode))
                .then_with(|| {
                    left.label
                        .to_ascii_lowercase()
                        .cmp(&right.label.to_ascii_lowercase())
                })
                .then_with(|| left.label.cmp(&right.label))
                .then_with(|| left.id.cmp(&right.id))
        });

        let selected = active_account_id
            .as_ref()
            .and_then(|id| accounts.iter().position(|account| &account.id == id))
            .unwrap_or(0);

        Self {
            app_event_tx,
            accounts,
            selected,
            error,
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
        self.mode = LoginAccountsMode::ConfirmRemove {
            account_id: account.id.clone(),
            label: account.label.clone(),
        };
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
        lines += self.accounts.len().max(1);
        lines += 3;
        lines += Self::wrapped_line_count(
            "up/down Navigate  Enter Select  d Disconnect  Esc Close",
            content_width,
        );
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
}

impl LoginAddAccountView {
    pub(crate) fn new(app_event_tx: AppEventSender) -> Self {
        Self {
            app_event_tx,
            state: LoginAddAccountState::Choose,
            selected: 0,
            is_complete: false,
            completion: None,
        }
    }

    pub(crate) fn with_state(app_event_tx: AppEventSender, state: LoginAddAccountState) -> Self {
        Self {
            app_event_tx,
            state,
            selected: 0,
            is_complete: false,
            completion: None,
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
                self.finish(ViewCompletion::Accepted);
                self.app_event_tx.send(AppEvent::ShowLoginAccounts);
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
            (_, KeyCode::Esc) => self.finish(ViewCompletion::Cancelled),
            (LoginAddAccountState::ApiKey { .. }, KeyCode::Char('q'))
                if text_input_modifiers(key_event.modifiers) =>
            {
                self.handle_api_key_char('q');
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
            .title(" Add Account ")
            .title_alignment(Alignment::Center);
        let inner = block.inner(area);
        block.render(area, buf);

        let mut lines = Vec::new();
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
                    false,
                ));
                lines.push(render_selectable_line("API key", self.selected == 1, false));
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("Start", "Close"));
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
                    "The account list will refresh when the key is saved.",
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
                    Style::default().fg(Color::Cyan),
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
                    Style::default().fg(Color::Cyan),
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
                    "Add account failed",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    message.clone(),
                    Style::default().fg(Color::Red),
                )));
                lines.push(Line::from(""));
                lines.push(render_selectable_line("Try again", true, false));
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("Retry", "Close"));
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
                lines.push(render_selectable_line("Try again", true, false));
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("Retry", "Close"));
            }
            LoginAddAccountState::Complete => {
                lines.push(Line::from(vec![Span::styled(
                    "Account added",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "The account list will refresh with your new ChatGPT account.",
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(""));
                lines.push(add_account_hint_line("Continue", "Close"));
            }
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
            "Add account...",
            add_selected,
            false,
        ));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("up/down", Style::default().fg(Color::Cyan)),
            Span::styled(" Navigate  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::styled(" Select  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "d",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Disconnect  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "Esc",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Close", Style::default().fg(Color::DarkGray)),
        ]));

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
            mode,
            is_active,
        }
    }

    fn activation_supported(&self) -> bool {
        matches!(
            self.mode,
            AuthMode::ApiKey | AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens
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
        if let Some(detail) = &self.detail {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                detail.clone(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if !self.activation_supported() {
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
            details.push("ChatGPT".to_string());
            if let Some(account_id) = account.tokens.as_ref().and_then(|tokens| {
                tokens
                    .account_id
                    .as_deref()
                    .or(tokens.id_token.chatgpt_account_id.as_deref())
            }) {
                details.push(account_id.to_string());
            }
        }
        AuthMode::AgentIdentity => details.push("agent identity".to_string()),
        AuthMode::PersonalAccessToken => details.push("personal access token".to_string()),
    }
    if let Some(created_at) = account.created_at {
        details.push(format!("connected {}", format_timestamp(created_at)));
    }
    (!details.is_empty()).then(|| details.join(" - "))
}

fn sync_account_store_from_auth(
    codex_home: &std::path::Path,
    default_auth_home_is_current: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> Option<String> {
    if !default_auth_home_is_current {
        return None;
    }

    let auth = match codex_login::load_auth_dot_json(codex_home, auth_credentials_store_mode) {
        Ok(Some(auth)) => auth,
        Ok(None) => return None,
        Err(err) => return Some(format!("Failed to read current auth: {err}")),
    };

    if let Some(tokens) = auth.tokens {
        let last_refresh = auth.last_refresh.unwrap_or_else(Utc::now);
        let email = tokens.id_token.email.clone();
        return codex_login::upsert_chatgpt_account(
            codex_home,
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
            codex_home, api_key, /*label*/ None, /*make_active*/ true,
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
