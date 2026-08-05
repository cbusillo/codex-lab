use super::external_capabilities::ExternalAgentCapabilities;
use super::external_capabilities::ExternalAgentCapabilityCacheKey;
use super::external_capabilities::ExternalAgentDiscoveryCacheKey;
use super::external_capabilities::antigravity_capabilities;
use super::external_capabilities::cache_capabilities;
use super::external_capabilities::cache_discovery;
use super::external_capabilities::cached_capabilities;
use super::external_capabilities::cached_discovery;
use super::external_capabilities::claude_capabilities;
use super::external_capabilities::record_active_capability_catalog;
use super::external_capabilities::validate_requested_capabilities;
use super::external_command::EXTERNAL_AGENT_TRUNCATED_MARKER;
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
const MAX_CAPABILITY_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_CLI_VERSION_BYTES: usize = 200;
const CARGO_TARGET_DIR_ENV_VAR: &str = "CARGO_TARGET_DIR";
const CODEX_LAB_CARGO_TARGET_DIR_ENV_VAR: &str = "CODEX_LAB_CARGO_TARGET_DIR";

struct ExternalAgentCapabilityProbe {
    capabilities: ExternalAgentCapabilities,
    resolved_command: Option<PathBuf>,
    command_args: Vec<String>,
}

#[derive(Clone, Copy)]
pub(super) enum ExternalAgentPreflightOutputLimit {
    Diagnostic,
    Capability,
}

impl ExternalAgentPreflightOutputLimit {
    fn bytes(self) -> usize {
        match self {
            Self::Diagnostic => MAX_PREFLIGHT_MESSAGE_BYTES,
            Self::Capability => MAX_CAPABILITY_OUTPUT_BYTES,
        }
    }
}

/// Discovers bounded local capabilities for a configured external agent.
pub async fn discover_external_agent_capabilities(
    backend: &ExternalCommandAgentBackendConfig,
    workspace: &Path,
) -> ExternalAgentCapabilities {
    let cache_key = ExternalAgentDiscoveryCacheKey::new(backend, workspace);
    if let Some(capabilities) = cached_discovery(&cache_key) {
        record_active_capability_catalog(backend, workspace, &capabilities);
        return capabilities;
    }
    let capabilities = probe_external_agent_backend(backend, workspace)
        .await
        .capabilities;
    cache_discovery(cache_key, &capabilities);
    record_active_capability_catalog(backend, workspace, &capabilities);
    capabilities
}

pub(crate) async fn preflight_external_agent_backend(
    agent_type: Option<&str>,
    backend: &ExternalCommandAgentBackendConfig,
    workspace: &Path,
    is_read_only: bool,
) -> Result<ExternalAgentProviderProvenance, ExternalAgentFailureDetail> {
    let probe = probe_external_agent_backend(backend, workspace).await;
    if let Some(failure) = probe.capabilities.failure.as_ref()
        && capability_failure_is_fatal(failure.kind)
    {
        return Err(failure.clone());
    }
    validate_requested_capabilities(
        backend,
        &probe.command_args,
        is_read_only,
        &probe.capabilities,
    )?;
    let resolved_command = probe.resolved_command.ok_or_else(|| {
        probe.capabilities.failure.clone().unwrap_or_else(|| {
            ExternalAgentFailureDetail::new(
                ExternalAgentFailureKind::LaunchFailed,
                "external agent capability probe did not resolve a command",
            )
        })
    })?;

    let mut provenance = ExternalAgentProviderProvenance::new(
        agent_type,
        backend,
        workspace,
        is_read_only,
        probe.capabilities.cli_version,
    );
    provenance.set_resolved_command(resolved_command);
    Ok(provenance)
}

