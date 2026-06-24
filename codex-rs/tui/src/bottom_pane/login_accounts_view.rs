use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AuthMode;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::StoredAccount;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
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

use crate::account_label::account_display_label;
use crate::account_label::account_mode_priority;
use crate::app_event::AppEvent;
use crate::app_event::AuthAccountSelection;
use crate::app_event::AuthProfileSelection;
use crate::app_event_sender::AppEventSender;
use crate::render::renderable::Renderable;

use super::BottomPaneView;
use super::ViewCompletion;

/// Interactive view shown for `/login` to manage stored accounts.
pub(crate) struct LoginAccountsView {
    app_event_tx: AppEventSender,
    add_profile_name: String,
    accounts: Vec<AccountRow>,
    selected: usize,
    error: Option<String>,
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

impl LoginAccountsView {
    pub(crate) fn new(
        codex_home: &std::path::Path,
        app_event_tx: AppEventSender,
        add_profile_name: String,
        default_auth_home_is_current: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
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
            add_profile_name,
            accounts,
            selected,
            error,
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
        if self.selected == self.add_row_index() {
            self.finish(ViewCompletion::Accepted);
            self.app_event_tx.send(AppEvent::SwitchAuthProfile {
                selection: AuthProfileSelection::Named {
                    profile_name: self.add_profile_name.clone(),
                    login_after_switch: true,
                },
            });
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

    fn content_line_count(&self) -> usize {
        let mut lines = 0;
        if self.error.is_some() {
            lines += 2;
        }
        lines += 2;
        lines += self.accounts.len().max(1);
        lines += 4;
        lines
    }
}

impl BottomPaneView for LoginAccountsView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Esc | KeyCode::Char('q') => self.finish(ViewCompletion::Cancelled),
            KeyCode::Up => self.select_previous(),
            KeyCode::Down => self.select_next(),
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

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }
}

impl Renderable for LoginAccountsView {
    fn desired_height(&self, _width: u16) -> u16 {
        (self.content_line_count() + 2).max(9) as u16
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
                "Esc",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Close", Style::default().fg(Color::DarkGray)),
        ]));

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
