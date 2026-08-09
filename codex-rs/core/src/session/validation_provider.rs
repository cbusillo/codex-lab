use std::path::Path;
use std::path::PathBuf;

use codex_config::MAX_VALIDATION_PROVIDER_TIMEOUT_MS;
use codex_config::ShellcheckValidationProviderConfig;
use codex_config::ValidationConfig;
use codex_git_utils::GitSha;
use codex_git_utils::get_git_repo_root;
use codex_git_utils::get_worktree_changed_files;
use codex_git_utils::get_worktree_changed_files_since;
use codex_utils_absolute_path::AbsolutePathBuf;
use tokio::io::AsyncReadExt;

use super::cargo_validation_provider::resolve_cargo_validation_command;

const SHELLCHECK_MAX_FILES: usize = 64;
const VALIDATION_PROVIDER_COMMAND_MAX_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum AutomaticValidationProviderKind {
    Cargo,
    Shellcheck,
}

#[derive(Debug)]
pub(crate) struct AutomaticValidationCommand {
    pub(crate) kind: AutomaticValidationProviderKind,
    pub(crate) command: Vec<String>,
    pub(crate) cwd: AbsolutePathBuf,
    pub(crate) execution_cwd: Option<AbsolutePathBuf>,
    pub(crate) execution_cwd_guard: Option<tempfile::TempDir>,
    pub(crate) timeout_ms: u64,
    pub(crate) changed_file_count: u32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum AutomaticValidationProviderSkipReason {
    ValidationDisabled,
    NoChangedFiles,
    NoApplicableProvider,
    UnsupportedEnvironment,
}

#[derive(Debug)]
pub(crate) struct AutomaticValidationProviderSkip {
    pub(crate) reason: AutomaticValidationProviderSkipReason,
    pub(crate) changed_file_count: Option<u32>,
}

#[derive(Debug)]
pub(crate) enum AutomaticValidationProviderResolution {
    Command(AutomaticValidationCommand),
    Skipped(AutomaticValidationProviderSkip),
}

#[derive(Debug)]
pub(crate) enum AutomaticValidationProviderErrorKind {
    Configuration,
    Infrastructure,
}

#[derive(Debug)]
pub(crate) struct AutomaticValidationProviderError {
    pub(crate) kind: AutomaticValidationProviderErrorKind,
    pub(crate) command: Vec<String>,
    pub(crate) cwd: Option<AbsolutePathBuf>,
    pub(crate) message: String,
}

pub(crate) fn automatic_validation_provider_enabled(config: &ValidationConfig) -> bool {
    config.groups.functional
        && (config.providers.cargo.enabled || config.providers.shellcheck.enabled)
}

pub(crate) async fn resolve_automatic_validation_provider(
    config: &ValidationConfig,
    cwd: &AbsolutePathBuf,
    base_commit: Option<&str>,
) -> Result<AutomaticValidationProviderResolution, AutomaticValidationProviderError> {
    if !automatic_validation_provider_enabled(config) {
        return Ok(AutomaticValidationProviderResolution::Skipped(
            AutomaticValidationProviderSkip {
                reason: AutomaticValidationProviderSkipReason::ValidationDisabled,
                changed_file_count: None,
            },
        ));
    }

    let Some(repo_root) = get_git_repo_root(cwd.as_ref()) else {
        return Ok(AutomaticValidationProviderResolution::Skipped(
            AutomaticValidationProviderSkip {
                reason: AutomaticValidationProviderSkipReason::UnsupportedEnvironment,
                changed_file_count: None,
            },
        ));
    };
    let repo_root = dunce::canonicalize(&repo_root).unwrap_or(repo_root);
    let provider_cwd = AbsolutePathBuf::try_from(repo_root.clone()).map_err(|error| {
        AutomaticValidationProviderError {
            kind: AutomaticValidationProviderErrorKind::Infrastructure,
            command: configured_provider_command(config),
            cwd: Some(cwd.clone()),
            message: format!("failed to resolve automatic validation repository root: {error}"),
        }
    })?;
    let changed_files = match base_commit {
        Some(base_commit) => {
            get_worktree_changed_files_since(&repo_root, &GitSha::new(base_commit)).await
        }
        None => get_worktree_changed_files(&repo_root).await,
    }
    .ok_or_else(|| AutomaticValidationProviderError {
        kind: AutomaticValidationProviderErrorKind::Infrastructure,
        command: configured_provider_command(config),
        cwd: Some(provider_cwd.clone()),
        message: "failed to enumerate changed files for automatic validation".to_string(),
    })?;
    let changed_file_count = u32::try_from(changed_files.len()).unwrap_or(u32::MAX);
    if changed_files.is_empty() {
        return Ok(AutomaticValidationProviderResolution::Skipped(
            AutomaticValidationProviderSkip {
                reason: AutomaticValidationProviderSkipReason::NoChangedFiles,
                changed_file_count: Some(changed_file_count),
            },
        ));
    }
    if config.providers.cargo.enabled
        && let Some(command) = resolve_cargo_validation_command(
            &config.providers.cargo,
            &provider_cwd,
            &changed_files,
            changed_file_count,
        )
        .await?
    {
        return Ok(AutomaticValidationProviderResolution::Command(command));
    }
    if config.providers.shellcheck.enabled {
        let matching_files = matching_shellcheck_files(&repo_root, &changed_files).await;
        if !matching_files.is_empty() {
            return build_shellcheck_command(
                &config.providers.shellcheck,
                provider_cwd,
                matching_files,
                changed_file_count,
            )
            .map(AutomaticValidationProviderResolution::Command);
        }
    }

    Ok(AutomaticValidationProviderResolution::Skipped(
        AutomaticValidationProviderSkip {
            reason: AutomaticValidationProviderSkipReason::NoApplicableProvider,
            changed_file_count: Some(changed_file_count),
        },
    ))
}

fn configured_provider_command(config: &ValidationConfig) -> Vec<String> {
    match (
        config.providers.cargo.enabled,
        config.providers.shellcheck.enabled,
    ) {
        (true, false) => config.providers.cargo.command.clone(),
        (false, true) => config.providers.shellcheck.command.clone(),
        _ => Vec::new(),
    }
}

async fn matching_shellcheck_files(repo_root: &Path, changed_files: &[PathBuf]) -> Vec<PathBuf> {
    let mut matching_files = Vec::new();
    for path in changed_files {
        if path.extension().and_then(|extension| extension.to_str()) == Some("sh")
            || file_has_shell_shebang(&repo_root.join(path)).await
        {
            matching_files.push(path.clone());
        }
    }
    matching_files.sort();
    matching_files.dedup();
    matching_files
}

async fn file_has_shell_shebang(path: &Path) -> bool {
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return false;
    };
    let mut prefix = [0_u8; 256];
    let Ok(bytes_read) = file.read(&mut prefix).await else {
        return false;
    };
    let first_line = prefix[..bytes_read]
        .split(|byte| *byte == b'\n')
        .next()
        .and_then(|line| std::str::from_utf8(line).ok());
    first_line.is_some_and(is_shell_shebang)
}

