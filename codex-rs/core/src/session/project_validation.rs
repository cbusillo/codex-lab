use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use codex_config::MAX_PROJECT_VALIDATION_TIMEOUT_MS;
use codex_config::ProjectValidationCommand;
use codex_git_utils::get_git_repo_root;
use codex_git_utils::get_head_commit_hash;
use codex_git_utils::get_worktree_diff_fingerprint;
use codex_git_utils::resolve_root_git_project_for_trust;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::SandboxErr;
use codex_protocol::exec_output::ExecToolCallOutput;
use codex_protocol::models::SandboxPermissions;
use codex_protocol::protocol::ProjectValidationCompletedEvent;
use codex_protocol::protocol::ProjectValidationStatus;
use codex_utils_absolute_path::AbsolutePathBuf;
use tokio_util::sync::CancellationToken;

use super::Session;
use super::turn_context::TurnContext;
use super::validation_provider::AutomaticValidationCommand;
use super::validation_provider::AutomaticValidationProviderError;
use super::validation_provider::AutomaticValidationProviderErrorKind;
use super::validation_provider::automatic_validation_provider_enabled;
use super::validation_provider::resolve_automatic_validation_provider;
use crate::exec::ExecCapturePolicy;
use crate::exec::ExecExpiration;
use crate::exec::ExecParams;
use crate::exec::process_exec_tool_call;
use crate::exec_env::create_env;

const PROJECT_VALIDATION_OUTPUT_MAX_BYTES: usize = 8 * 1024;
const PROJECT_VALIDATION_COMMAND_MAX_BYTES: usize = 8 * 1024;
const OUTPUT_TRUNCATED_MARKER: &str = "\n… project validation output truncated …\n";

pub(crate) enum ProjectValidationRun {
    Skipped,
    Completed(ProjectValidationCompletedEvent),
    Cancelled,
}

