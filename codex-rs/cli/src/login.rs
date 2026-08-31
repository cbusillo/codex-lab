//! CLI login commands and their direct-user observability surfaces.
//!
//! The TUI path already installs a broader tracing stack with feedback, OpenTelemetry, and other
//! interactive-session layers. Direct `codex login` intentionally does less: it preserves the
//! existing stderr/browser UX and adds only a small file-backed tracing layer for login-specific
//! targets. Keeping that setup local avoids pulling the TUI's session-oriented logging machinery
//! into a one-shot CLI command while still producing a durable `codex-login.log` artifact that
//! support can request from users.

use codex_config::types::AuthCredentialsStoreMode;
use codex_core::config::Config;
use codex_core::config::edit::ConfigEdit;
use codex_core::config::edit::ConfigEditsBuilder;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::AuthRouteConfig;
use codex_login::CLIENT_ID;
use codex_login::CodexAuth;
use codex_login::ServerOptions;
use codex_login::is_workload_identity_selected;
use codex_login::list_auth_profiles;
use codex_login::login_with_access_token;
use codex_login::login_with_api_key;
use codex_login::login_with_api_key_for_profile;
use codex_login::logout_with_revoke;
use codex_login::profile_home;
use codex_login::record_auth_profile_login;
use codex_login::run_device_code_login;
use codex_login::run_login_server;
use codex_login::run_profile_device_code_login;
use codex_login::run_profile_login_server;
use codex_protocol::auth::AuthMode;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_utils_cli::CliConfigOverrides;
use std::fs::OpenOptions;
use std::io::IsTerminal;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use tracing_appender::non_blocking;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const CHATGPT_LOGIN_DISABLED_MESSAGE: &str =
    "ChatGPT login is disabled. Use API key login instead.";
const API_KEY_LOGIN_DISABLED_MESSAGE: &str =
    "API key login is disabled. Use ChatGPT login instead.";
const ACCESS_TOKEN_LOGIN_DISABLED_MESSAGE: &str =
    "Access token login is disabled. Use API key login instead.";
const LOGIN_SUCCESS_MESSAGE: &str = "Successfully logged in";

struct LoginTarget {
    codex_home: PathBuf,
    profile: Option<String>,
}

impl LoginTarget {
    fn is_default(&self) -> bool {
        self.profile.is_none()
    }

    fn label(&self) -> String {
        self.profile
            .as_ref()
            .map(|profile| format!("profile `{profile}`"))
            .unwrap_or_else(|| "default profile".to_string())
    }
}

fn profile_suffix(target: &LoginTarget) -> String {
    if target.is_default() {
        String::new()
    } else {
        format!(" for {}", target.label())
    }
}

fn login_success_message(target: &LoginTarget) -> String {
    if target.is_default() {
        LOGIN_SUCCESS_MESSAGE.to_string()
    } else {
        format!(
            "Successfully logged in to {}; the GUI control account was not changed",
            target.label()
        )
    }
}

fn resolve_login_target(config: &Config, profile: Option<String>) -> std::io::Result<LoginTarget> {
    let codex_home = match profile.as_deref() {
        Some(profile_name) => profile_home(&config.codex_home, profile_name)?,
        None => config.codex_home.to_path_buf(),
    };
    Ok(LoginTarget {
        codex_home,
        profile,
    })
}

fn note_profile_login(
    config: &Config,
    target: &LoginTarget,
    auth: &CodexAuth,
) -> std::io::Result<()> {
    let Some(profile) = target.profile.as_deref() else {
        return Ok(());
    };
    record_auth_profile_login(
        &config.codex_home,
        profile,
        auth.get_account_id(),
        auth.get_account_email(),
    )?;
    Ok(())
}

async fn load_auth_for_target(
    config: &Config,
    target: &LoginTarget,
) -> std::io::Result<Option<CodexAuth>> {
    CodexAuth::from_auth_storage(
        &target.codex_home,
        config.cli_auth_credentials_store_mode,
        Some(&config.chatgpt_base_url),
        config.auth_keyring_backend_kind(),
        &config.auth_route_config(),
    )
    .await
}

