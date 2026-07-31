use crate::agent::AgentControl;
use crate::agent::AgentStatus;
use crate::agent::external_diagnostics::ExternalAgentFailureDetail;
use crate::agent::external_diagnostics::ExternalAgentFailureKind;
use crate::agent::external_diagnostics::ExternalAgentProviderProvenance;
use crate::agent::external_diagnostics::classify_provider_failure_text;
use crate::agent::external_diagnostics::redact_external_agent_status;
use crate::agent::external_preflight::antigravity_launch_dir;
#[cfg(test)]
use crate::agent::external_preflight::github_copilot_version_output;
use crate::agent::external_preflight::preflight_external_agent_backend;
#[cfg(all(test, unix))]
use crate::agent::external_preflight::run_external_agent_preflight_command_with_timeout;
use crate::config::ExternalCommandAgentBackendConfig;
use crate::config::ExternalCommandProtocol;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::Child;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

const MAX_EXTERNAL_AGENT_STDOUT_BYTES: usize = 64 * 1024;
const MAX_EXTERNAL_AGENT_STDERR_BYTES: usize = 8 * 1024;
const EXTERNAL_AGENT_TRUNCATED_MARKER: &[u8] = b"[external agent output truncated]\n";
const MAX_MODEL_VISIBLE_EXTERNAL_AGENT_BYTES: usize = 8 * 1024;
const EXTERNAL_AGENT_MESSAGE_TRUNCATED_MARKER: &str = "[external agent result truncated]\n";
pub(super) const MAX_PREFLIGHT_MESSAGE_BYTES: usize = 2 * 1024;
const CARGO_TARGET_DIR_ENV_VAR: &str = "CARGO_TARGET_DIR";
const CODEX_LAB_CARGO_TARGET_DIR_ENV_VAR: &str = "CODEX_LAB_CARGO_TARGET_DIR";
const CODEX_LAB_CARGO_TARGET_SCOPE_ENV_VAR: &str = "CODEX_LAB_CARGO_TARGET_SCOPE";
const CODEX_LAB_CARGO_TARGET_KEY_ENV_VAR: &str = "CODEX_LAB_CARGO_TARGET_KEY";
const EXTERNAL_AGENT_CARGO_TARGET_SCOPE_VALUE: &str = "agent";

#[derive(Debug, Clone)]
pub(crate) struct ExternalAgentLaunch {
    pub(crate) thread_id: ThreadId,
    pub(crate) parent_thread_id: ThreadId,
    pub(crate) author: AgentPath,
    pub(crate) recipient: AgentPath,
    pub(crate) role: Option<String>,
    pub(crate) task_name: Option<String>,
    pub(crate) initial_operation: Op,
    pub(crate) backend: ExternalCommandAgentBackendConfig,
    pub(crate) cwd: std::path::PathBuf,
    pub(crate) cancellation_token: CancellationToken,
    pub(crate) is_read_only: bool,
    pub(crate) preflight_completed: bool,
    pub(crate) resolved_command: Option<PathBuf>,
    pub(crate) hide_provider_metadata: bool,
}

#[derive(Debug, Serialize)]
struct ExternalAgentRequest {
    protocol_version: u32,
    thread_id: ThreadId,
    parent_thread_id: ThreadId,
    author: String,
    recipient: String,
    role: Option<String>,
    task_name: Option<String>,
    cwd: String,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalAgentResponse {
    status: ExternalAgentResponseStatus,
    final_message: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ExternalAgentResponseStatus {
    Completed,
    Failed,
}

struct ExternalAgentProcessOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Debug)]
struct ExternalAgentRunError {
    detail: ExternalAgentFailureDetail,
    source: anyhow::Error,
}

impl ExternalAgentRunError {
    fn new(kind: ExternalAgentFailureKind, source: impl Into<anyhow::Error>) -> Self {
        let source = source.into();
        Self {
            detail: ExternalAgentFailureDetail::new(kind, source.to_string()),
            source,
        }
    }