fn is_shell_shebang(line: &str) -> bool {
    let Some(command) = line.strip_prefix("#!") else {
        return false;
    };
    let mut arguments = command.split_ascii_whitespace();
    let Some(program) = arguments.next() else {
        return false;
    };
    let program = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str());

    match program {
        Some("sh" | "bash" | "dash" | "ksh") => true,
        Some("env") => arguments
            .find(|argument| !argument.starts_with('-') && !argument.contains('='))
            .is_some_and(is_supported_shell_name),
        Some("busybox") => arguments.next().is_some_and(|argument| argument == "sh"),
        _ => false,
    }
}

fn is_supported_shell_name(argument: &str) -> bool {
    Path::new(argument)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "sh" | "bash" | "dash" | "ksh"))
}

fn build_shellcheck_command(
    config: &ShellcheckValidationProviderConfig,
    cwd: AbsolutePathBuf,
    matching_files: Vec<PathBuf>,
    changed_file_count: u32,
) -> Result<AutomaticValidationCommand, AutomaticValidationProviderError> {
    let mut command = config.command.clone();
    if command
        .first()
        .is_none_or(|program| program.trim().is_empty())
    {
        return Err(configuration_error(
            command,
            cwd,
            "validation.providers.shellcheck.command must include a non-empty executable",
        ));
    }
    if config.timeout_ms == 0 || config.timeout_ms > MAX_VALIDATION_PROVIDER_TIMEOUT_MS {
        return Err(configuration_error(
            command,
            cwd,
            format!(
                "validation.providers.shellcheck.timeout_ms must be between 1 and {MAX_VALIDATION_PROVIDER_TIMEOUT_MS}"
            ),
        ));
    }
    if matching_files.len() > SHELLCHECK_MAX_FILES {
        return Err(infrastructure_error(
            command,
            cwd,
            format!("shellcheck validation matched more than {SHELLCHECK_MAX_FILES} changed files"),
        ));
    }

    command.extend(["-f".to_string(), "gcc".to_string()]);
    for path in matching_files {
        let Some(path) = path.to_str() else {
            return Err(infrastructure_error(
                command,
                cwd,
                "shellcheck validation requires UTF-8 changed-file paths",
            ));
        };
        command.push(path.to_string());
    }
    let command_bytes = command.iter().fold(0usize, |total, argument| {
        total.saturating_add(argument.len() + 1)
    });
    if command_bytes > VALIDATION_PROVIDER_COMMAND_MAX_BYTES {
        return Err(infrastructure_error(
            command,
            cwd,
            format!(
                "shellcheck validation command must not exceed {VALIDATION_PROVIDER_COMMAND_MAX_BYTES} bytes"
            ),
        ));
    }

    Ok(AutomaticValidationCommand {
        kind: AutomaticValidationProviderKind::Shellcheck,
        command,
        cwd,
        execution_cwd: None,
        execution_cwd_guard: None,
        timeout_ms: config.timeout_ms,
        changed_file_count,
    })
}

pub(super) fn configuration_error(
    command: Vec<String>,
    cwd: AbsolutePathBuf,
    message: impl Into<String>,
) -> AutomaticValidationProviderError {
    AutomaticValidationProviderError {
        kind: AutomaticValidationProviderErrorKind::Configuration,
        command,
        cwd: Some(cwd),
        message: message.into(),
    }
}

pub(super) fn infrastructure_error(
    command: Vec<String>,
    cwd: AbsolutePathBuf,
    message: impl Into<String>,
) -> AutomaticValidationProviderError {
    AutomaticValidationProviderError {
        kind: AutomaticValidationProviderErrorKind::Infrastructure,
        command,
        cwd: Some(cwd),
        message: message.into(),
    }
}

#[cfg(test)]
#[path = "validation_provider_tests.rs"]
mod tests;
