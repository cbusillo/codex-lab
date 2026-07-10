use std::io;
use std::sync::Arc;
use std::time::Duration;

use codex_config::MAX_PROJECT_VALIDATION_TIMEOUT_MS;
use codex_protocol::error::CodexErr;
use codex_protocol::error::SandboxErr;
use codex_protocol::exec_output::ExecToolCallOutput;
use codex_protocol::models::SandboxPermissions;
use codex_protocol::protocol::ProjectValidationCompletedEvent;
use codex_protocol::protocol::ProjectValidationStatus;
use codex_utils_absolute_path::AbsolutePathBuf;
use tokio_util::sync::CancellationToken;

use super::Session;
use super::turn_context::TurnContext;
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

pub(crate) async fn run_project_validation(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    cancellation_token: CancellationToken,
) -> ProjectValidationRun {
    let Some(configured) = turn_context.config.validation.project_command.as_ref() else {
        return ProjectValidationRun::Skipped;
    };
    if turn_context.session_source.is_non_root_agent() {
        return ProjectValidationRun::Skipped;
    }

    let command = configured.command.clone();
    if command
        .first()
        .is_none_or(|program| program.trim().is_empty())
    {
        return ProjectValidationRun::Completed(configuration_error(
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
        return ProjectValidationRun::Completed(configuration_error(
            turn_context,
            command,
            None,
            format!(
                "validation.project_command.command must not exceed {PROJECT_VALIDATION_COMMAND_MAX_BYTES} bytes"
            ),
        ));
    }
    if configured.timeout_ms == 0 || configured.timeout_ms > MAX_PROJECT_VALIDATION_TIMEOUT_MS {
        return ProjectValidationRun::Completed(configuration_error(
            turn_context,
            command,
            None,
            format!(
                "validation.project_command.timeout_ms must be between 1 and {MAX_PROJECT_VALIDATION_TIMEOUT_MS}"
            ),
        ));
    }

    let Some(cwd) = turn_context
        .environments
        .single_local_environment_cwd()
        .cloned()
    else {
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

    let params = ExecParams {
        command: command.clone(),
        cwd: cwd.clone(),
        expiration: ExecExpiration::TimeoutOrCancellation {
            timeout: Duration::from_millis(configured.timeout_ms),
            cancellation: cancellation_token.clone(),
        },
        capture_policy: ExecCapturePolicy::ShellTool,
        env: create_env(&turn_context.shell_environment_policy, Some(sess.thread_id)),
        network: turn_context.network.clone(),
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
        &cwd,
        &turn_context.config.effective_workspace_roots(),
        &turn_context.codex_linux_sandbox_exe,
        turn_context.features.use_legacy_landlock(),
        None,
    )
    .await;

    if cancellation_token.is_cancelled() {
        return ProjectValidationRun::Cancelled;
    }

    ProjectValidationRun::Completed(match result {
        Ok(output) if output.timed_out => completed_from_output(
            turn_context,
            command,
            cwd,
            ProjectValidationStatus::TimedOut,
            output,
        ),
        Ok(output) => {
            let status = if output.exit_code == 0 {
                ProjectValidationStatus::Passed
            } else {
                ProjectValidationStatus::ActionableFailure
            };
            completed_from_output(turn_context, command, cwd, status, output)
        }
        Err(CodexErr::Sandbox(SandboxErr::Timeout { output })) => completed_from_output(
            turn_context,
            command,
            cwd,
            ProjectValidationStatus::TimedOut,
            *output,
        ),
        Err(CodexErr::Sandbox(SandboxErr::Denied { output, .. })) => completed_from_output(
            turn_context,
            command,
            cwd,
            ProjectValidationStatus::InfrastructureFailure,
            *output,
        ),
        Err(CodexErr::TurnAborted) => return ProjectValidationRun::Cancelled,
        Err(CodexErr::Io(error)) if is_configuration_io_error(&error) => configuration_error(
            turn_context,
            command,
            Some(cwd),
            format!("failed to start project validation command: {error}"),
        ),
        Err(error) => completed_event(
            turn_context,
            command,
            Some(cwd),
            ProjectValidationStatus::InfrastructureFailure,
            None,
            format!("project validation infrastructure failure: {error}"),
            Duration::ZERO,
        ),
    })
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