    fn from_detail(detail: ExternalAgentFailureDetail) -> Self {
        let message = detail
            .message
            .clone()
            .unwrap_or_else(|| format!("external agent failed: {}", detail.kind.as_str()));
        Self {
            detail,
            source: anyhow::anyhow!(message),
        }
    }
}

impl fmt::Display for ExternalAgentRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for ExternalAgentRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.source()
    }
}

pub(super) fn bounded_preflight_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut output = Vec::with_capacity(stdout.len().saturating_add(stderr.len() + 1));
    output.extend_from_slice(stdout);
    if !stdout.is_empty() && !stderr.is_empty() {
        output.push(b'\n');
    }
    output.extend_from_slice(stderr);
    let output = String::from_utf8_lossy(&output);
    let mut start = output.len().saturating_sub(MAX_PREFLIGHT_MESSAGE_BYTES);
    while !output.is_char_boundary(start) {
        start += 1;
    }
    output[start..].trim().to_string()
}

pub(crate) async fn run_external_agent(launch: ExternalAgentLaunch, control: AgentControl) {
    let thread_id = launch.thread_id;
    control.update_external_agent_status(thread_id, AgentStatus::Running);
    let result = run_external_agent_inner(&launch).await;
    if launch.cancellation_token.is_cancelled() {
        control.update_external_agent_status(thread_id, AgentStatus::Shutdown);
        send_completion_to_parent(&launch, &control, "external agent cancelled".to_string()).await;
        control.release_external_agent(thread_id);
        return;
    }
    match result {
        Ok(response) if response.status == ExternalAgentResponseStatus::Completed => {
            let final_message = response.final_message.unwrap_or_default();
            control.update_external_agent_status(
                thread_id,
                AgentStatus::Completed(Some(final_message.clone())),
            );
            send_completion_to_parent(&launch, &control, final_message.clone()).await;
        }
        Ok(response) => {
            let message = response
                .final_message
                .filter(|message| !message.is_empty())
                .unwrap_or_else(|| "external agent failed".to_string());
            let failure = ExternalAgentFailureDetail::new(
                classify_provider_failure_text(&message),
                message.clone(),
            );
            let parent_message =
                external_agent_parent_failure_message(&launch, &failure, message.as_str());
            control.update_external_agent_failure(
                thread_id,
                AgentStatus::Errored(message.clone()),
                failure,
            );
            send_completion_to_parent(&launch, &control, parent_message).await;
        }
        Err(err) => {
            if launch.cancellation_token.is_cancelled() {
                control.update_external_agent_status(thread_id, AgentStatus::Shutdown);
                send_completion_to_parent(
                    &launch,
                    &control,
                    "external agent cancelled".to_string(),
                )
                .await;
                control.release_external_agent(thread_id);
                return;
            }
            let message = bound_external_agent_message(&err.to_string());
            let parent_message =
                external_agent_parent_failure_message(&launch, &err.detail, message.as_str());
            control.update_external_agent_failure(
                thread_id,
                AgentStatus::Errored(message.clone()),
                err.detail,
            );
            send_completion_to_parent(&launch, &control, parent_message).await;
        }
    }
}

fn external_agent_parent_failure_message(
    launch: &ExternalAgentLaunch,
    failure: &ExternalAgentFailureDetail,
    message: &str,
) -> String {
    if !launch.hide_provider_metadata {
        return message.to_string();
    }
    match redact_external_agent_status(AgentStatus::Errored(message.to_string()), Some(failure)) {
        AgentStatus::Errored(message) => message,
        _ => unreachable!("errored external agent status should remain errored"),
    }
}