async fn record_profile_after_login(config: &Config, target: &LoginTarget) -> std::io::Result<()> {
    if target.profile.is_none() {
        return Ok(());
    }
    match load_auth_for_target(config, target).await? {
        Some(auth) => note_profile_login(config, target, &auth),
        None => Err(std::io::Error::other(format!(
            "login completed but no credentials were stored for {}",
            target.label()
        ))),
    }
}

fn resolve_login_target_or_exit(config: &Config, profile: Option<String>) -> LoginTarget {
    match resolve_login_target(config, profile) {
        Ok(target) => target,
        Err(err) => {
            eprintln!("Error resolving login target: {err}");
            std::process::exit(1);
        }
    }
}

/// Installs a small file-backed tracing layer for direct `codex login` flows.
///
/// This deliberately duplicates a narrow slice of the TUI logging setup instead of reusing it
/// wholesale. The TUI stack includes session-oriented layers that are valuable for interactive
/// runs but unnecessary for a one-shot login command. Keeping the direct CLI path local lets this
/// command produce a durable `codex-login.log` artifact without coupling it to the TUI's broader
/// telemetry and feedback initialization.
fn init_login_file_logging(config: &Config) -> Option<WorkerGuard> {
    let log_dir = match codex_core::config::log_dir(config) {
        Ok(log_dir) => log_dir,
        Err(err) => {
            eprintln!("Warning: failed to resolve login log directory: {err}");
            return None;
        }
    };

    if let Err(err) = std::fs::create_dir_all(&log_dir) {
        eprintln!(
            "Warning: failed to create login log directory {}: {err}",
            log_dir.display()
        );
        return None;
    }

    let mut log_file_opts = OpenOptions::new();
    log_file_opts.create(true).append(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log_file_opts.mode(0o600);
    }

    let log_path = log_dir.join("codex-login.log");
    let log_file = match log_file_opts.open(&log_path) {
        Ok(log_file) => log_file,
        Err(err) => {
            eprintln!(
                "Warning: failed to open login log file {}: {err}",
                log_path.display()
            );
            return None;
        }
    };

    let (non_blocking, guard) = non_blocking(log_file);
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("codex_cli=info,codex_core=info,codex_login=info"));
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_target(true)
        .with_ansi(false)
        .with_filter(env_filter);

    // Direct `codex login` otherwise relies on ephemeral stderr and browser output.
    // Persist the same login targets to a file so support can inspect auth failures
    // without reproducing them through TUI or app-server.
    if let Err(err) = tracing_subscriber::registry().with(file_layer).try_init() {
        eprintln!(
            "Warning: failed to initialize login log file {}: {err}",
            log_path.display()
        );
        return None;
    }

    Some(guard)
}

fn print_login_server_start(actual_port: u16, auth_url: &str) {
    eprintln!(
        "Starting local login server on http://localhost:{actual_port}.\nIf your browser did not open, navigate to this URL to authenticate:\n\n{auth_url}\n\nOn a remote or headless machine? Use `codex login --device-auth` instead."
    );
}

async fn clear_existing_auth_before_login(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_keyring_backend_kind: AuthKeyringBackendKind,
    auth_route_config: &AuthRouteConfig,
) {
    if let Err(err) = logout_with_revoke(
        codex_home,
        auth_credentials_store_mode,
        auth_keyring_backend_kind,
        auth_route_config,
    )
    .await
    {
        tracing::warn!("failed to clear existing auth before login: {err}");
    }
}

