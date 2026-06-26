use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::Widget;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::popup_consts::accept_cancel_hint_line;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::ListKeymap;
use crate::keymap::primary_binding;
use crate::render::Insets;
use crate::render::RectExt as _;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::measure_rows_height;
use super::selection_popup_common::render_rows;

pub(crate) const ACCOUNT_SWITCH_SETTINGS_VIEW_ID: &str = "account-switch-settings";

#[derive(Clone, Copy, PartialEq, Eq)]
enum AccountSwitchSetting {
    AutoSwitch,
    ApiKeyFallback,
}

struct AccountSwitchMenuItem {
    setting: AccountSwitchSetting,
    name: &'static str,
    description: &'static str,
    enabled: bool,
}

pub(crate) struct AccountSwitchSettingsView {
    items: Vec<AccountSwitchMenuItem>,
    state: ScrollState,
    complete: bool,
    app_event_tx: AppEventSender,
    keymap: ListKeymap,
}

impl AccountSwitchSettingsView {
    pub(crate) fn new(
        auto_switch_enabled: bool,
        api_key_fallback_enabled: bool,
        app_event_tx: AppEventSender,
        keymap: ListKeymap,
    ) -> Self {
        let mut view = Self {
            items: vec![
                AccountSwitchMenuItem {
                    setting: AccountSwitchSetting::AutoSwitch,
                    name: "Auto-switch accounts",
                    description: "Switch to another connected account after rate or usage limits.",
                    enabled: auto_switch_enabled,
                },
                AccountSwitchMenuItem {
                    setting: AccountSwitchSetting::ApiKeyFallback,
                    name: "API key fallback",
                    description: "Use saved API keys only after every ChatGPT account is limited.",
                    enabled: api_key_fallback_enabled,
                },
            ],
            state: ScrollState::new(),
            complete: false,
            app_event_tx,
            keymap,
        };
        view.initialize_selection();
        view
    }

    fn initialize_selection(&mut self) {
        self.state.selected_idx = (!self.items.is_empty()).then_some(0);
    }

    fn settings_header(&self) -> ColumnRenderable<'_> {
        let mut header = ColumnRenderable::new();
        header.push(Line::from("Account Switching".bold()));
        header.push(Line::from(
            "Choose how Codex moves between connected accounts. Changes are saved to config.toml"
                .dim(),
        ));
        header
    }

    fn selected_index(&self) -> Option<usize> {
        self.state
            .selected_idx
            .filter(|idx| *idx < self.items.len())
    }

    fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            self.state.selected_idx = None;
            return;
        }
        let current = self.selected_index().unwrap_or(0);
        let last = self.items.len().saturating_sub(1);
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs()).min(last)
        } else {
            current.saturating_add(delta as usize).min(last)
        };
        self.state.selected_idx = Some(next);
    }

    fn selected_setting_mut(&mut self) -> Option<&mut AccountSwitchMenuItem> {
        let idx = self.selected_index()?;
        self.items.get_mut(idx)
    }

    fn toggle_selected(&mut self) {
        let Some(item) = self.selected_setting_mut() else {
            return;
        };
        item.enabled = !item.enabled;
        let setting = item.setting;
        let enabled = item.enabled;
        match setting {
            AccountSwitchSetting::AutoSwitch => {
                self.app_event_tx
                    .send(AppEvent::SetAutoSwitchAccountsOnRateLimit(enabled));
            }
            AccountSwitchSetting::ApiKeyFallback => {
                self.app_event_tx
                    .send(AppEvent::SetApiKeyFallbackOnAllAccountsLimited(enabled));
            }
        }
    }

    fn build_rows(&self) -> Vec<GenericDisplayRow> {
        self.items
            .iter()
            .map(|item| GenericDisplayRow {
                name: format!("{} {}", if item.enabled { "[x]" } else { "[ ]" }, item.name),
                match_indices: None,
                description: Some(item.description.to_string()),
                wrap_indent: Some(4),
                ..Default::default()
            })
            .collect()
    }

    fn rows_width(width: u16) -> u16 {
        width.saturating_sub(4).max(1)
    }

    fn desired_content_height(&self, width: u16) -> u16 {
        let rows = self.build_rows();
        let rows_width = Self::rows_width(width);
        let rows_height =
            measure_rows_height(&rows, &self.state, MAX_POPUP_ROWS, rows_width).max(1);
        let header_height = self.settings_header().desired_height(width);
        header_height
            .saturating_add(rows_height)
            .saturating_add(/*hint*/ 1)
            .saturating_add(/*border and top inset*/ 3)
    }
}