async fn run_external_agent_inner(
    launch: &ExternalAgentLaunch,
) -> Result<ExternalAgentResponse, ExternalAgentRunError> {
    if launch.cancellation_token.is_cancelled() {
        return Err(ExternalAgentRunError::new(
            ExternalAgentFailureKind::LaunchFailed,
            anyhow::anyhow!("external agent cancelled before launch"),
        ));
    }

    let message = render_external_agent_message(&launch.initial_operation);
    let request_json = match launch.backend.protocol {
        ExternalCommandProtocol::Json => Some(
            serde_json::to_vec(&ExternalAgentRequest {
                protocol_version: 1,
                thread_id: launch.thread_id,
                parent_thread_id: launch.parent_thread_id,
                author: launch.author.to_string(),
                recipient: launch.recipient.to_string(),
                role: launch.role.clone(),
                task_name: launch.task_name.clone(),
                cwd: launch.cwd.display().to_string(),
                message: message.clone(),
            })
            .map_err(|error| {
                ExternalAgentRunError::new(ExternalAgentFailureKind::LaunchFailed, error)
            })?,
        ),
        ExternalCommandProtocol::RawCli => None,
    };

    let launch_cwd = external_agent_launch_cwd(launch);
    if launch.backend.launch_family.as_deref() == Some("antigravity") {
        if !launch.cwd.is_dir() {
            return Err(ExternalAgentRunError::new(
                ExternalAgentFailureKind::LaunchFailed,
                anyhow::anyhow!(
                    "antigravity workspace directory does not exist: {}",
                    launch.cwd.display()
                ),
            ));
        }
        tokio::fs::create_dir_all(&launch_cwd)
            .await
            .map_err(|error| {
                ExternalAgentRunError::new(ExternalAgentFailureKind::LaunchFailed, error)
            })?;
    }

    let preflight_provider = if launch.preflight_completed {
        None
    } else {
        Some(
            preflight_external_agent_backend(
                launch.role.as_deref(),
                &launch.backend,
                &launch.cwd,
                launch.is_read_only,
            )
            .await
            .map_err(ExternalAgentRunError::from_detail)?,
        )
    };
    let mut invocation = build_external_agent_invocation(launch, &message).map_err(|error| {
        ExternalAgentRunError::new(ExternalAgentFailureKind::LaunchFailed, error)
    })?;
    if let Some(resolved_command) = launch.resolved_command.as_deref().or_else(|| {
        preflight_provider
            .as_ref()
            .and_then(ExternalAgentProviderProvenance::resolved_command)
    }) {
        invocation.command = resolved_command.to_path_buf();
    }
    let mut command = Command::new(&invocation.command);
    command
        .args(&invocation.args)
        .current_dir(&launch_cwd)
        .stdin(if request_json.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    command.envs(external_agent_process_env(launch));
    command.env_remove(CARGO_TARGET_DIR_ENV_VAR);
    command.env_remove(CODEX_LAB_CARGO_TARGET_DIR_ENV_VAR);

    if launch.cancellation_token.is_cancelled() {
        return Err(ExternalAgentRunError::new(
            ExternalAgentFailureKind::LaunchFailed,
            anyhow::anyhow!("external agent cancelled before launch"),
        ));
    }

    let child = command.spawn().map_err(|error| {
        ExternalAgentRunError::new(
            if error.kind() == std::io::ErrorKind::NotFound {
                ExternalAgentFailureKind::CommandMissing
            } else {
                ExternalAgentFailureKind::LaunchFailed
            },
            error,
        )
    })?;
    let mut child = ExternalAgentChildGuard::new(child);
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take().ok_or_else(|| {
        ExternalAgentRunError::new(
            ExternalAgentFailureKind::LaunchFailed,
            anyhow::anyhow!("failed to open external agent stdout"),
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ExternalAgentRunError::new(
            ExternalAgentFailureKind::LaunchFailed,
            anyhow::anyhow!("failed to open external agent stderr"),
        )
    })?;

    let interaction = async move {
        if let Some(request_json) = request_json {
            let mut stdin = stdin
                .take()
                .ok_or_else(|| anyhow::anyhow!("failed to open external agent stdin"))?;
            stdin.write_all(&request_json).await?;
            stdin.write_all(b"\n").await?;
            stdin.shutdown().await?;
            drop(stdin);
        }

        let (stdout, stderr, status) = tokio::try_join!(
            read_limited_output(stdout, MAX_EXTERNAL_AGENT_STDOUT_BYTES, "stdout"),
            read_limited_output(stderr, MAX_EXTERNAL_AGENT_STDERR_BYTES, "stderr"),
            async { child.wait().await.map_err(anyhow::Error::from) },
        )?;
        child.disarm();

        Ok::<ExternalAgentProcessOutput, anyhow::Error>(ExternalAgentProcessOutput {
            status,
            stdout,
            stderr,
        })
    };

    let output = tokio::select! {
        _ = launch.cancellation_token.cancelled() => {
            return Err(ExternalAgentRunError::new(
                ExternalAgentFailureKind::LaunchFailed,
                anyhow::anyhow!("external agent cancelled"),
            ));
        }
        output = tokio::time::timeout(Duration::from_millis(launch.backend.timeout_ms), interaction) => {
            let output = output.map_err(|_| {
                ExternalAgentRunError::new(
                    ExternalAgentFailureKind::TimedOut,
                    anyhow::anyhow!("external agent timed out"),
                )
            })?;
            output.map_err(|error| {
                ExternalAgentRunError::new(ExternalAgentFailureKind::LaunchFailed, error)
            })?
        },
    };

    if !output.status.success() {
        let diagnostic = bounded_preflight_output(&output.stdout, &output.stderr);
        let reason = if diagnostic.is_empty() {
            format!("external agent exited with {}", output.status)
        } else {
            format!("external agent exited with {}: {diagnostic}", output.status)
        };
        let kind = classify_provider_failure_text(&reason);
        return Err(ExternalAgentRunError::new(kind, anyhow::anyhow!(reason)));
    }
    match launch.backend.protocol {
        ExternalCommandProtocol::Json => {
            let mut response = serde_json::from_slice::<ExternalAgentResponse>(&output.stdout)
                .map_err(|error| {
                    ExternalAgentRunError::new(
                        ExternalAgentFailureKind::MalformedOutput,
                        anyhow::anyhow!("external agent returned malformed JSON: {error}"),
                    )
                })?;
            if response.status == ExternalAgentResponseStatus::Completed {
                let final_message = response
                    .final_message
                    .as_deref()
                    .map(str::trim)
                    .filter(|message| !message.is_empty())
                    .ok_or_else(|| {
                        ExternalAgentRunError::new(
                            ExternalAgentFailureKind::EmptyOutput,
                            anyhow::anyhow!("external agent completed without output"),
                        )
                    })?;
                response.final_message = Some(bound_external_agent_message(final_message));
            } else {
                // Failed responses flow into `AgentStatus::Errored` and the parent completion
                // context, so they need the same model-visible bound as completed ones.
                response.final_message = response
                    .final_message
                    .as_deref()
                    .map(bound_external_agent_message);
            }
            Ok(response)
        }
        ExternalCommandProtocol::RawCli => {
            let final_message = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if final_message.is_empty() {
                return Err(ExternalAgentRunError::new(
                    ExternalAgentFailureKind::EmptyOutput,
                    anyhow::anyhow!("external agent completed without output"),
                ));
            }
            Ok(ExternalAgentResponse {
                status: ExternalAgentResponseStatus::Completed,
                final_message: Some(bound_external_agent_message(&final_message)),
            })
        }
    }
}

fn external_agent_process_env(launch: &ExternalAgentLaunch) -> HashMap<String, String> {
    let mut env = launch.backend.env.clone();
    env.remove(CARGO_TARGET_DIR_ENV_VAR);
    env.remove(CODEX_LAB_CARGO_TARGET_DIR_ENV_VAR);
    // Always enforce per-agent target isolation for external agents. Backend config cannot
    // override these two routing keys because cargo-build-env.sh gives target-dir overrides
    // precedence over scope/key selection.
    env.insert(
        CODEX_LAB_CARGO_TARGET_SCOPE_ENV_VAR.to_string(),
        EXTERNAL_AGENT_CARGO_TARGET_SCOPE_VALUE.to_string(),
    );
    env.insert(
        CODEX_LAB_CARGO_TARGET_KEY_ENV_VAR.to_string(),
        launch.thread_id.to_string(),
    );
    env
}

pub(super) struct ExternalAgentChildGuard {
    child: Child,
    process_group_id: Option<u32>,
    kill_on_drop: bool,
}

impl ExternalAgentChildGuard {
    pub(super) fn new(child: Child) -> Self {
        let process_group_id = child.id();
        Self {
            child,
            process_group_id,
            kill_on_drop: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.kill_on_drop = false;
    }
}

impl std::ops::Deref for ExternalAgentChildGuard {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.child
    }
}

impl std::ops::DerefMut for ExternalAgentChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}

impl Drop for ExternalAgentChildGuard {
    fn drop(&mut self) {
        if !self.kill_on_drop {
            return;
        }
        if let Some(process_group_id) = self.process_group_id
            && let Err(err) = codex_utils_pty::process_group::kill_process_group(process_group_id)
        {
            tracing::warn!("failed to kill external agent process group {process_group_id}: {err}");
        }
        if let Err(err) = self.child.start_kill()
            && !child_is_already_gone(&err)
        {
            tracing::warn!("failed to kill external agent process: {err}");
        }
    }
}

fn child_is_already_gone(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::NotFound
        || err.raw_os_error() == Some(child_already_gone_raw_os_error())
}

#[cfg(unix)]
fn child_already_gone_raw_os_error() -> i32 {
    libc::ESRCH
}

#[cfg(not(unix))]
fn child_already_gone_raw_os_error() -> i32 {
    0
}

#[derive(Debug)]
struct ExternalAgentInvocation {
    command: PathBuf,
    args: Vec<String>,
}

fn build_external_agent_invocation(
    launch: &ExternalAgentLaunch,
    message: &str,
) -> anyhow::Result<ExternalAgentInvocation> {
    let (command, mut args) = match launch.backend.protocol {
        ExternalCommandProtocol::Json => (launch.backend.command.trim().to_string(), Vec::new()),
        ExternalCommandProtocol::RawCli => split_command_and_args(&launch.backend.command)?,
    };
    if command.is_empty() {
        return Err(anyhow::anyhow!(
            "external_command backend command must not be empty"
        ));
    }
    args.extend(launch.backend.args.iter().cloned());
    args.extend(
        mode_args(&launch.backend, launch.is_read_only)
            .iter()
            .cloned(),
    );
    if launch.backend.protocol == ExternalCommandProtocol::RawCli {
        let launch_family = launch.backend.launch_family.as_deref();
        if launch_family == Some("antigravity") {
            args.push("--add-dir".to_string());
            args.push(launch.cwd.display().to_string());
        }
        if raw_cli_uses_prompt_flag(launch_family) {
            args.push("-p".to_string());
        }
        args.push(message.to_string());
    }
    Ok(ExternalAgentInvocation {
        command: PathBuf::from(command),
        args,
    })
}

fn raw_cli_uses_prompt_flag(launch_family: Option<&str>) -> bool {
    matches!(
        launch_family,
        Some("antigravity" | "claude" | "copilot" | "gemini" | "qwen")
    )
}

fn external_agent_launch_cwd(launch: &ExternalAgentLaunch) -> PathBuf {
    if launch.backend.launch_family.as_deref() == Some("antigravity") {
        return antigravity_launch_dir();
    }
    launch.cwd.clone()
}

fn mode_args(backend: &ExternalCommandAgentBackendConfig, is_read_only: bool) -> &[String] {
    if is_read_only {
        &backend.args_read_only
    } else {
        &backend.args_write
    }
}

/// Detects `C:\dir\tool.exe`, `C:/dir/tool.exe`, and
/// `\\server\share\tool.exe` style paths.
///
/// Shape-based rather than `cfg(windows)`-gated so the behaviour is identical
/// (and testable) on every host: no POSIX command line legitimately starts with
/// a drive letter or a UNC prefix.
fn is_windows_absolute_path(command: &str) -> bool {
    if command.starts_with(r"\\") {
        return true;
    }
    let mut chars = command.chars();
    let Some(drive) = chars.next() else {
        return false;
    };
    drive.is_ascii_alphabetic()
        && chars.next() == Some(':')
        && matches!(chars.next(), Some('\\') | Some('/'))
}

fn quoted_windows_command(command: &str) -> Option<(&str, &str)> {
    let quote = command.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }

    let path_start = quote.len_utf8();
    let path_end = command[path_start..].find(quote)? + path_start;
    let path = &command[path_start..path_end];
    if !is_windows_absolute_path(path) {
        return None;
    }

    Some((path, command[path_end + quote.len_utf8()..].trim()))
}

