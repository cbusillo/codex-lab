use std::ffi::OsString;
use std::io;
use std::path::Path;
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
use codex_protocol::protocol::ProjectValidationSkipReason;
use codex_protocol::protocol::ProjectValidationStatus;
use codex_utils_absolute_path::AbsolutePathBuf;
use tokio_util::sync::CancellationToken;

use super::Session;
use super::cargo_validation_provider::classify_cargo_output;
use super::cargo_validation_provider::render_cargo_output;
use super::project_validation_coordinator::ProjectValidationSuccessKey;
use super::turn_context::TurnContext;
use super::validation_provider::AutomaticValidationCommand;
use super::validation_provider::AutomaticValidationProviderError;
use super::validation_provider::AutomaticValidationProviderErrorKind;
use super::validation_provider::AutomaticValidationProviderKind;
use super::validation_provider::AutomaticValidationProviderResolution;
use super::validation_provider::AutomaticValidationProviderSkipReason;
use super::validation_provider::automatic_validation_provider_enabled;
use super::validation_provider::resolve_automatic_validation_provider;
use crate::exec::ExecCapturePolicy;
use crate::exec::ExecExpiration;
use crate::exec::ExecParams;
use crate::exec::process_exec_tool_call;
use crate::exec_env::create_env;

const PROJECT_VALIDATION_OUTPUT_MAX_BYTES: usize = 8 * 1024;
const PROJECT_VALIDATION_COMMAND_MAX_BYTES: usize = 8 * 1024;
const COMMAND_TRUNCATED_MARKER: &str = "… project validation command truncated …";
const OUTPUT_TRUNCATED_MARKER: &str = "\n… project validation output truncated …\n";

pub(crate) enum ProjectValidationRun {
    Skipped(ProjectValidationCompletedEvent),
    Completed(ProjectValidationCompletedEvent),
    Cancelled(ProjectValidationCompletedEvent),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ValidationCommandKind {
    Cargo,
    ProjectCommand,
    Shellcheck,
}

struct ValidationCommandPlan {
    kind: ValidationCommandKind,
    command: Vec<String>,
    cwd: AbsolutePathBuf,
    execution_cwd: AbsolutePathBuf,
    _execution_cwd_guard: Option<tempfile::TempDir>,
    timeout_ms: u64,
    changed_file_count: Option<u32>,
}

#[derive(Default)]
struct ProjectValidationEventMetadata {
    skip_reason: Option<ProjectValidationSkipReason>,
    changed_file_count: Option<u32>,
    exit_code: Option<i32>,
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
    if turn_context.session_source.is_non_root_agent() {
        return ProjectValidationRun::Skipped(skipped_event(
            turn_context,
            Vec::new(),
            None,
            ProjectValidationSkipReason::NonRootAgent,
            None,
        ));
    }

    let project_command = match configured_project_command {
        Some(configured) => match validate_project_command(turn_context, configured) {
            Ok(command) => Some(command),
            Err(event) => return ProjectValidationRun::Completed(event),
        },
        None => None,
    };

    if project_command.is_none()
        && !automatic_validation_provider_enabled(&turn_context.config.validation)
    {
        let cwd = turn_context.environments.single_local_environment_cwd();
        if cancellation_token.is_cancelled() {
            return ProjectValidationRun::Cancelled(cancelled_event(
                turn_context,
                Vec::new(),
                cwd,
                None,
            ));
        }
        return ProjectValidationRun::Skipped(skipped_event(
            turn_context,
            Vec::new(),
            cwd,
            ProjectValidationSkipReason::ValidationDisabled,
            None,
        ));
    }

    let Some(cwd) = turn_context.environments.single_local_environment_cwd() else {
        if let Some(command) = project_command {
            return ProjectValidationRun::Completed(completed_event(
                turn_context,
                command,
                None,
                ProjectValidationStatus::InfrastructureFailure,
                None,
                "project validation requires exactly one local turn environment".to_string(),
                Duration::ZERO,
            ));
        }
        return ProjectValidationRun::Skipped(skipped_event(
            turn_context,
            Vec::new(),
            None,
            ProjectValidationSkipReason::UnsupportedEnvironment,
            None,
        ));
    };