async fn login_with_chatgpt(
    target: &LoginTarget,
    forced_chatgpt_workspace_id: Option<Vec<String>>,
    cli_auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_keyring_backend_kind: AuthKeyringBackendKind,
    auth_route_config: AuthRouteConfig,
) -> std::io::Result<()> {
    clear_existing_auth_before_login(
        &target.codex_home,
        cli_auth_credentials_store_mode,
        auth_keyring_backend_kind,
        &auth_route_config,
    )
    .await;

    let opts = ServerOptions::new(
        target.codex_home.clone(),
        CLIENT_ID.to_string(),
        forced_chatgpt_workspace_id,
        cli_auth_credentials_store_mode,
        auth_keyring_backend_kind,
        auth_route_config,
    );
    let server = if target.profile.is_some() {
        run_profile_login_server(opts)?
    } else {
        run_login_server(opts)?
    };

    print_login_server_start(server.actual_port, &server.auth_url);

    server.block_until_done().await
}

pub async fn run_login_with_chatgpt(
    cli_config_overrides: CliConfigOverrides,
    profile: Option<String>,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guard = init_login_file_logging(&config);
    tracing::info!("starting browser login flow");

    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Api)) {
        eprintln!("{CHATGPT_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }

    let target = resolve_login_target_or_exit(&config, profile);
    let forced_chatgpt_workspace_id = config.forced_chatgpt_workspace_id.clone();
    match login_with_chatgpt(
        &target,
        forced_chatgpt_workspace_id,
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
        config.auth_route_config(),
    )
    .await
    {
        Ok(_) => {
            if let Err(err) = record_profile_after_login(&config, &target).await {
                eprintln!("Error updating auth profile account data: {err}");
                std::process::exit(1);
            }
            eprintln!("{}", login_success_message(&target));
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("Error logging in: {e}");
            std::process::exit(1);
        }
    }
}

pub async fn run_login_with_api_key(
    cli_config_overrides: CliConfigOverrides,
    profile: Option<String>,
    api_key: String,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guard = init_login_file_logging(&config);
    tracing::info!("starting api key login flow");

    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Chatgpt)) {
        eprintln!("{API_KEY_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }

    let target = resolve_login_target_or_exit(&config, profile);

    let login_result = if target.profile.is_some() {
        login_with_api_key_for_profile(
            &target.codex_home,
            &api_key,
            config.cli_auth_credentials_store_mode,
            config.auth_keyring_backend_kind(),
        )
    } else {
        login_with_api_key(
            &target.codex_home,
            &api_key,
            config.cli_auth_credentials_store_mode,
            config.auth_keyring_backend_kind(),
        )
    };
    match login_result {
        Ok(_) => {
            if let Err(err) = record_profile_after_login(&config, &target).await {
                eprintln!("Error updating auth profile account data: {err}");
                std::process::exit(1);
            }
            eprintln!("{}", login_success_message(&target));
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("Error logging in: {e}");
            std::process::exit(1);
        }
    }
}

pub async fn run_login_with_access_token(
    cli_config_overrides: CliConfigOverrides,
    profile: Option<String>,
    access_token: String,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guard = init_login_file_logging(&config);
    tracing::info!("starting access token login flow");

    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Api)) {
        eprintln!("{ACCESS_TOKEN_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }

    let target = resolve_login_target_or_exit(&config, profile);
    let auth_route_config = config.auth_route_config();
    match login_with_access_token(
        &target.codex_home,
        &access_token,
        config.cli_auth_credentials_store_mode,
        config.forced_chatgpt_workspace_id.as_deref(),
        Some(&config.chatgpt_base_url),
        config.auth_keyring_backend_kind(),
        &auth_route_config,
    )
    .await
    {
        Ok(_) => {
            if let Err(err) = record_profile_after_login(&config, &target).await {
                eprintln!("Error updating auth profile account data: {err}");
                std::process::exit(1);
            }
            eprintln!("{}", login_success_message(&target));
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("Error logging in with access token: {e}");
            std::process::exit(1);
        }
    }
}

pub fn read_api_key_from_stdin() -> String {
    read_stdin_secret(
        "--with-api-key expects the API key on stdin. Try piping it, e.g. `printenv OPENAI_API_KEY | codex login --with-api-key`.",
        "Reading API key from stdin...",
        "No API key provided via stdin.",
    )
}

pub fn read_access_token_from_stdin() -> String {
    read_stdin_secret(
        "--with-access-token expects the access token on stdin. Try piping it, e.g. `printenv CODEX_ACCESS_TOKEN | codex login --with-access-token`.",
        "Reading access token from stdin...",
        "No access token provided via stdin.",
    )
}

fn read_stdin_secret(terminal_message: &str, reading_message: &str, empty_message: &str) -> String {
    let mut stdin = std::io::stdin();

    if stdin.is_terminal() {
        eprintln!("{terminal_message}");
        std::process::exit(1);
    }

    eprintln!("{reading_message}");

    let mut buffer = String::new();
    if let Err(err) = stdin.read_to_string(&mut buffer) {
        eprintln!("Failed to read stdin: {err}");
        std::process::exit(1);
    }

    let secret = buffer.trim().to_string();
    if secret.is_empty() {
        eprintln!("{empty_message}");
        std::process::exit(1);
    }

    secret
}

/// Login using the OAuth device code flow.
pub async fn run_login_with_device_code(
    cli_config_overrides: CliConfigOverrides,
    profile: Option<String>,
    issuer_base_url: Option<String>,
    client_id: Option<String>,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guard = init_login_file_logging(&config);
    tracing::info!("starting device code login flow");
    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Api)) {
        eprintln!("{CHATGPT_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }
    let target = resolve_login_target_or_exit(&config, profile);
    let auth_route_config = config.auth_route_config();
    clear_existing_auth_before_login(
        &target.codex_home,
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
        &auth_route_config,
    )
    .await;
    let forced_chatgpt_workspace_id = config.forced_chatgpt_workspace_id.clone();
    let mut opts = ServerOptions::new(
        target.codex_home.clone(),
        client_id.unwrap_or(CLIENT_ID.to_string()),
        forced_chatgpt_workspace_id,
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
        auth_route_config,
    );
    if let Some(iss) = issuer_base_url {
        opts.issuer = iss;
    }
    let login_result = if target.profile.is_some() {
        run_profile_device_code_login(opts).await
    } else {
        run_device_code_login(opts).await
    };
    match login_result {
        Ok(()) => {
            if let Err(err) = record_profile_after_login(&config, &target).await {
                eprintln!("Error updating auth profile account data: {err}");
                std::process::exit(1);
            }
            eprintln!("{}", login_success_message(&target));
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("Error logging in with device code: {e}");
            std::process::exit(1);
        }
    }
}

/// Prefers device-code login (with `open_browser = false`) when headless environment is detected, but keeps
/// `codex login` working in environments where device-code may be disabled/feature-gated.
/// If `run_device_code_login` returns `ErrorKind::NotFound` ("device-code unsupported"), this
/// falls back to starting the local browser login server.
pub async fn run_login_with_device_code_fallback_to_browser(
    cli_config_overrides: CliConfigOverrides,
    issuer_base_url: Option<String>,
    client_id: Option<String>,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let _login_log_guard = init_login_file_logging(&config);
    tracing::info!("starting login flow with device code fallback");
    if matches!(config.forced_login_method, Some(ForcedLoginMethod::Api)) {
        eprintln!("{CHATGPT_LOGIN_DISABLED_MESSAGE}");
        std::process::exit(1);
    }
    let auth_route_config = config.auth_route_config();
    clear_existing_auth_before_login(
        &config.codex_home,
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
        &auth_route_config,
    )
    .await;

    let forced_chatgpt_workspace_id = config.forced_chatgpt_workspace_id.clone();
    let mut opts = ServerOptions::new(
        config.codex_home.to_path_buf(),
        client_id.unwrap_or(CLIENT_ID.to_string()),
        forced_chatgpt_workspace_id,
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
        auth_route_config,
    );
    if let Some(iss) = issuer_base_url {
        opts.issuer = iss;
    }
    opts.open_browser = false;

    match run_device_code_login(opts.clone()).await {
        Ok(()) => {
            eprintln!("{LOGIN_SUCCESS_MESSAGE}");
            std::process::exit(0);
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                eprintln!("Device code login is not enabled; falling back to browser login.");
                match run_login_server(opts) {
                    Ok(server) => {
                        print_login_server_start(server.actual_port, &server.auth_url);
                        match server.block_until_done().await {
                            Ok(()) => {
                                eprintln!("{LOGIN_SUCCESS_MESSAGE}");
                                std::process::exit(0);
                            }
                            Err(e) => {
                                eprintln!("Error logging in: {e}");
                                std::process::exit(1);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Error logging in: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                eprintln!("Error logging in with device code: {e}");
                std::process::exit(1);
            }
        }
    }
}

pub async fn run_login_status(
    cli_config_overrides: CliConfigOverrides,
    profile: Option<String>,
) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;

    if is_workload_identity_selected() {
        match AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false).await {
            Ok(_) => {
                eprintln!("Logged in using workload identity");
                std::process::exit(0);
            }
            Err(err) => {
                eprintln!("Error checking login status: {err}");
                std::process::exit(1);
            }
        }
    }

    let target = resolve_login_target_or_exit(&config, profile);

    match load_auth_for_target(&config, &target).await {
        Ok(Some(auth)) => match auth.auth_mode() {
            AuthMode::ApiKey => match auth.get_token() {
                Ok(api_key) => {
                    eprintln!(
                        "Logged in using an API key{} - {}",
                        profile_suffix(&target),
                        safe_format_key(&api_key)
                    );
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("Unexpected error retrieving API key: {e}");
                    std::process::exit(1);
                }
            },
            AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => {
                eprintln!("Logged in using ChatGPT{}", profile_suffix(&target));
                std::process::exit(0);
            }
            AuthMode::Headers => {
                unreachable!("header auth cannot be loaded from auth storage")
            }
            AuthMode::AgentIdentity => {
                eprintln!("Logged in using access token{}", profile_suffix(&target));
                std::process::exit(0);
            }
            AuthMode::PersonalAccessToken => {
                eprintln!(
                    "Logged in using personal access token{}",
                    profile_suffix(&target)
                );
                std::process::exit(0);
            }
            AuthMode::BedrockApiKey => {
                eprintln!(
                    "Logged in using Amazon Bedrock API key{}",
                    profile_suffix(&target)
                );
                std::process::exit(0);
            }
            AuthMode::BedrockAccessKeys => {
                eprintln!("Logged in using Amazon Bedrock AWS access keys");
                std::process::exit(0);
            }
        },
        Ok(None) => {
            eprintln!("Not logged in");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Error checking login status: {e}");
            std::process::exit(1);
        }
    }
}

pub async fn run_login_profiles(cli_config_overrides: CliConfigOverrides) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let profiles = match list_auth_profiles(&config.codex_home) {
        Ok(profiles) => profiles,
        Err(err) => {
            eprintln!("Error listing auth profiles: {err}");
            std::process::exit(1);
        }
    };

    if profiles.is_empty() {
        eprintln!("No auth profiles configured");
        std::process::exit(0);
    }

    for profile in profiles {
        let mut details = Vec::new();
        if let Some(email) = profile.metadata.email.as_deref() {
            details.push(email.to_string());
        }
        if profile.metadata.priming_enabled == Some(true) {
            details.push("priming enabled".to_string());
        }
        let suffix = if details.is_empty() {
            String::new()
        } else {
            format!(" ({})", details.join(", "))
        };
        eprintln!("{}{}", profile.name, suffix);
    }
    std::process::exit(0);
}

pub async fn run_logout(cli_config_overrides: CliConfigOverrides, profile: Option<String>) -> ! {
    let config = load_config_or_exit(cli_config_overrides).await;
    let target = resolve_login_target_or_exit(&config, profile);
    let auth_route_config = config.auth_route_config();

    let logged_out = match logout_with_revoke(
        &target.codex_home,
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
        &auth_route_config,
    )
    .await
    {
        Ok(logged_out) => logged_out,
        Err(err) => {
            eprintln!("Error logging out: {err}");
            std::process::exit(1);
        }
    };
    remove_profile_metadata_after_logout(&config, &target);

    let cleared_bedrock_config =
        if let Some(paths) = ConfigEditsBuilder::bedrock_provider_config_paths_to_clear(&config) {
            let edits = paths
                .into_iter()
                .map(|segments| ConfigEdit::ClearPath { segments });
            if let Err(err) = ConfigEditsBuilder::for_config(&config)
                .with_edits(edits)
                .apply()
                .await
            {
                eprintln!("Error clearing Amazon Bedrock configuration after logout: {err}");
                std::process::exit(1);
            }
            true
        } else {
            false
        };

    if logged_out || cleared_bedrock_config {
        eprintln!("Successfully logged out");
    } else {
        eprintln!("Not logged in");
    }
    std::process::exit(0);
}

fn remove_profile_metadata_after_logout(config: &Config, target: &LoginTarget) {
    if let Some(profile) = target.profile.as_deref()
        && let Err(err) = codex_login::remove_auth_profile_metadata(&config.codex_home, profile)
    {
        eprintln!("Warning: failed to update auth profile metadata: {err}");
    }
}

async fn load_config_or_exit(cli_config_overrides: CliConfigOverrides) -> Config {
    let cli_overrides = match cli_config_overrides.parse_overrides() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error parsing -c overrides: {e}");
            std::process::exit(1);
        }
    };

    match Config::load_with_cli_overrides(cli_overrides).await {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Error loading configuration: {e}");
            std::process::exit(1);
        }
    }
}