fn windows_executable_end(command: &str) -> Option<usize> {
    const EXECUTABLE_SUFFIXES: [&str; 4] = [".exe", ".com", ".cmd", ".bat"];

    let lowercase = command.to_ascii_lowercase();
    let mut executable_end = None;
    for suffix in EXECUTABLE_SUFFIXES {
        for (index, _) in lowercase.match_indices(suffix) {
            let end = index + suffix.len();
            let is_boundary = end == command.len()
                || command[end..]
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace);
            if is_boundary {
                executable_end =
                    Some(executable_end.map_or(end, |current: usize| current.min(end)));
            }
        }
    }
    executable_end
}

fn split_windows_inline_args(args: &str) -> anyhow::Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut active_quote = None;
    let mut word_started = false;

    for character in args.chars() {
        match active_quote {
            Some(quote) if character == quote => {
                active_quote = None;
                word_started = true;
            }
            Some(_) => {
                current.push(character);
                word_started = true;
            }
            None if matches!(character, '\'' | '"') => {
                active_quote = Some(character);
                word_started = true;
            }
            None if character.is_whitespace() => {
                if word_started {
                    words.push(std::mem::take(&mut current));
                    word_started = false;
                }
            }
            None => {
                current.push(character);
                word_started = true;
            }
        }
    }

    if active_quote.is_some() {
        anyhow::bail!("external_command backend command has invalid shell quoting");
    }
    if word_started {
        words.push(current);
    }
    Ok(words)
}