    if cancellation_token.is_cancelled() {
        return ProjectValidationRun::Cancelled(cancelled_event(
            turn_context,
            project_command.clone().unwrap_or_default(),
            Some(cwd),
            None,
        ));
    }

    let env = create_env(
        &turn_context.config.permissions.shell_environment_policy,
        Some(sess.thread_id),
    );
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
        Some(true) => {
            return ProjectValidationRun::Skipped(skipped_event(
                turn_context,
                project_command.clone().unwrap_or_default(),
                Some(cwd),
                ProjectValidationSkipReason::UnchangedFingerprint,
                None,
            ));
        }
        Some(false) => {}
        None => {
            return ProjectValidationRun::Cancelled(cancelled_event(
                turn_context,
                project_command.clone().unwrap_or_default(),
                Some(cwd),
                None,
            ));
        }
    }

    let lease_root = project_validation_lease_root(turn_context, &cwd).await;
    let _lease = if let Some(repo_root) = lease_root.clone() {
        let Some(lease) = sess
            .services
            .project_validation_coordinator
            .acquire(repo_root, &cancellation_token)
            .await
        else {
            return ProjectValidationRun::Cancelled(cancelled_event(
                turn_context,
                project_command.clone().unwrap_or_default(),
                Some(cwd),
                None,
            ));
        };
        Some(lease)
    } else {
        None
    };

    match initial_attempt_worktree_unchanged(&cwd, &attempt, &cancellation_token).await {
        Some(true) => {
            return ProjectValidationRun::Skipped(skipped_event(
                turn_context,
                project_command.clone().unwrap_or_default(),
                Some(cwd),
                ProjectValidationSkipReason::UnchangedFingerprint,
                None,
            ));
        }
        Some(false) => {}
        None => {
            return ProjectValidationRun::Cancelled(cancelled_event(
                turn_context,
                project_command.clone().unwrap_or_default(),
                Some(cwd),
                None,
            ));
        }
    }

