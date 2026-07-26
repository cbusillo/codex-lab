use super::external_command::ExternalAgentChildGuard;
use super::external_command::MAX_PREFLIGHT_MESSAGE_BYTES;
use super::external_command::bounded_preflight_output;
use super::external_command::read_limited_output;
use super::external_command::split_command_and_args;
use super::external_diagnostics::ExternalAgentFailureDetail;
use super::external_diagnostics::ExternalAgentFailureKind;
use super::external_diagnostics::ExternalAgentProviderProvenance;
use super::external_diagnostics::classify_provider_failure_text;
use crate::config::ExternalCommandAgentBackendConfig;
use crate::config::ExternalCommandProtocol;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const GITHUB_COPILOT_VERSION_MARKER: &[u8] = b"GitHub Copilot CLI";
const THIRD_PARTY_CLI_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CLI_VERSION_BYTES: usize = 200;
const CARGO_TARGET_DIR_ENV_VAR: &str = "CARGO_TARGET_DIR";
const CODEX_LAB_CARGO_TARGET_DIR_ENV_VAR: &str = "CODEX_LAB_CARGO_TARGET_DIR";

pub(crate) async fn preflight_external_agent_backend(
    agent_type: Option<&str>,
    backend: &ExternalCommandAgentBackendConfig,
    workspace: &Path,
    is_read_only: bool,
) -> Result<ExternalAgentProviderProvenance, ExternalAgentFailureDetail> {
    let (command, command_args) = configured_external_agent_command(backend).map_err(|error| {
        ExternalAgentFailureDetail::new(ExternalAgentFailureKind::LaunchFailed, error.to_string())
    })?;
    let launch_cwd = external_agent_preflight_cwd(backend, workspace);
    if backend.launch_family.as_deref() == Some("antigravity") {
        if !workspace.is_dir() {
            return Err(ExternalAgentFailureDetail::new(
                ExternalAgentFailureKind::LaunchFailed,
                format!(
                    "antigravity workspace directory does not exist: {}",
                    workspace.display()
                ),
            ));
        }
        tokio::fs::create_dir_all(&launch_cwd)
            .await
            .map_err(|error| {
                ExternalAgentFailureDetail::new(
                    ExternalAgentFailureKind::LaunchFailed,
                    format!(
                        "failed to prepare Antigravity launch directory `{}`: {error}",
                        launch_cwd.display()
                    ),
                )
            })?;
    }
    let resolved_command =
        resolve_external_agent_command(backend, command.as_path(), launch_cwd.as_path())?;
    let cli_version =
        capture_external_agent_cli_version(backend, &resolved_command, &command_args, &launch_cwd)
            .await?;
    verify_external_agent_auth(backend, &resolved_command, &command_args, &launch_cwd).await?;

    let mut provenance = ExternalAgentProviderProvenance::new(
        agent_type,
        backend,
        workspace,
        is_read_only,
        cli_version,
    );
    provenance.set_resolved_command(resolved_command);
    Ok(provenance)
}

fn configured_external_agent_command(
    backend: &ExternalCommandAgentBackendConfig,
) -> anyhow::Result<(PathBuf, Vec<String>)> {
    let (command, args) = match backend.protocol {
        ExternalCommandProtocol::Json => (backend.command.trim().to_string(), Vec::new()),
        ExternalCommandProtocol::RawCli => split_command_and_args(&backend.command)?,
    };
    if command.is_empty() {
        return Err(anyhow::anyhow!(
            "external_command backend command must not be empty"
        ));
    }
    Ok((PathBuf::from(command), args))
}

fn resolve_external_agent_command(
    backend: &ExternalCommandAgentBackendConfig,
    command: &Path,
    cwd: &Path,
) -> Result<PathBuf, ExternalAgentFailureDetail> {
    let search_path = backend
        .env
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| OsString::from(value))
        .or_else(|| std::env::var_os("PATH"));
    which::which_in(command, search_path.as_ref(), cwd).map_err(|_| {
        let command = command.display();
        let agent_name = third_party_agent_display_name(backend.launch_family.as_deref());
        let hint = install_hint_for_third_party_agent(
            backend.launch_family.as_deref(),
            &command.to_string(),
        );
        ExternalAgentFailureDetail::new(
            ExternalAgentFailureKind::CommandMissing,
            format!("{agent_name} command `{command}` was not found or is not executable. {hint}"),
        )
    })
}