fn split_shell_words(command: &str) -> anyhow::Result<Vec<String>> {
    shlex::split(command).ok_or_else(|| {
        anyhow::anyhow!("external_command backend command has invalid shell quoting")
    })
}

pub(super) fn split_command_and_args(command: &str) -> anyhow::Result<(String, Vec<String>)> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Ok((String::new(), Vec::new()));
    }

    if let Some((path, inline_args)) = quoted_windows_command(trimmed) {
        return Ok((path.to_string(), split_windows_inline_args(inline_args)?));
    }

    // POSIX shell quoting treats `\` as an escape, so running an absolute
    // Windows path through `shlex` silently rewrites `C:\dir\tool.exe` into
    // `C:dirtool.exe`, while splitting also corrupts unquoted spaces in paths
    // like `C:/Program Files/...`. Split common executable suffixes without
    // rewriting the path; commands without a recognized suffix stay verbatim
    // and can declare arguments in the backend's `args` field.
    if is_windows_absolute_path(trimmed) {
        if let Some(executable_end) = windows_executable_end(trimmed) {
            let executable = trimmed[..executable_end].to_string();
            let inline_args = trimmed[executable_end..].trim();
            return Ok((executable, split_windows_inline_args(inline_args)?));
        }
        return Ok((trimmed.to_string(), Vec::new()));
    }
    let tokens = split_shell_words(trimmed)?;
    match tokens.split_first() {
        Some((first, rest)) => Ok((first.clone(), rest.to_vec())),
        None => Ok((String::new(), Vec::new())),
    }
}