    if cancellation_token.is_cancelled() {
        return ProjectValidationRun::Cancelled(cancelled_event(
            turn_context,
            project_command.clone().unwrap_or_default(),
            Some(cwd),
            None,
        ));
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
            execution_cwd: cwd.clone(),
            _execution_cwd_guard: None,
            timeout_ms: configured.timeout_ms,
            changed_file_count: None,
        }
    } else {
        let mut automatic = match resolve_automatic_validation_provider(
            &turn_context.config.validation,
            &cwd,
            attempt_worktree_start(&attempt).map(|worktree| worktree.head_commit.as_str()),
        )
        .await
        {
            Ok(AutomaticValidationProviderResolution::Command(command)) => command,
            Ok(AutomaticValidationProviderResolution::Skipped(skip)) => {
                return ProjectValidationRun::Skipped(skipped_event(
                    turn_context,
                    Vec::new(),
                    Some(cwd),
                    automatic_provider_skip_reason(skip.reason),
                    skip.changed_file_count,
                ));
            }
            Err(error) => {
                return ProjectValidationRun::Completed(provider_error_event(turn_context, error));
            }
        };
        let resolved_program = match which::which_in(
            &automatic.command[0],
            search_path.as_ref(),
            automatic.cwd.as_ref(),
        ) {
            Ok(program) => program,
            Err(_) => {
                let label = automatic.kind.label();
                return ProjectValidationRun::Completed(configuration_error(
                    turn_context,
                    automatic.command,
                    Some(automatic.cwd),
                    format!("{label} executable was not found or is not executable"),
                ));
            }
        };
        if automatic.kind == AutomaticValidationProviderKind::Cargo {
            if !resolved_program
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| matches!(name, "cargo" | "cargo.exe"))
            {
                return ProjectValidationRun::Completed(configuration_error(
                    turn_context,
                    automatic.command,
                    Some(automatic.cwd),
                    "cargo validation executable must be named cargo",
                ));
            }
            let checkout_root = project_validation_repo_root(&cwd);
            if cargo_lookup_path_is_inside_repository(
                &automatic.command[0],
                search_path.as_ref(),
                &automatic.cwd,
                checkout_root.as_deref(),
                lease_root.as_deref(),
            ) {
                return ProjectValidationRun::Completed(configuration_error(
                    turn_context,
                    automatic.command,
                    Some(automatic.cwd),
                    "cargo validation executable must not resolve inside the repository",
                ));
            }
            let canonical_program =
                dunce::canonicalize(&resolved_program).unwrap_or_else(|_| resolved_program.clone());
            if checkout_root.as_ref().is_some_and(|repo_root| {
                resolved_program.starts_with(repo_root) || canonical_program.starts_with(repo_root)
            }) || lease_root.as_ref().is_some_and(|repo_root| {
                resolved_program.starts_with(repo_root) || canonical_program.starts_with(repo_root)
            }) {
                return ProjectValidationRun::Completed(configuration_error(
                    turn_context,
                    automatic.command,
                    Some(automatic.cwd),
                    "cargo validation executable must not resolve inside the repository",
                ));
            }
            let Some(execution_cwd) = automatic.execution_cwd.as_ref() else {
                return ProjectValidationRun::Completed(completed_event(
                    turn_context,
                    automatic.command,
                    Some(automatic.cwd),
                    ProjectValidationStatus::InfrastructureFailure,
                    None,
                    "cargo validation requires an isolated execution directory".to_string(),
                    Duration::ZERO,
                ));
            };
            if project_validation_repo_root(&cwd)
                .is_some_and(|repo_root| execution_cwd.as_ref().starts_with(repo_root))
                || lease_root
                    .as_ref()
                    .is_some_and(|repo_root| execution_cwd.as_ref().starts_with(repo_root))
            {
                return ProjectValidationRun::Completed(completed_event(
                    turn_context,
                    automatic.command,
                    Some(automatic.cwd),
                    ProjectValidationStatus::InfrastructureFailure,
                    None,
                    "cargo validation execution directory must be outside the repository"
                        .to_string(),
                    Duration::ZERO,
                ));
            }
            let Some(resolved_program) = resolved_program.to_str() else {
                return ProjectValidationRun::Completed(configuration_error(
                    turn_context,
                    automatic.command,
                    Some(automatic.cwd),
                    "cargo validation executable path must be valid UTF-8",
                ));
            };
            automatic.command[0] = resolved_program.to_string();
        }
        automatic_validation_plan(automatic)
    };

    let successful_validation_key =
        cargo_validation_success_key(&cwd, lease_root.as_ref(), &plan, &cancellation_token).await;
    if matches!(attempt, ProjectValidationAttempt::Initial { .. })
        && let Some(key) = successful_validation_key.as_ref()
        && sess
            .services
            .project_validation_coordinator
            .has_successful_validation(key)
            .await
    {
        return ProjectValidationRun::Skipped(skipped_event(
            turn_context,
            plan.command,
            Some(plan.cwd),
            ProjectValidationSkipReason::UnchangedFingerprint,
            plan.changed_file_count,
        ));
    }
    let _cargo_permit = if plan.kind == ValidationCommandKind::Cargo {
        let Some(permit) = sess
            .services
            .project_validation_coordinator
            .acquire_cargo(&cancellation_token)
            .await
        else {
            return ProjectValidationRun::Cancelled(cancelled_event(
                turn_context,
                plan.command,
                Some(plan.cwd),
                plan.changed_file_count,
            ));
        };
        Some(permit)
    } else {
        None
    };

    let params = ExecParams {
        command: plan.command.clone(),
        cwd: plan.execution_cwd.clone(),
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
        return ProjectValidationRun::Cancelled(cancelled_event(
            turn_context,
            plan.command,
            Some(plan.cwd),
            plan.changed_file_count,
        ));
    }

    let event = match result {
        Ok(output) if output.timed_out => completed_from_output(
            turn_context,
            plan.command,
            plan.cwd,
            plan.kind,
            ProjectValidationStatus::TimedOut,
            output,
            plan.changed_file_count,
        ),
        Ok(output) if plan.kind == ValidationCommandKind::Cargo => {
            let cargo_output = classify_cargo_output(&output);
            completed_from_output_text(
                turn_context,
                plan.command,
                plan.cwd,
                cargo_output.status,
                output,
                plan.changed_file_count,
                cargo_output.text,
            )
        }
        Ok(output) => {
            let status = if output.exit_code == 0 {
                ProjectValidationStatus::Passed
            } else {
                ProjectValidationStatus::ActionableFailure
            };
            completed_from_output(
                turn_context,
                plan.command,
                plan.cwd,
                plan.kind,
                status,
                output,
                plan.changed_file_count,
            )
        }
        Err(error) => match error.details() {
            CodexErrorDetails::Sandbox(SandboxErr::Timeout { output }) => completed_from_output(
                turn_context,
                plan.command,
                plan.cwd,
                plan.kind,
                ProjectValidationStatus::TimedOut,
                output.as_ref().clone(),
                plan.changed_file_count,
            ),
            CodexErrorDetails::Sandbox(SandboxErr::Denied { output, .. }) => completed_from_output(
                turn_context,
                plan.command,
                plan.cwd,
                plan.kind,
                ProjectValidationStatus::InfrastructureFailure,
                output.as_ref().clone(),
                plan.changed_file_count,
            ),
            CodexErrorDetails::TurnAborted => {
                return ProjectValidationRun::Cancelled(cancelled_event(
                    turn_context,
                    plan.command,
                    Some(plan.cwd),
                    plan.changed_file_count,
                ));
            }
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
    };
    if event.status == ProjectValidationStatus::Passed
        && let Some(key) = successful_validation_key
    {
        sess.services
            .project_validation_coordinator
            .record_successful_validation(key)
            .await;
    }
    ProjectValidationRun::Completed(event)
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
    let execution_cwd = command
        .execution_cwd
        .clone()
        .unwrap_or_else(|| command.cwd.clone());
    ValidationCommandPlan {
        kind: match command.kind {
            AutomaticValidationProviderKind::Cargo => ValidationCommandKind::Cargo,
            AutomaticValidationProviderKind::Shellcheck => ValidationCommandKind::Shellcheck,
        },
        command: command.command,
        cwd: command.cwd,
        execution_cwd,
        _execution_cwd_guard: command.execution_cwd_guard,
        timeout_ms: command.timeout_ms,
        changed_file_count: Some(command.changed_file_count),
    }
}