fn safe_format_key(key: &str) -> String {
    if key.len() <= 13 {
        return "***".to_string();
    }
    let prefix = &key[..8];
    let suffix = &key[key.len() - 5..];
    format!("{prefix}***{suffix}")
}

#[cfg(test)]
mod tests {
    use codex_config::types::AuthCredentialsStoreMode;
    use codex_login::AuthKeyringBackendKind;
    use codex_login::load_auth_dot_json;
    use codex_login::login_with_api_key;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::LoginTarget;
    use super::clear_existing_auth_before_login;
    use super::login_success_message;
    use super::safe_format_key;
    use std::path::PathBuf;

    const TEST_AUTH_KEYRING_BACKEND: AuthKeyringBackendKind = AuthKeyringBackendKind::Direct;

    #[tokio::test]
    async fn clears_existing_auth_before_login() {
        let codex_home = tempdir().expect("create temporary Codex home");
        login_with_api_key(
            codex_home.path(),
            "sk-existing",
            AuthCredentialsStoreMode::Ephemeral,
            TEST_AUTH_KEYRING_BACKEND,
        )
        .expect("save existing auth");

        clear_existing_auth_before_login(
            codex_home.path(),
            AuthCredentialsStoreMode::Ephemeral,
            TEST_AUTH_KEYRING_BACKEND,
            &codex_login::test_support::transport_default_auth_route_config(),
        )
        .await;

        let auth = load_auth_dot_json(
            codex_home.path(),
            AuthCredentialsStoreMode::Ephemeral,
            TEST_AUTH_KEYRING_BACKEND,
        )
        .expect("load auth after cleanup");
        assert_eq!(auth, None);
    }

    #[test]
    fn formats_long_key() {
        let key = "sk-proj-1234567890ABCDE";
        assert_eq!(safe_format_key(key), "sk-proj-***ABCDE");
    }

    #[test]
    fn short_key_returns_stars() {
        let key = "sk-proj-12345";
        assert_eq!(safe_format_key(key), "***");
    }

    #[test]
    fn profile_login_message_explains_control_account_is_unchanged() {
        let target = LoginTarget {
            codex_home: PathBuf::from("unused"),
            profile: Some("Main".to_string()),
        };

        assert_eq!(
            login_success_message(&target),
            "Successfully logged in to profile `Main`; the GUI control account was not changed"
        );
    }
}