pub(super) async fn read_limited_output<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
    stream_name: &str,
) -> anyhow::Result<Vec<u8>> {
    let mut tail = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;

    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }

        tail.extend_from_slice(&buffer[..bytes_read]);
        if tail.len() > limit {
            truncated = true;
            let overflow = tail.len() - limit;
            tail.drain(..overflow);
        }
    }

    if truncated {
        tracing::warn!("external agent {stream_name} exceeded limit; truncating output");
        let mut output = Vec::with_capacity(EXTERNAL_AGENT_TRUNCATED_MARKER.len() + tail.len());
        output.extend_from_slice(EXTERNAL_AGENT_TRUNCATED_MARKER);
        output.extend_from_slice(&tail);
        return Ok(output);
    }

    Ok(tail)
}

async fn send_completion_to_parent(
    launch: &ExternalAgentLaunch,
    control: &AgentControl,
    message: String,
) {
    if !control.is_external_agent(launch.thread_id) {
        return;
    }
    // External agents produce their final message outside Codex, so it bypasses
    // `format_inter_agent_completion_message`. Apply the same completion-message budget here
    // before the text lands in the parent thread's model-visible history.
    let message = crate::session_prefix::bounded_completion_payload(&message);
    let communication = InterAgentCommunication::new(
        launch.recipient.clone(),
        launch.author.clone(),
        Vec::new(),
        message,
        /*trigger_turn*/ false,
    );
    let context = crate::agent_communication::AgentCommunicationContext::new(
        crate::agent_communication::AgentCommunicationKind::Result,
        launch.thread_id,
    );
    let _ = control
        .send_inter_agent_communication(
            launch.parent_thread_id,
            communication,
            context,
            /*parent_turn_id*/ None,
        )
        .await;
}