pub(crate) enum ProjectValidationAttempt {
    Initial {
        worktree_at_turn_start: Option<ProjectValidationWorktreeFingerprint>,
        model_used_tools: bool,
    },
    CorrectionRerun {
        worktree_at_turn_start: Option<ProjectValidationWorktreeFingerprint>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProjectValidationWorktreeFingerprint {
    head_commit: String,
    worktree_diff: Option<String>,
}

enum ValidationCommandKind {
    ProjectCommand,
    Shellcheck,
}

struct ValidationCommandPlan {
    kind: ValidationCommandKind,
    command: Vec<String>,
    cwd: AbsolutePathBuf,
    timeout_ms: u64,
}

pub(crate) async fn project_validation_worktree_fingerprint(
    turn_context: &TurnContext,
) -> Option<ProjectValidationWorktreeFingerprint> {
    if turn_context.config.validation.project_command.is_none()
        && !automatic_validation_provider_enabled(&turn_context.config.validation)
    {
        return None;
    }
    if turn_context.session_source.is_non_root_agent() {
        return None;
    }
    let cwd = turn_context.environments.single_local_environment_cwd()?;
    capture_worktree_fingerprint(&cwd).await
}

pub(crate) async fn run_project_validation(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    attempt: ProjectValidationAttempt,
    cancellation_token: CancellationToken,
) -> ProjectValidationRun {
    let configured_project_command = turn_context.config.validation.project_command.as_ref();
    if configured_project_command.is_none()
        && !automatic_validation_provider_enabled(&turn_context.config.validation)
    {
        return ProjectValidationRun::Skipped;
    }
    if turn_context.session_source.is_non_root_agent() {
        return ProjectValidationRun::Skipped;
    }

    let project_command = match configured_project_command {
        Some(configured) => match validate_project_command(turn_context, configured) {
            Ok(command) => Some(command),
            Err(event) => return ProjectValidationRun::Completed(event),
        },
        None => None,
    };

    let Some(cwd) = turn_context
        .environments
        .single_local_environment_cwd()
    else {
        let command = project_command.unwrap_or_else(|| {
            turn_context
                .config
                .validation
                .providers
                .shellcheck
                .command
                .clone()
        });
        return ProjectValidationRun::Completed(completed_event(
            turn_context,
            command,
            None,
            ProjectValidationStatus::InfrastructureFailure,
            None,
            "project validation requires exactly one local turn environment".to_string(),
            Duration::ZERO,
        ));
    };

    if cancellation_token.is_cancelled() {
        return ProjectValidationRun::Cancelled;
    }

    let env = create_env(&turn_context.config.permissions.shell_environment_policy, Some(sess.thread_id));
    let search_path = env
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| OsString::from(value));
    if let Some(command) = project_command.as_ref()
        && which::which_in(&command[0], search_path.as_ref(), cwd.as_ref()).is_err()
    {
        return ProjectValidationRun::Completed(configuration_error(
            turn_context,
            command.clone(),
            None,
            "project validation executable was not found or is not executable",
        ));
    }

    match initial_attempt_worktree_unchanged(&cwd, &attempt, &cancellation_token).await {
        Some(true) => return ProjectValidationRun::Skipped,
        Some(false) => {}
        None => return ProjectValidationRun::Cancelled,
    }

    let _lease = if let Some(repo_root) = project_validation_lease_root(turn_context, &cwd).await {
        let Some(lease) = sess
            .services
            .project_validation_coordinator
            .acquire(repo_root, &cancellation_token)
            .await
        else {
            return ProjectValidationRun::Cancelled;
        };
        Some(lease)
    } else {
        None
    };

    match initial_attempt_worktree_unchanged(&cwd, &attempt, &cancellation_token).await {
        Some(true) => return ProjectValidationRun::Skipped,
        Some(false) => {}
        None => return ProjectValidationRun::Cancelled,
    }

    if cancellation_token.is_cancelled() {
        return ProjectValidationRun::Cancelled;
    }

    let plan = if let Some(configured) = configured_project_command {
        let Some(command) = project_command else {
            return ProjectValidationRun::Completed(completed_event(
                turn_context,
                Vec::new(),
                Some(cwd),
                ProjectValidationStatus::InfrastructureFailure,
                None,
                "validated project command was unavailable".to_string(),
                Duration::ZERO,
            ));
        };
        ValidationCommandPlan {
            kind: ValidationCommandKind::ProjectCommand,
            command,
            cwd: cwd.clone(),
            timeout_ms: configured.timeout_ms,
        }
    } else {
        let automatic = match resolve_automatic_validation_provider(
            &turn_context.config.validation,
            &cwd,
            attempt_worktree_start(&attempt).map(|worktree| worktree.head_commit.as_str()),
        )
        .await
        {
            Ok(Some(command)) => command,
            Ok(None) => return ProjectValidationRun::Skipped,
            Err(error) => {
                return ProjectValidationRun::Completed(provider_error_event(turn_context, error));
            }
        };
        if which::which_in(
            &automatic.command[0],
            search_path.as_ref(),
            automatic.cwd.as_ref(),
        )
        .is_err()
        {
            return ProjectValidationRun::Completed(configuration_error(
                turn_context,
                automatic.command,
                Some(automatic.cwd),
                "shellcheck validation executable was not found or is not executable",
            ));
        }
        automatic_validation_plan(automatic)
    };

    let params = ExecParams {
        command: plan.command.clone(),
        cwd: plan.cwd.clone(),
        expiration: ExecExpiration::TimeoutOrCancellation {
            timeout: Duration::from_millis(plan.timeout_ms),
            cancellation: cancellation_token.clone(),
        },
        capture_policy: ExecCapturePolicy::ShellTool,
        env,
        network: turn_context.network.clone(),
        network_environment_id: turn_context
            .environments
            .single_local_environment()
            .map(|environment| environment.environment_id.clone()),
        sandbox_permissions: SandboxPermissions::UseDefault,
        windows_sandbox_level: turn_context.windows_sandbox_level,
        windows_sandbox_private_desktop: turn_context
            .config
            .permissions
            .windows_sandbox_private_desktop,
        justification: None,
        arg0: None,
    };
    let result = process_exec_tool_call(
        params,
        &turn_context.permission_profile,
        &plan.cwd,
        &turn_context.config.effective_workspace_roots(),
        &turn_context.config.codex_linux_sandbox_exe,
        turn_context.config.features.use_legacy_landlock(),
        None,
    )
    .await;

    if cancellation_token.is_cancelled() {
        return ProjectValidationRun::Cancelled;
    }

    ProjectValidationRun::Completed(match result {
        Ok(output) if output.timed_out => completed_from_output(
            turn_context,
            plan.command,
            plan.cwd,
            ProjectValidationStatus::TimedOut,
            output,
        ),
        Ok(output) => {
            let status = if output.exit_code == 0 {
                ProjectValidationStatus::Passed
            } else {
                ProjectValidationStatus::ActionableFailure
            };
            completed_from_output(turn_context, plan.command, plan.cwd, status, output)
        }
        Err(error) => match error.details() {
            CodexErrorDetails::Sandbox(SandboxErr::Timeout { output }) => completed_from_output(
                turn_context,
                plan.command,
                plan.cwd,
                ProjectValidationStatus::TimedOut,
                output.as_ref().clone(),
            ),
            CodexErrorDetails::Sandbox(SandboxErr::Denied { output, .. }) => completed_from_output(
                turn_context,
                plan.command,
                plan.cwd,
                ProjectValidationStatus::InfrastructureFailure,
                output.as_ref().clone(),
            ),
            CodexErrorDetails::TurnAborted => return ProjectValidationRun::Cancelled,
            CodexErrorDetails::Io(io_error) if is_configuration_io_error(io_error) => {
                configuration_error(
                    turn_context,
                    plan.command,
                    Some(plan.cwd),
                    format!(
                        "failed to start {}: {error}",
                        plan.kind.start_failure_label()
                    ),
                )
            }
            _ => completed_event(
                turn_context,
                plan.command,
                Some(plan.cwd),
                ProjectValidationStatus::InfrastructureFailure,
                None,
                format!("{} infrastructure failure: {error}", plan.kind.label()),
                Duration::ZERO,
            ),
        },
    })
}

fn validate_project_command(
    turn_context: &TurnContext,
    configured: &ProjectValidationCommand,
) -> Result<Vec<String>, ProjectValidationCompletedEvent> {
    let command = configured.command.clone();
    if command
        .first()
        .is_none_or(|program| program.trim().is_empty())
    {
        return Err(configuration_error(
            turn_context,
            command,
            None,
            "validation.project_command.command must include a non-empty executable",
        ));
    }
    let command_bytes = command.iter().fold(0usize, |total, argument| {
        total.saturating_add(argument.len() + 1)
    });
    if command_bytes > PROJECT_VALIDATION_COMMAND_MAX_BYTES {
        return Err(configuration_error(
            turn_context,
            command,
            None,
            format!(
                "validation.project_command.command must not exceed {PROJECT_VALIDATION_COMMAND_MAX_BYTES} bytes"
            ),
        ));
    }
    if configured.timeout_ms == 0 || configured.timeout_ms > MAX_PROJECT_VALIDATION_TIMEOUT_MS {
        return Err(configuration_error(
            turn_context,
            command,
            None,
            format!(
                "validation.project_command.timeout_ms must be between 1 and {MAX_PROJECT_VALIDATION_TIMEOUT_MS}"
            ),
        ));
    }
    Ok(command)
}

fn automatic_validation_plan(command: AutomaticValidationCommand) -> ValidationCommandPlan {
    ValidationCommandPlan {
        kind: ValidationCommandKind::Shellcheck,
        command: command.command,
        cwd: command.cwd,
        timeout_ms: command.timeout_ms,
    }
}

fn provider_error_event(
    turn_context: &TurnContext,
    error: AutomaticValidationProviderError,
) -> ProjectValidationCompletedEvent {
    match error.kind {
        AutomaticValidationProviderErrorKind::Configuration => {
            configuration_error(turn_context, error.command, error.cwd, error.message)
        }
        AutomaticValidationProviderErrorKind::Infrastructure => completed_event(
            turn_context,
            error.command,
            error.cwd,
            ProjectValidationStatus::InfrastructureFailure,
            None,
            error.message,
            Duration::ZERO,
        ),
    }
}

impl ValidationCommandKind {
    fn label(&self) -> &'static str {
        match self {
            Self::ProjectCommand => "project validation",
            Self::Shellcheck => "shellcheck validation",
        }
    }