impl BottomPaneView for AccountSwitchSettingsView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if key_event.modifiers == KeyModifiers::NONE && key_event.code == KeyCode::Esc {
            self.complete = true;
            return;
        }

        if self.keymap.cancel.is_pressed(key_event) {
            self.complete = true;
        } else if self.keymap.move_up.is_pressed(key_event) {
            self.move_selection(-1);
        } else if self.keymap.move_down.is_pressed(key_event) {
            self.move_selection(1);
        } else if self.keymap.accept.is_pressed(key_event) {
            self.toggle_selected();
        }
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.complete = true;
        CancellationEvent::Handled
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }

    fn view_id(&self) -> Option<&'static str> {
        Some(ACCOUNT_SWITCH_SETTINGS_VIEW_ID)
    }
}

impl Renderable for AccountSwitchSettingsView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let desired_height = self
            .desired_content_height(area.width)
            .max(3)
            .min(area.height);
        let area = Rect {
            y: area
                .y
                .saturating_add(area.height.saturating_sub(desired_height)),
            height: desired_height.min(area.height),
            ..area
        };

        let block = Block::bordered();
        let inner = block.inner(area).inset(Insets::tlbr(1, 1, 0, 1));
        block.render(area, buf);

        let chunks = Layout::vertical([
            Constraint::Length(self.settings_header().desired_height(inner.width)),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas::<3>(inner);

        self.settings_header().render(chunks[0], buf);
        let rows = self.build_rows();
        render_rows(
            chunks[1],
            buf,
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            "  No account settings available",
        );
        accept_cancel_hint_line(
            primary_binding(&self.keymap.accept),
            "to toggle",
            primary_binding(&self.keymap.cancel),
            "to close",
        )
        .render(chunks[2], buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.desired_content_height(width).max(3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::RuntimeKeymap;
    use crate::render::renderable::Renderable;
    use crossterm::event::KeyEventKind;
    use insta::assert_snapshot;
    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio::sync::mpsc::unbounded_channel;

    fn event_channel() -> (AppEventSender, UnboundedReceiver<AppEvent>) {
        let (tx, rx) = unbounded_channel();
        (AppEventSender::new(tx), rx)
    }

    fn render_lines(view: &AccountSwitchSettingsView, width: u16) -> String {
        let height = view.desired_height(width).max(1);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        view.render(area, &mut buffer);
        buffer
            .content
            .chunks(width as usize)
            .map(|line| line.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_account_switch_settings() {
        let (tx, _rx) = event_channel();
        let view = AccountSwitchSettingsView::new(
            /*auto_switch_enabled*/ true,
            /*api_key_fallback_enabled*/ false,
            tx,
            RuntimeKeymap::defaults().list,
        );

        assert_snapshot!(render_lines(&view, /*width*/ 84));
    }

    #[test]
    fn toggles_selected_setting() {
        let (tx, mut rx) = event_channel();
        let mut view = AccountSwitchSettingsView::new(
            /*auto_switch_enabled*/ true,
            /*api_key_fallback_enabled*/ false,
            tx,
            RuntimeKeymap::defaults().list,
        );

        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match rx.try_recv() {
            Ok(AppEvent::SetAutoSwitchAccountsOnRateLimit(false)) => {}
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn toggles_api_key_fallback_setting() {
        let (tx, mut rx) = event_channel();
        let mut view = AccountSwitchSettingsView::new(
            /*auto_switch_enabled*/ true,
            /*api_key_fallback_enabled*/ false,
            tx,
            RuntimeKeymap::defaults().list,
        );

        view.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match rx.try_recv() {
            Ok(AppEvent::SetApiKeyFallbackOnAllAccountsLimited(true)) => {}
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn escape_completes_view() {
        let (tx, _rx) = event_channel();
        let mut view = AccountSwitchSettingsView::new(
            /*auto_switch_enabled*/ true,
            /*api_key_fallback_enabled*/ false,
            tx,
            RuntimeKeymap::defaults().list,
        );

        view.handle_key_event(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        });

        assert!(view.is_complete());
    }
}