async fn probe_external_agent_backend(
    backend: &ExternalCommandAgentBackendConfig,
    workspace: &Path,
) -> ExternalAgentCapabilityProbe {
    let cli_family = backend.launch_family.as_deref().unwrap_or("custom");
    let (command, command_args) = match configured_external_agent_command(backend) {
        Ok(command) => command,
        Err(error) => {
            return failed_capability_probe(
                cli_family,
                None,
                None,
                Vec::new(),
                ExternalAgentFailureDetail::new(
                    ExternalAgentFailureKind::LaunchFailed,
                    error.to_string(),
                ),
            );
        }
    };
    let launch_cwd = external_agent_preflight_cwd(backend, workspace);
    if backend.launch_family.as_deref() == Some("antigravity") {
        if !workspace.is_dir() {
            return failed_capability_probe(
                cli_family,
                None,
                None,
                command_args,
                ExternalAgentFailureDetail::new(
                    ExternalAgentFailureKind::LaunchFailed,
                    format!(
                        "antigravity workspace directory does not exist: {}",
                        workspace.display()
                    ),
                ),
            );
        }
        if let Err(error) = tokio::fs::create_dir_all(&launch_cwd).await {
            return failed_capability_probe(
                cli_family,
                None,
                None,
                command_args,
                ExternalAgentFailureDetail::new(
                    ExternalAgentFailureKind::LaunchFailed,
                    format!(
                        "failed to prepare Antigravity launch directory `{}`: {error}",
                        launch_cwd.display()
                    ),
                ),
            );
        }
    }
    let resolved_command =
        match resolve_external_agent_command(backend, command.as_path(), launch_cwd.as_path()) {
            Ok(command) => command,
            Err(failure) => {
                return failed_capability_probe(cli_family, None, None, command_args, failure);
            }
        };
    let cli_version = match capture_external_agent_cli_version(
        backend,
        &resolved_command,
        &command_args,
        &launch_cwd,
    )
    .await
    {
        Ok(version) => version,
        Err(failure) => {
            return failed_capability_probe(
                cli_family,
                None,
                Some(resolved_command),
                command_args,
                failure,
            );
        }
    };
    let cache_key = ExternalAgentCapabilityCacheKey::new(
        &resolved_command,
        &command_args,
        cli_family,
        cli_version.as_deref(),
    );
    if let Some(capabilities) = cached_capabilities(&cache_key) {
        return ExternalAgentCapabilityProbe {
            capabilities,
            resolved_command: Some(resolved_command),
            command_args,
        };
    }

    let capabilities = match backend.launch_family.as_deref() {
        Some("claude") => {
            if let Err(failure) =
                verify_external_agent_auth(backend, &resolved_command, &command_args, &launch_cwd)
                    .await
            {
                return failed_capability_probe(
                    cli_family,
                    cli_version,
                    Some(resolved_command),
                    command_args,
                    failure,
                );
            }
            match run_external_agent_preflight_command(
                backend,
                &resolved_command,
                &command_args,
                &launch_cwd,
                &["--help"],
                "capability",
                ExternalAgentPreflightOutputLimit::Capability,
            )
            .await
            {
                Ok(output) if output.status.success() => {
                    let help_output = combined_capability_output(&output);
                    claude_capabilities(cli_version, &help_output, output_was_truncated(&output))
                }
                Ok(output) => ExternalAgentCapabilities::conservative(
                    cli_family,
                    cli_version,
                    capability_failure_from_output(cli_family, "capability", &output),
                ),
                Err(failure) => {
                    ExternalAgentCapabilities::conservative(cli_family, cli_version, failure)
                }
            }
        }
        Some("antigravity") => {
            let models_output = match run_external_agent_preflight_command(
                backend,
                &resolved_command,
                &command_args,
                &launch_cwd,
                &["models"],
                "authentication",
                ExternalAgentPreflightOutputLimit::Capability,
            )
            .await
            {
                Ok(output) if output.status.success() => output,
                Ok(output) => {
                    return failed_capability_probe(
                        cli_family,
                        cli_version,
                        Some(resolved_command),
                        command_args,
                        authentication_failure_from_output(backend, &output),
                    );
                }
                Err(failure) => {
                    return failed_capability_probe(
                        cli_family,
                        cli_version,
                        Some(resolved_command),
                        command_args,
                        failure,
                    );
                }
            };
            match run_external_agent_preflight_command(
                backend,
                &resolved_command,
                &command_args,
                &launch_cwd,
                &["--help"],
                "capability",
                ExternalAgentPreflightOutputLimit::Capability,
            )
            .await
            {
                Ok(help_output) if help_output.status.success() => {
                    let combined_help_output = combined_capability_output(&help_output);
                    antigravity_capabilities(
                        cli_version,
                        &models_output.stdout,
                        output_was_truncated(&models_output),
                        &combined_help_output,
                        output_was_truncated(&help_output),
                    )
                }
                Ok(output) => ExternalAgentCapabilities::conservative(
                    cli_family,
                    cli_version,
                    capability_failure_from_output(cli_family, "capability", &output),
                ),
                Err(failure) => {
                    ExternalAgentCapabilities::conservative(cli_family, cli_version, failure)
                }
            }
        }
        _ => ExternalAgentCapabilities::not_probed(cli_family, cli_version),
    };
    cache_capabilities(cache_key, &capabilities);
    ExternalAgentCapabilityProbe {
        capabilities,
        resolved_command: Some(resolved_command),
        command_args,
    }
}