    fn start_failure_label(&self) -> &'static str {
        match self {
            Self::ProjectCommand => "project validation command",
            Self::Shellcheck => "shellcheck validation command",
        }
    }
}

fn attempt_worktree_start(
    attempt: &ProjectValidationAttempt,
) -> Option<&ProjectValidationWorktreeFingerprint> {
    match attempt {
        ProjectValidationAttempt::Initial {
            worktree_at_turn_start,
            ..
        }
        | ProjectValidationAttempt::CorrectionRerun {
            worktree_at_turn_start,
        } => worktree_at_turn_start.as_ref(),
    }
}

async fn initial_attempt_worktree_unchanged(
    cwd: &AbsolutePathBuf,
    attempt: &ProjectValidationAttempt,
    cancellation_token: &CancellationToken,
) -> Option<bool> {
    let ProjectValidationAttempt::Initial {
        worktree_at_turn_start: Some(worktree_at_turn_start),
        model_used_tools: false,
    } = attempt
    else {
        return Some(false);
    };
    let worktree = tokio::select! {
        _ = cancellation_token.cancelled() => return None,
        worktree = capture_worktree_fingerprint(cwd) => worktree,
    };
    Some(worktree.is_some_and(|worktree| worktree == *worktree_at_turn_start))
}

async fn capture_worktree_fingerprint(
    cwd: &AbsolutePathBuf,
) -> Option<ProjectValidationWorktreeFingerprint> {
    let repo_root = project_validation_repo_root(cwd)?;
    let head_commit = get_head_commit_hash(&repo_root).await?.0;
    let worktree_diff = get_worktree_diff_fingerprint(&repo_root).await;
    if worktree_diff.as_deref() == Some("unknown") {
        return None;
    }
    Some(ProjectValidationWorktreeFingerprint {
        head_commit,
        worktree_diff,
    })
}

