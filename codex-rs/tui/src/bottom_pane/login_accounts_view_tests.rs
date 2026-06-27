use super::*;
use crate::app_event_sender::AppEventSender;
use crate::terminal_hyperlinks::strip_osc8;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tokio::sync::mpsc::unbounded_channel;

fn app_event_sender() -> AppEventSender {
    let (tx, _rx) = unbounded_channel();
    AppEventSender::new(tx)
}

fn collect_osc8_chars(buf: &Buffer, area: Rect, url: &str) -> String {
    let open = format!("\x1B]8;;{url}\x07");
    let close = "\x1B]8;;\x07";
    let mut chars = String::new();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let sym = buf[(x, y)].symbol();
            if let Some(rest) = sym.strip_prefix(open.as_str())
                && let Some(ch) = rest.strip_suffix(close)
            {
                chars.push_str(ch);
            }
        }
    }
    chars
}

fn render_add_account_state(state: LoginAddAccountState, area: Rect) -> Buffer {
    let view = LoginAddAccountView::with_state(app_event_sender(), state);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    buf
}

fn render_snapshot(buf: &Buffer, area: Rect) -> String {
    let mut lines: Vec<String> = (area.top()..area.bottom())
        .map(|y| {
            let mut line = String::new();
            for x in area.left()..area.right() {
                let symbol = buf[(x, y)].symbol();
                if symbol.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(&strip_osc8(symbol));
                }
            }
            line.trim_end().to_string()
        })
        .collect();

    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[test]
fn add_account_browser_url_renders_wrapped_osc8_hyperlink() {
    let url = "https://auth.example.com/login?state=abcdefghijklmnopqrstuvwxyz";
    let area = Rect::new(0, 0, 32, 12);
    let buf = render_add_account_state(
        LoginAddAccountState::Waiting {
            login_id: "login-1".to_string(),
            auth_url: url.to_string(),
        },
        area,
    );

    assert_eq!(collect_osc8_chars(&buf, area, url), url);
    insta::assert_snapshot!(
        "add_account_browser_url_wrapped",
        render_snapshot(&buf, area)
    );
}

#[test]
fn add_account_device_code_url_renders_wrapped_osc8_hyperlink() {
    let url = "https://chatgpt.com/device?flow=abcdefghijklmnopqrstuvwxyz";
    let area = Rect::new(0, 0, 32, 14);
    let buf = render_add_account_state(
        LoginAddAccountState::DeviceCodeWaiting {
            login_id: "login-1".to_string(),
            verification_url: url.to_string(),
            user_code: "ABCD-EFGH".to_string(),
        },
        area,
    );

    assert_eq!(collect_osc8_chars(&buf, area, url), url);
    insta::assert_snapshot!(
        "add_account_device_code_url_wrapped",
        render_snapshot(&buf, area)
    );
}
