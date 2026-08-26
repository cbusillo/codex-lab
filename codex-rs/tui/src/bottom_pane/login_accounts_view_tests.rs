use super::*;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::terminal_hyperlinks::strip_osc8;
use assert_matches::assert_matches;
use codex_app_server_protocol::AccountHealth;
use codex_app_server_protocol::AccountListEntry;
use codex_app_server_protocol::AuthMode as AppServerAuthMode;
use codex_protocol::auth::AuthMode as CoreAuthMode;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tokio::sync::mpsc::unbounded_channel;

const TEST_AUTH_KEYRING_BACKEND: codex_login::AuthKeyringBackendKind =
    codex_login::AuthKeyringBackendKind::Direct;

fn app_event_sender() -> AppEventSender {
    let (tx, _rx) = unbounded_channel();
    AppEventSender::new(tx)
}

#[test]
fn keyring_auth_is_not_mirrored_to_plaintext_account_catalog() {
    let codex_home = tempfile::tempdir().expect("tempdir");

    assert_eq!(
        sync_account_store_from_auth(
            codex_home.path(),
            /*default_auth_home_is_current*/ true,
            codex_config::types::AuthCredentialsStoreMode::Keyring,
            TEST_AUTH_KEYRING_BACKEND,
        ),
        None
    );
    assert!(!codex_home.path().join("auth_accounts.json").exists());
}

fn current_only_api_key_view(
    app_event_tx: AppEventSender,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> LoginAccountsView {
    let auth = codex_login::AuthDotJson {
        auth_mode: Some(CoreAuthMode::ApiKey),
        openai_api_key: Some("sk-secure".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    };
    LoginAccountsView {
        app_event_tx,
        codex_home: std::path::PathBuf::new(),
        default_auth_home_is_current: true,
        auth_credentials_store_mode,
        auth_keyring_backend_kind: TEST_AUTH_KEYRING_BACKEND,
        pool_behavior: AccountPoolBehavior::CurrentOnly,
        accounts: vec![current_auth_account_row(&auth, auth_credentials_store_mode)],
        remote_loaded: false,
        selected: 0,
        error: None,
        feedback: None,
        is_complete: false,
        completion: None,
    }
}

#[test]
fn keyring_current_login_renders_without_claiming_pool_membership() {
    let view = current_only_api_key_view(app_event_sender(), AuthCredentialsStoreMode::Keyring);
    let area = Rect::new(0, 0, 76, view.desired_height(/*width*/ 76));
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);

    insta::assert_snapshot!(
        "keyring_current_login_not_pooled",
        render_snapshot(&buf, area)
    );
}

#[test]
fn auto_current_login_renders_without_claiming_pool_membership() {
    let view = current_only_api_key_view(app_event_sender(), AuthCredentialsStoreMode::Auto);
    let area = Rect::new(0, 0, 76, view.desired_height(/*width*/ 76));
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);

    insta::assert_snapshot!("auto_current_login_not_pooled", render_snapshot(&buf, area));
}