fn external_agent_preflight_cwd(
    backend: &ExternalCommandAgentBackendConfig,
    workspace: &Path,
) -> PathBuf {
    if backend.launch_family.as_deref() == Some("antigravity") {
        antigravity_launch_dir()
    } else {
        workspace.to_path_buf()
    }
}

async fn capture_external_agent_cli_version(
    backend: &ExternalCommandAgentBackendConfig,
    resolved_command: &Path,
    command_args: &[String],
    workspace: &Path,
) -> Result<Option<String>, ExternalAgentFailureDetail> {
    if !is_builtin_third_party_agent_family(backend.launch_family.as_deref()) {
        return Ok(None);
    }

    let output = run_external_agent_preflight_command(
        backend,
        resolved_command,
        command_args,
        workspace,
        &["--version"],
        "version",
    )
    .await?;
    if backend.launch_family.as_deref() == Some("copilot")
        && (!output.status.success()
            || !github_copilot_version_output(&output.stdout, &output.stderr))
    {
        return Err(ExternalAgentFailureDetail::new(
            ExternalAgentFailureKind::LaunchFailed,
            format!(
                "GitHub Copilot CLI command `{}` resolved to a different `copilot` executable. Install GitHub Copilot CLI and ensure its `copilot` command appears first on PATH.",
                resolved_command.display()
            ),
        ));
    }
    if !output.status.success() {
        return Ok(None);
    }

    Ok(first_bounded_output_line(&output.stdout, &output.stderr))
}

async fn verify_external_agent_auth(
    backend: &ExternalCommandAgentBackendConfig,
    resolved_command: &Path,
    command_args: &[String],
    workspace: &Path,
) -> Result<(), ExternalAgentFailureDetail> {
    let (args, probe_name) = match backend.launch_family.as_deref() {
        Some("claude") => (&["auth", "status"][..], "authentication"),
        Some("antigravity") => (&["models"][..], "authentication"),
        _ => return Ok(()),
    };
    let output = run_external_agent_preflight_command(
        backend,
        resolved_command,
        command_args,
        workspace,
        args,
        probe_name,
    )
    .await?;
    let output_text = bounded_preflight_output(&output.stdout, &output.stderr);
    let claude_signed_out = backend.launch_family.as_deref() == Some("claude")
        && serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .ok()
            .and_then(|value| value.get("loggedIn").and_then(serde_json::Value::as_bool))
            == Some(false);
    if output.status.success() && !claude_signed_out {
        return Ok(());
    }

    let kind = if claude_signed_out {
        ExternalAgentFailureKind::AuthenticationRequired
    } else {
        match classify_provider_failure_text(&output_text) {
            ExternalAgentFailureKind::ProviderFailed => ExternalAgentFailureKind::LaunchFailed,
            kind => kind,
        }
    };
    let agent_name = third_party_agent_display_name(backend.launch_family.as_deref());
    let detail = if output_text.is_empty() {
        format!(
            "{agent_name} authentication preflight exited with {}",
            output.status
        )
    } else {
        format!("{agent_name} authentication preflight failed: {output_text}")
    };
    Err(ExternalAgentFailureDetail::new(kind, detail))
}

async fn run_external_agent_preflight_command(
    backend: &ExternalCommandAgentBackendConfig,
    resolved_command: &Path,
    command_args: &[String],
    workspace: &Path,
    args: &[&str],
    probe_name: &str,
) -> Result<std::process::Output, ExternalAgentFailureDetail> {
    run_external_agent_preflight_command_with_timeout(
        backend,
        resolved_command,
        command_args,
        workspace,
        args,
        probe_name,
        THIRD_PARTY_CLI_PREFLIGHT_TIMEOUT,
    )
    .await
}