fn failed_capability_probe(
    cli_family: &str,
    cli_version: Option<String>,
    resolved_command: Option<PathBuf>,
    command_args: Vec<String>,
    failure: ExternalAgentFailureDetail,
) -> ExternalAgentCapabilityProbe {
    ExternalAgentCapabilityProbe {
        capabilities: ExternalAgentCapabilities::conservative(cli_family, cli_version, failure),
        resolved_command,
        command_args,
    }
}

fn capability_failure_is_fatal(kind: ExternalAgentFailureKind) -> bool {
    matches!(
        kind,
        ExternalAgentFailureKind::CommandMissing
            | ExternalAgentFailureKind::AuthenticationRequired
            | ExternalAgentFailureKind::QuotaOrRateLimited
            | ExternalAgentFailureKind::TimedOut
            | ExternalAgentFailureKind::LaunchFailed
            | ExternalAgentFailureKind::ProviderFailed
    )
}

fn output_was_truncated(output: &std::process::Output) -> bool {
    output.stdout.starts_with(EXTERNAL_AGENT_TRUNCATED_MARKER)
        || output.stderr.starts_with(EXTERNAL_AGENT_TRUNCATED_MARKER)
}

fn combined_capability_output(output: &std::process::Output) -> Vec<u8> {
    let mut combined = Vec::with_capacity(
        output
            .stdout
            .len()
            .saturating_add(output.stderr.len().saturating_add(1)),
    );
    combined.extend_from_slice(&output.stdout);
    if !output.stdout.is_empty() && !output.stderr.is_empty() {
        combined.push(b'\n');
    }
    combined.extend_from_slice(&output.stderr);
    combined
}

fn capability_failure_from_output(
    cli_family: &str,
    probe_name: &str,
    output: &std::process::Output,
) -> ExternalAgentFailureDetail {
    let output_text = bounded_preflight_output(&output.stdout, &output.stderr);
    let detail = if output_text.is_empty() {
        format!(
            "{cli_family} {probe_name} probe exited with {}",
            output.status
        )
    } else {
        format!("{cli_family} {probe_name} probe failed: {output_text}")
    };
    ExternalAgentFailureDetail::new(ExternalAgentFailureKind::MalformedOutput, detail)
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
        ExternalAgentPreflightOutputLimit::Diagnostic,
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
        _ => return Ok(()),
    };
    let output = run_external_agent_preflight_command(
        backend,
        resolved_command,
        command_args,
        workspace,
        args,
        probe_name,
        ExternalAgentPreflightOutputLimit::Diagnostic,
    )
    .await?;
    let claude_signed_out = backend.launch_family.as_deref() == Some("claude")
        && serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .ok()
            .and_then(|value| value.get("loggedIn").and_then(serde_json::Value::as_bool))
            == Some(false);
    if output.status.success() && !claude_signed_out {
        return Ok(());
    }

    Err(authentication_failure_from_output_with_signed_out(
        backend,
        &output,
        claude_signed_out,
    ))
}

fn authentication_failure_from_output(
    backend: &ExternalCommandAgentBackendConfig,
    output: &std::process::Output,
) -> ExternalAgentFailureDetail {
    authentication_failure_from_output_with_signed_out(backend, output, false)
}

fn authentication_failure_from_output_with_signed_out(
    backend: &ExternalCommandAgentBackendConfig,
    output: &std::process::Output,
    signed_out: bool,
) -> ExternalAgentFailureDetail {
    let output_text = bounded_preflight_output(&output.stdout, &output.stderr);
    let kind = if signed_out {
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
    ExternalAgentFailureDetail::new(kind, detail)
}

async fn run_external_agent_preflight_command(
    backend: &ExternalCommandAgentBackendConfig,
    resolved_command: &Path,
    command_args: &[String],
    workspace: &Path,
    args: &[&str],
    probe_name: &str,
    output_limit: ExternalAgentPreflightOutputLimit,
) -> Result<std::process::Output, ExternalAgentFailureDetail> {
    run_external_agent_preflight_command_with_timeout(
        backend,
        resolved_command,
        command_args,
        workspace,
        args,
        probe_name,
        output_limit,
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
    output_limit: ExternalAgentPreflightOutputLimit,
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
        let output_limit = output_limit.bytes();
        let (stdout, stderr, status) = tokio::try_join!(
            read_limited_output(stdout, output_limit, "preflight stdout"),
            read_limited_output(stderr, output_limit, "preflight stderr"),
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

#[cfg(test)]
#[path = "external_preflight_tests.rs"]
mod tests;

pub(super) fn antigravity_launch_dir() -> PathBuf {
    crate::config::find_codex_home()
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("code"))
        .join("agent-cache")
        .join("antigravity")
}
