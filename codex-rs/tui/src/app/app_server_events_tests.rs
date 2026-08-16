use super::*;
use crate::app::test_support::make_test_app;
use pretty_assertions::assert_eq;

fn login_completed(
    login_id: &str,
    success: bool,
    error: Option<&str>,
) -> AccountLoginCompletedNotification {
    AccountLoginCompletedNotification {
        login_id: Some(login_id.to_string()),
        success,
        error: error.map(str::to_string),
        onboarding_entrypoint: None,
    }
}

fn account_updated(auth_mode: AuthMode) -> AccountUpdatedNotification {
    AccountUpdatedNotification {
        auth_mode: Some(auth_mode),
        plan_type: None,
    }
}

async fn app_waiting_for_login(login_id: &str) -> App {
    let mut app = make_test_app().await;
    app.chat_widget.open_login_add_account_view();
    assert!(
        app.chat_widget
            .update_login_add_account_view(LoginAddAccountState::Waiting {
                login_id: login_id.to_string(),
                auth_url: "https://example.com/auth".to_string(),
            })
    );
    app.pending_login_add_account_id = Some(login_id.to_string());
    app
}

#[tokio::test]
async fn unrelated_and_stale_login_completions_are_ignored() {
    let mut app = app_waiting_for_login("login-a").await;

    app.handle_login_add_account_completed(&login_completed(
        "login-b", /*success*/ true, /*error*/ None,
    ));
    assert_eq!(app.pending_login_add_account_id.as_deref(), Some("login-a"));
    assert_eq!(app.completed_login_add_account_id, None);

    let mut stale_app = make_test_app().await;
    stale_app.pending_login_add_account_id = Some("login-a".to_string());
    stale_app.handle_login_add_account_completed(&login_completed(
        "login-a", /*success*/ true, /*error*/ None,
    ));
    assert_eq!(stale_app.completed_login_add_account_id, None);
}

#[tokio::test]
async fn login_success_waits_for_chatgpt_account_update() {
    let mut app = app_waiting_for_login("login-a").await;

    app.handle_login_add_account_completed(&login_completed(
        "login-a", /*success*/ true, /*error*/ None,
    ));
    assert_eq!(
        app.completed_login_add_account_id.as_deref(),
        Some("login-a")
    );

    app.maybe_complete_login_add_account(&account_updated(AuthMode::ApiKey));
    assert_eq!(app.pending_login_add_account_id.as_deref(), Some("login-a"));

    app.maybe_complete_login_add_account(&account_updated(AuthMode::Chatgpt));
    assert_eq!(app.pending_login_add_account_id, None);
    assert_eq!(app.completed_login_add_account_id, None);
}

#[tokio::test]
async fn failed_login_completion_clears_pending_attempt() {
    let mut app = app_waiting_for_login("login-a").await;

    app.handle_login_add_account_completed(&login_completed(
        "login-a",
        /*success*/ false,
        Some("login failed"),
    ));

    assert_eq!(app.pending_login_add_account_id, None);
    assert_eq!(app.completed_login_add_account_id, None);
}