pub(super) async fn run_external_agent_preflight_command_with_timeout(
    backend: &ExternalCommandAgentBackendConfig,
    resolved_command: &Path,
    command_args: &[String],
    workspace: &Path,
    args: &[&str],
    probe_name: &str,
    timeout: Duration,
) -> Result<std::process::Output, ExternalAgentFailureDetail> {
    let mut command = Command::new(resolved_command);
    command
        .args(command_args)
        .args(args)
        .current_dir(workspace)
        .envs(external_agent_backend_env(backend))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let child = command.spawn().map_err(|error| {
        ExternalAgentFailureDetail::new(
            if error.kind() == std::io::ErrorKind::NotFound {
                ExternalAgentFailureKind::CommandMissing
            } else {
                ExternalAgentFailureKind::LaunchFailed
            },
            format!(
                "failed to run {probe_name} preflight for `{}`: {error}",
                resolved_command.display()
            ),
        )
    })?;
    let mut child = ExternalAgentChildGuard::new(child);
    let stdout = child.stdout.take().ok_or_else(|| {
        ExternalAgentFailureDetail::new(
            ExternalAgentFailureKind::LaunchFailed,
            format!(
                "failed to capture {probe_name} preflight stdout for `{}`",
                resolved_command.display()
            ),
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ExternalAgentFailureDetail::new(
            ExternalAgentFailureKind::LaunchFailed,
            format!(
                "failed to capture {probe_name} preflight stderr for `{}`",
                resolved_command.display()
            ),
        )
    })?;
    let interaction = async move {
        let (stdout, stderr, status) = tokio::try_join!(
            read_limited_output(stdout, MAX_PREFLIGHT_MESSAGE_BYTES, "preflight stdout"),
            read_limited_output(stderr, MAX_PREFLIGHT_MESSAGE_BYTES, "preflight stderr"),
            async { child.wait().await.map_err(anyhow::Error::from) },
        )?;
        child.disarm();
        Ok::<std::process::Output, anyhow::Error>(std::process::Output {
            status,
            stdout,
            stderr,
        })
    };
    tokio::time::timeout(timeout, interaction)
        .await
        .map_err(|_| {
            ExternalAgentFailureDetail::new(
                ExternalAgentFailureKind::TimedOut,
                format!(
                    "timed out while running {probe_name} preflight for `{}`",
                    resolved_command.display()
                ),
            )
        })?
        .map_err(|error| {
            ExternalAgentFailureDetail::new(
                ExternalAgentFailureKind::LaunchFailed,
                format!(
                    "failed while running {probe_name} preflight for `{}`: {error}",
                    resolved_command.display()
                ),
            )
        })
}

fn external_agent_backend_env(
    backend: &ExternalCommandAgentBackendConfig,
) -> HashMap<String, String> {
    let mut env = backend.env.clone();
    env.remove(CARGO_TARGET_DIR_ENV_VAR);
    env.remove(CODEX_LAB_CARGO_TARGET_DIR_ENV_VAR);
    env
}

fn first_bounded_output_line(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    for output in [stdout, stderr] {
        let output = String::from_utf8_lossy(output);
        if let Some(line) = output.lines().map(str::trim).find(|line| !line.is_empty()) {
            return Some(truncate_utf8(line, MAX_CLI_VERSION_BYTES));
        }
    }
    None
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

pub(super) fn github_copilot_version_output(stdout: &[u8], stderr: &[u8]) -> bool {
    [stdout, stderr].into_iter().any(|output| {
        output
            .windows(GITHUB_COPILOT_VERSION_MARKER.len())
            .any(|window| window == GITHUB_COPILOT_VERSION_MARKER)
    })
}

fn is_builtin_third_party_agent_family(launch_family: Option<&str>) -> bool {
    matches!(
        launch_family,
        Some("antigravity" | "claude" | "copilot" | "qwen")
    )
}

fn third_party_agent_display_name(launch_family: Option<&str>) -> &'static str {
    match launch_family {
        Some("antigravity") => "Antigravity CLI",
        Some("claude") => "Claude Code",
        Some("copilot") => "GitHub Copilot CLI",
        Some("qwen") => "Qwen Code",
        _ => "Third-party agent CLI",
    }
}

fn install_hint_for_third_party_agent(launch_family: Option<&str>, command: &str) -> String {
    match launch_family {
        Some("claude") => {
            format!("Install claude-code and make sure `{command}` is on PATH.")
        }
        Some("qwen") => format!("Install qwen-code and make sure `{command}` is on PATH."),
        Some("antigravity") => {
            format!("Install Antigravity CLI and make sure `{command}` is on PATH.")
        }
        Some("copilot") => {
            format!("Install GitHub Copilot CLI and make sure `{command}` is on PATH.")
        }
        _ => format!("Install `{command}` and make sure it is on PATH."),
    }
}

pub(super) fn antigravity_launch_dir() -> PathBuf {
    crate::config::find_codex_home()
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("code"))
        .join("agent-cache")
        .join("antigravity")
}