fn render_external_agent_message(initial_operation: &Op) -> String {
    match initial_operation {
        Op::UserInput { items, .. } => items
            .iter()
            .filter_map(|item| match item {
                UserInput::Text { text, .. } => Some(text.clone()),
                UserInput::Image { .. } => Some("[image]".to_string()),
                UserInput::LocalImage { path, .. } => {
                    Some(format!("[local_image:{}]", path.display()))
                }
                UserInput::Skill { name, path, .. } => {
                    Some(format!("[skill:${name}]({})", path.display()))
                }
                UserInput::Mention { name, path, .. } => Some(format!("[mention:${name}]({path})")),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Op::InterAgentCommunication { communication } => communication
            .encrypted_content
            .clone()
            .filter(|content| !content.is_empty())
            .unwrap_or_else(|| communication.content.clone()),
        _ => String::new(),
    }
}

fn bound_external_agent_message(message: &str) -> String {
    if message.len() <= MAX_MODEL_VISIBLE_EXTERNAL_AGENT_BYTES {
        return message.to_string();
    }
    let marker = if message.starts_with("[external agent output truncated]\n") {
        "[external agent output truncated]\n"
    } else {
        EXTERNAL_AGENT_MESSAGE_TRUNCATED_MARKER
    };
    let payload_limit = MAX_MODEL_VISIBLE_EXTERNAL_AGENT_BYTES.saturating_sub(marker.len());
    let mut boundary = message.len().saturating_sub(payload_limit);
    while boundary < message.len() && !message.is_char_boundary(boundary) {
        boundary += 1;
    }
    format!("{marker}{}", &message[boundary..])
}

#[cfg(test)]
#[path = "external_command_tests.rs"]
mod tests;