#[test]
fn current_only_modes_ignore_stale_plaintext_catalog() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    codex_login::upsert_api_key_account(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        "sk-stale".to_string(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("seed stale file account");
    codex_login::login_with_api_key(
        codex_home.path(),
        "sk-current",
        AuthCredentialsStoreMode::Ephemeral,
        TEST_AUTH_KEYRING_BACKEND,
    )
    .expect("seed current ephemeral auth");
    let current_auth = codex_login::load_auth_dot_json(
        codex_home.path(),
        AuthCredentialsStoreMode::Ephemeral,
        TEST_AUTH_KEYRING_BACKEND,
    )
    .expect("load current auth")
    .expect("current auth should exist");

    assert_eq!(
        load_account_rows(
            codex_home.path(),
            /*default_auth_home_is_current*/ true,
            AuthCredentialsStoreMode::Ephemeral,
            TEST_AUTH_KEYRING_BACKEND,
            /*previously_selected_id*/ None,
        ),
        LoadedLoginAccounts {
            accounts: vec![current_auth_account_row(
                &current_auth,
                AuthCredentialsStoreMode::Ephemeral,
            )],
            selected: 0,
            error: None,
        }
    );
}

fn app_event_sender_with_rx() -> (
    AppEventSender,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) {
    let (tx, rx) = unbounded_channel();
    (AppEventSender::new(tx), rx)
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

fn account_list_entry(
    account_id: &str,
    auth_mode: AppServerAuthMode,
    label: &str,
    is_active: bool,
) -> AccountListEntry {
    AccountListEntry {
        account_id: account_id.to_string(),
        auth_mode,
        health: AccountHealth::Ok,
        label: Some(label.to_string()),
        created_at: None,
        last_used_at: None,
        is_active,
    }
}

#[test]
fn unhealthy_account_renders_repair_state_and_enter_starts_login() {
    let (tx, mut rx) = app_event_sender_with_rx();
    let mut unhealthy = account_list_entry(
        "chatgpt",
        AppServerAuthMode::Chatgpt,
        "ChatGPT",
        /*is_active*/ false,
    );
    unhealthy.health = AccountHealth::ReauthRequired;
    let mut view =
        LoginAccountsView::new_with_loaded_accounts(tx, vec![unhealthy], /*feedback*/ None);
    let area = Rect::new(0, 0, 70, view.desired_height(/*width*/ 70));
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    insta::assert_snapshot!(
        "unhealthy_account_requires_sign_in",
        render_snapshot(&buf, area)
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(view.is_complete());
    assert_matches!(rx.try_recv(), Ok(AppEvent::ShowLoginAddAccount));
}

#[test]
fn loaded_account_list_refresh_reopens_accounts() {
    let (tx, mut rx) = app_event_sender_with_rx();
    let mut view = LoginAccountsView::new_with_loaded_accounts(
        tx,
        vec![account_list_entry(
            "api",
            AppServerAuthMode::ApiKey,
            "API key",
            /*is_active*/ true,
        )],
        /*feedback*/ None,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));

    assert!(view.is_complete());
    assert_eq!(view.completion(), Some(ViewCompletion::Accepted));
    assert_matches!(rx.try_recv(), Ok(AppEvent::ShowLoginAccounts));
}

#[test]
fn loaded_account_list_healthy_rows_are_informational() {
    let (tx, mut rx) = app_event_sender_with_rx();
    let mut view = LoginAccountsView::new_with_loaded_accounts(
        tx,
        vec![account_list_entry(
            "chatgpt",
            AppServerAuthMode::Chatgpt,
            "ChatGPT",
            /*is_active*/ true,
        )],
        /*feedback*/ None,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(!view.is_complete());
    assert_matches!(rx.try_recv(), Err(_));
}

#[test]
fn loaded_account_list_hides_deferred_actions() {
    let view = LoginAccountsView::new_with_loaded_accounts(
        app_event_sender(),
        vec![
            account_list_entry(
                "chatgpt",
                AppServerAuthMode::Chatgpt,
                "ChatGPT",
                /*is_active*/ true,
            ),
            account_list_entry(
                "api",
                AppServerAuthMode::ApiKey,
                "API key",
                /*is_active*/ false,
            ),
        ],
        /*feedback*/ None,
    );
    let area = Rect::new(0, 0, 56, view.desired_height(/*width*/ 56));
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);

    insta::assert_snapshot!(
        "loaded_account_list_deferred_actions_hidden",
        render_snapshot(&buf, area)
    );
}

fn assert_add_account_returns_to_accounts(
    state: LoginAddAccountState,
    key: KeyEvent,
    expect_cancel: bool,
) {
    let (tx, mut rx) = app_event_sender_with_rx();
    let mut view = LoginAddAccountView::with_state(tx, state);

    view.handle_key_event(key);

    assert!(view.is_complete());
    assert_eq!(view.completion(), Some(ViewCompletion::Accepted));
    if expect_cancel {
        assert_matches!(rx.try_recv(), Ok(AppEvent::LoginCancelChatGpt));
    }
    assert_matches!(rx.try_recv(), Ok(AppEvent::ShowLoginAccounts));
}

#[test]
fn add_account_choose_esc_returns_to_accounts() {
    let (tx, mut rx) = app_event_sender_with_rx();
    let mut view = LoginAddAccountView::new(tx);

    view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(view.is_complete());
    assert_eq!(view.completion(), Some(ViewCompletion::Accepted));
    assert_matches!(rx.try_recv(), Ok(AppEvent::ShowLoginAccounts));
}

#[test]
fn add_account_chatgpt_enter_starts_login() {
    let (tx, mut rx) = app_event_sender_with_rx();
    let mut view = LoginAddAccountView::new(tx);

    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(!view.is_complete());
    assert_matches!(view.state, LoginAddAccountState::Starting);
    assert_matches!(rx.try_recv(), Ok(AppEvent::LoginStartChatGpt));
    assert_matches!(rx.try_recv(), Err(_));
}

#[test]
fn add_account_api_key_esc_returns_to_accounts() {
    let (tx, mut rx) = app_event_sender_with_rx();
    let mut view = LoginAddAccountView::new(tx);
    view.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(view.is_complete());
    assert_eq!(view.completion(), Some(ViewCompletion::Accepted));
    assert_matches!(rx.try_recv(), Ok(AppEvent::ShowLoginAccounts));
}

#[test]
fn add_account_complete_enter_returns_to_accounts() {
    let (tx, mut rx) = app_event_sender_with_rx();
    let mut view = LoginAddAccountView::with_state(tx, LoginAddAccountState::Complete);

    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(view.is_complete());
    assert_eq!(view.completion(), Some(ViewCompletion::Accepted));
    assert_matches!(rx.try_recv(), Ok(AppEvent::ShowLoginAccounts));
}

#[test]
fn add_account_failed_esc_returns_to_accounts() {
    assert_add_account_returns_to_accounts(
        LoginAddAccountState::Failed("sign-in failed".to_string()),
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        /*expect_cancel*/ false,
    );
}

#[test]
fn add_account_api_key_failed_esc_returns_to_accounts() {
    assert_add_account_returns_to_accounts(
        LoginAddAccountState::ApiKeyFailed("invalid API key".to_string()),
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        /*expect_cancel*/ false,
    );
}

#[test]
fn add_account_device_code_failed_esc_returns_to_accounts() {
    assert_add_account_returns_to_accounts(
        LoginAddAccountState::DeviceCodeFailed("device code expired".to_string()),
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        /*expect_cancel*/ false,
    );
}

#[test]
fn add_account_complete_esc_returns_to_accounts() {
    assert_add_account_returns_to_accounts(
        LoginAddAccountState::Complete,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        /*expect_cancel*/ false,
    );
}

#[test]
fn add_account_choose_q_returns_to_accounts() {
    assert_add_account_returns_to_accounts(
        LoginAddAccountState::Choose,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        /*expect_cancel*/ false,
    );
}

#[test]
fn add_account_waiting_esc_cancels_and_returns_to_accounts() {
    assert_add_account_returns_to_accounts(
        LoginAddAccountState::Waiting {
            login_id: "login-1".to_string(),
            auth_url: "https://auth.example.com/login".to_string(),
        },
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        /*expect_cancel*/ true,
    );
}

#[test]
fn add_account_starting_esc_cancels_and_returns_to_accounts() {
    assert_add_account_returns_to_accounts(
        LoginAddAccountState::Starting,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        /*expect_cancel*/ true,
    );
}

#[test]
fn add_account_device_code_waiting_esc_cancels_and_returns_to_accounts() {
    assert_add_account_returns_to_accounts(
        LoginAddAccountState::DeviceCodeWaiting {
            login_id: "login-1".to_string(),
            verification_url: "https://chatgpt.com/device".to_string(),
            user_code: "ABCD-EFGH".to_string(),
        },
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        /*expect_cancel*/ true,
    );
}

#[test]
fn add_account_device_code_starting_esc_cancels_and_returns_to_accounts() {
    assert_add_account_returns_to_accounts(
        LoginAddAccountState::DeviceCodeStarting,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        /*expect_cancel*/ true,
    );
}

#[test]
fn add_account_waiting_q_cancels_and_returns_to_accounts() {
    assert_add_account_returns_to_accounts(
        LoginAddAccountState::Waiting {
            login_id: "login-1".to_string(),
            auth_url: "https://auth.example.com/login".to_string(),
        },
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        /*expect_cancel*/ true,
    );
}

#[test]
fn add_account_saving_api_key_esc_does_not_reopen_accounts() {
    let (tx, mut rx) = app_event_sender_with_rx();
    let mut view = LoginAddAccountView::with_state(tx, LoginAddAccountState::SavingApiKey);

    view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(view.is_complete());
    assert_eq!(view.completion(), Some(ViewCompletion::Cancelled));
    assert_matches!(rx.try_recv(), Err(_));
}

#[test]
fn add_account_saving_api_key_q_does_not_reopen_accounts() {
    let (tx, mut rx) = app_event_sender_with_rx();
    let mut view = LoginAddAccountView::with_state(tx, LoginAddAccountState::SavingApiKey);

    view.handle_key_event(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));

    assert!(view.is_complete());
    assert_eq!(view.completion(), Some(ViewCompletion::Cancelled));
    assert_matches!(rx.try_recv(), Err(_));
}

#[test]
fn add_account_back_hint_states_render_back() {
    let area = Rect::new(0, 0, 48, 12);
    let states = [
        ("choose", LoginAddAccountState::Choose),
        (
            "api_key_failed",
            LoginAddAccountState::ApiKeyFailed("invalid API key".to_string()),
        ),
        (
            "device_code_failed",
            LoginAddAccountState::DeviceCodeFailed("device code expired".to_string()),
        ),
        (
            "failed",
            LoginAddAccountState::Failed("sign-in failed".to_string()),
        ),
        ("complete", LoginAddAccountState::Complete),
    ];

    let rendered = states
        .into_iter()
        .map(|(name, state)| {
            let buf = render_add_account_state(state, area);
            format!("== {name} ==\n{}", render_snapshot(&buf, area))
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    insta::assert_snapshot!("add_account_back_hint_states", rendered);
}

#[test]
fn keyring_complete_view_explains_login_replacement() {
    let view = LoginAddAccountView::with_state_for_store_mode(
        app_event_sender(),
        LoginAddAccountState::Complete,
        AuthCredentialsStoreMode::Keyring,
    );
    let area = Rect::new(0, 0, 56, view.desired_height(/*width*/ 56));
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);

    insta::assert_snapshot!("keyring_login_replaced", render_snapshot(&buf, area));
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