fn project_validation_repo_root(cwd: &AbsolutePathBuf) -> Option<PathBuf> {
    let repo_root = get_git_repo_root(cwd.as_ref())?;
    Some(dunce::canonicalize(&repo_root).unwrap_or(repo_root))
}

async fn project_validation_lease_root(
    turn_context: &TurnContext,
    cwd: &AbsolutePathBuf,
) -> Option<PathBuf> {
    if let Some(filesystem) = turn_context.environments.primary_filesystem()
        && let Some(repo_root) = resolve_root_git_project_for_trust(filesystem.as_ref(), cwd).await
    {
        let repo_root = repo_root.into_path_buf();
        return Some(dunce::canonicalize(&repo_root).unwrap_or(repo_root));
    }

    project_validation_repo_root(cwd)
}

fn is_configuration_io_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    )
}

fn configuration_error(
    turn_context: &TurnContext,
    command: Vec<String>,
    cwd: Option<AbsolutePathBuf>,
    message: impl Into<String>,
) -> ProjectValidationCompletedEvent {
    completed_event(
        turn_context,
        command,
        cwd,
        ProjectValidationStatus::ConfigurationError,
        None,
        message.into(),
        Duration::ZERO,
    )
}

fn completed_from_output(
    turn_context: &TurnContext,
    command: Vec<String>,
    cwd: AbsolutePathBuf,
    status: ProjectValidationStatus,
    output: ExecToolCallOutput,
) -> ProjectValidationCompletedEvent {
    let text = if output.aggregated_output.text.is_empty() {
        match (output.stdout.text.is_empty(), output.stderr.text.is_empty()) {
            (false, false) => format!("{}\n{}", output.stdout.text, output.stderr.text),
            (false, true) => output.stdout.text,
            (true, false) => output.stderr.text,
            (true, true) => String::new(),
        }
    } else {
        output.aggregated_output.text
    };
    completed_event(
        turn_context,
        command,
        Some(cwd),
        status,
        Some(output.exit_code),
        text,
        output.duration,
    )
}

fn completed_event(
    turn_context: &TurnContext,
    command: Vec<String>,
    cwd: Option<AbsolutePathBuf>,
    status: ProjectValidationStatus,
    exit_code: Option<i32>,
    output: String,
    duration: Duration,
) -> ProjectValidationCompletedEvent {
    let (output, output_truncated) = truncate_output(&output);
    ProjectValidationCompletedEvent {
        turn_id: turn_context.sub_id.clone(),
        command,
        cwd,
        status,
        exit_code,
        output,
        output_truncated,
        duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
    }
}

fn truncate_output(output: &str) -> (String, bool) {
    if output.len() <= PROJECT_VALIDATION_OUTPUT_MAX_BYTES {
        return (output.to_string(), false);
    }

    let content_budget =
        PROJECT_VALIDATION_OUTPUT_MAX_BYTES.saturating_sub(OUTPUT_TRUNCATED_MARKER.len());
    let prefix_budget = content_budget / 2;
    let suffix_budget = content_budget.saturating_sub(prefix_budget);
    let prefix_end = floor_char_boundary(output, prefix_budget);
    let suffix_start = ceil_char_boundary(output, output.len().saturating_sub(suffix_budget));
    (
        format!(
            "{}{}{}",
            &output[..prefix_end],
            OUTPUT_TRUNCATED_MARKER,
            &output[suffix_start..]
        ),
        true,
    )
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(value: &str, mut index: usize) -> usize {
    while !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
#[path = "project_validation_tests.rs"]
mod tests;