fn automatic_provider_skip_reason(
    reason: AutomaticValidationProviderSkipReason,
) -> ProjectValidationSkipReason {
    match reason {
        AutomaticValidationProviderSkipReason::ValidationDisabled => {
            ProjectValidationSkipReason::ValidationDisabled
        }
        AutomaticValidationProviderSkipReason::NoChangedFiles => {
            ProjectValidationSkipReason::NoChangedFiles
        }
        AutomaticValidationProviderSkipReason::NoApplicableProvider => {
            ProjectValidationSkipReason::NoApplicableProvider
        }
        AutomaticValidationProviderSkipReason::UnsupportedEnvironment => {
            ProjectValidationSkipReason::UnsupportedEnvironment
        }
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
    fn label(self) -> &'static str {
        match self {
            Self::Cargo => "cargo validation",
            Self::ProjectCommand => "project validation",
            Self::Shellcheck => "shellcheck validation",
        }
    }

    fn start_failure_label(self) -> &'static str {
        match self {
            Self::Cargo => "cargo validation command",
            Self::ProjectCommand => "project validation command",
            Self::Shellcheck => "shellcheck validation command",
        }
    }
}

impl AutomaticValidationProviderKind {
    fn label(self) -> &'static str {
        match self {
            Self::Cargo => "cargo validation",
            Self::Shellcheck => "shellcheck validation",
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

async fn cargo_validation_success_key(
    cwd: &AbsolutePathBuf,
    lease_root: Option<&PathBuf>,
    plan: &ValidationCommandPlan,
    cancellation_token: &CancellationToken,
) -> Option<ProjectValidationSuccessKey> {
    if plan.kind != ValidationCommandKind::Cargo {
        return None;
    }
    let checkout_root = project_validation_repo_root(cwd)?;
    let fingerprint = tokio::select! {
        _ = cancellation_token.cancelled() => return None,
        fingerprint = capture_worktree_fingerprint(cwd) => fingerprint?,
    };
    let validation_scope = plan
        .cwd
        .as_ref()
        .strip_prefix(&checkout_root)
        .unwrap_or(plan.cwd.as_ref())
        .to_path_buf();
    Some(ProjectValidationSuccessKey::new(
        lease_root.cloned().unwrap_or(checkout_root),
        validation_scope,
        fingerprint.head_commit,
        fingerprint.worktree_diff,
        cargo_validation_cache_command(&plan.command),
    ))
}

fn cargo_validation_cache_command(command: &[String]) -> Vec<String> {
    let mut command = command.to_vec();
    for (flag, replacement) in [
        ("--manifest-path", "Cargo.toml"),
        ("--target-dir", "target"),
    ] {
        if let Some(value_index) = command
            .iter()
            .position(|argument| argument == flag)
            .and_then(|index| index.checked_add(1))
            && let Some(value) = command.get_mut(value_index)
        {
            *value = replacement.to_string();
        }
    }
    command
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

fn cargo_lookup_path_is_inside_repository(
    program: &str,
    search_path: Option<&OsString>,
    cwd: &AbsolutePathBuf,
    checkout_root: Option<&Path>,
    lease_root: Option<&Path>,
) -> bool {
    let program_path = std::path::Path::new(program);
    if program_path.is_absolute() || program_path.components().count() > 1 {
        let candidate = if program_path.is_absolute() {
            program_path.to_path_buf()
        } else {
            cwd.as_ref().join(program_path)
        };
        return path_is_inside_validation_repository(&candidate, checkout_root, lease_root);
    }

    let search_path = search_path.cloned().or_else(|| std::env::var_os("PATH"));
    let Some(search_path) = search_path else {
        return false;
    };
    for directory in std::env::split_paths(&search_path) {
        let directory = if directory.is_absolute() {
            directory
        } else {
            cwd.as_ref().join(directory)
        };
        let Ok(single_directory_path) = std::env::join_paths([&directory]) else {
            continue;
        };
        let Ok(resolved) = which::which_in(program, Some(&single_directory_path), cwd.as_ref())
        else {
            continue;
        };
        return path_is_inside_validation_repository(&directory, checkout_root, lease_root)
            || path_is_inside_validation_repository(&resolved, checkout_root, lease_root);
    }
    false
}

fn path_is_inside_validation_repository(
    path: &Path,
    checkout_root: Option<&Path>,
    lease_root: Option<&Path>,
) -> bool {
    let canonical = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let canonical_parent_path = path
        .parent()
        .and_then(|parent| dunce::canonicalize(parent).ok())
        .and_then(|parent| path.file_name().map(|name| parent.join(name)));
    checkout_root.is_some_and(|repo_root| {
        path.starts_with(repo_root)
            || canonical.starts_with(repo_root)
            || canonical_parent_path
                .as_ref()
                .is_some_and(|path| path.starts_with(repo_root))
    }) || lease_root.is_some_and(|repo_root| {
        path.starts_with(repo_root)
            || canonical.starts_with(repo_root)
            || canonical_parent_path
                .as_ref()
                .is_some_and(|path| path.starts_with(repo_root))
    })
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
    kind: ValidationCommandKind,
    status: ProjectValidationStatus,
    output: ExecToolCallOutput,
    changed_file_count: Option<u32>,
) -> ProjectValidationCompletedEvent {
    let text = if kind == ValidationCommandKind::Cargo {
        render_cargo_output(&output)
    } else if output.aggregated_output.text.is_empty() {
        match (output.stdout.text.is_empty(), output.stderr.text.is_empty()) {
            (false, false) => format!("{}\n{}", &output.stdout.text, &output.stderr.text),
            (false, true) => output.stdout.text.clone(),
            (true, false) => output.stderr.text.clone(),
            (true, true) => String::new(),
        }
    } else {
        output.aggregated_output.text.clone()
    };
    completed_from_output_text(
        turn_context,
        command,
        cwd,
        status,
        output,
        changed_file_count,
        text,
    )
}

fn completed_from_output_text(
    turn_context: &TurnContext,
    command: Vec<String>,
    cwd: AbsolutePathBuf,
    status: ProjectValidationStatus,
    output: ExecToolCallOutput,
    changed_file_count: Option<u32>,
    text: String,
) -> ProjectValidationCompletedEvent {
    terminal_event(
        turn_context,
        command,
        Some(cwd),
        status,
        ProjectValidationEventMetadata {
            changed_file_count,
            exit_code: Some(output.exit_code),
            ..Default::default()
        },
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
    terminal_event(
        turn_context,
        command,
        cwd,
        status,
        ProjectValidationEventMetadata {
            exit_code,
            ..Default::default()
        },
        output,
        duration,
    )
}

fn skipped_event(
    turn_context: &TurnContext,
    command: Vec<String>,
    cwd: Option<AbsolutePathBuf>,
    reason: ProjectValidationSkipReason,
    changed_file_count: Option<u32>,
) -> ProjectValidationCompletedEvent {
    let output = match (reason, changed_file_count) {
        (ProjectValidationSkipReason::ValidationDisabled, _) => {
            "automatic validation skipped: validation is disabled".to_string()
        }
        (ProjectValidationSkipReason::NoChangedFiles, _) => {
            "automatic validation skipped: no changed files".to_string()
        }
        (ProjectValidationSkipReason::NoApplicableProvider, Some(count)) => format!(
            "automatic validation skipped: no applicable provider for {count} changed file(s)"
        ),
        (ProjectValidationSkipReason::NoApplicableProvider, None) => {
            "automatic validation skipped: no applicable provider".to_string()
        }
        (ProjectValidationSkipReason::NonRootAgent, _) => {
            "automatic validation skipped: non-root agents do not run project validation"
                .to_string()
        }
        (ProjectValidationSkipReason::UnchangedFingerprint, _) => {
            "automatic validation skipped: worktree fingerprint is unchanged".to_string()
        }
        (ProjectValidationSkipReason::UnsupportedEnvironment, _) => {
            "automatic validation skipped: unsupported turn environment".to_string()
        }
    };
    terminal_event(
        turn_context,
        command,
        cwd,
        ProjectValidationStatus::Skipped,
        ProjectValidationEventMetadata {
            skip_reason: Some(reason),
            changed_file_count,
            ..Default::default()
        },
        output,
        Duration::ZERO,
    )
}

fn cancelled_event(
    turn_context: &TurnContext,
    command: Vec<String>,
    cwd: Option<AbsolutePathBuf>,
    changed_file_count: Option<u32>,
) -> ProjectValidationCompletedEvent {
    terminal_event(
        turn_context,
        command,
        cwd,
        ProjectValidationStatus::Cancelled,
        ProjectValidationEventMetadata {
            changed_file_count,
            ..Default::default()
        },
        "project validation cancelled".to_string(),
        Duration::ZERO,
    )
}

fn terminal_event(
    turn_context: &TurnContext,
    command: Vec<String>,
    cwd: Option<AbsolutePathBuf>,
    status: ProjectValidationStatus,
    metadata: ProjectValidationEventMetadata,
    output: String,
    duration: Duration,
) -> ProjectValidationCompletedEvent {
    let (command, command_truncated) = truncate_command(command);
    let (output, output_truncated) = truncate_output(&output);
    ProjectValidationCompletedEvent {
        turn_id: turn_context.sub_id.clone(),
        item_id: Some(uuid::Uuid::new_v4().to_string()),
        command,
        command_truncated,
        cwd,
        status,
        skip_reason: metadata.skip_reason,
        changed_file_count: metadata.changed_file_count,
        exit_code: metadata.exit_code,
        output,
        output_truncated,
        duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
    }
}

fn truncate_command(command: Vec<String>) -> (Vec<String>, bool) {
    let command_bytes = command.iter().fold(0usize, |total, argument| {
        total.saturating_add(argument.len() + 1)
    });
    if command_bytes <= PROJECT_VALIDATION_COMMAND_MAX_BYTES {
        return (command, false);
    }

    let mut bounded = Vec::new();
    let mut used_bytes = COMMAND_TRUNCATED_MARKER.len() + 1;
    for argument in command {
        let argument_bytes = argument.len() + 1;
        if used_bytes.saturating_add(argument_bytes) > PROJECT_VALIDATION_COMMAND_MAX_BYTES {
            break;
        }
        used_bytes += argument_bytes;
        bounded.push(argument);
    }
    bounded.push(COMMAND_TRUNCATED_MARKER.to_string());
    (bounded, true)
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
