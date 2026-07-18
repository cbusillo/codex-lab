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
#[cfg(test)]
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
            let message = err.to_string();
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
                response.final_message = Some(final_message.to_string());
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
                final_message: Some(final_message),
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

pub(super) fn split_command_and_args(command: &str) -> anyhow::Result<(String, Vec<String>)> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    let tokens = shlex::split(trimmed).ok_or_else(|| {
        anyhow::anyhow!("external_command backend command has invalid shell quoting")
    })?;
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
    let communication = InterAgentCommunication::new(
        launch.recipient.clone(),
        launch.author.clone(),
        Vec::new(),
        message,
        /*trigger_turn*/ false,
    );
    let _ = control
        .send_inter_agent_communication(launch.parent_thread_id, communication)
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

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::AgentPath;
    use codex_protocol::protocol::Op;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;

    fn test_launch(
        temp_dir: &TempDir,
        backend: ExternalCommandAgentBackendConfig,
        is_read_only: bool,
    ) -> ExternalAgentLaunch {
        ExternalAgentLaunch {
            thread_id: ThreadId::new(),
            parent_thread_id: ThreadId::new(),
            author: AgentPath::root(),
            recipient: AgentPath::try_from("/root/external").expect("agent path"),
            role: Some("external".to_string()),
            task_name: Some("external".to_string()),
            initial_operation: Op::UserInput {
                environments: None,
                items: vec![UserInput::Text {
                    text: "inspect this repo".to_string(),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
                responsesapi_client_metadata: None,
                additional_context: Default::default(),
                thread_settings: Default::default(),
            },
            backend,
            cwd: temp_dir.path().to_path_buf(),
            cancellation_token: CancellationToken::new(),
            is_read_only,
            preflight_completed: false,
            resolved_command: None,
            hide_provider_metadata: false,
        }
    }

    #[tokio::test]
    async fn pre_cancelled_external_agent_does_not_launch_subprocess() {
        let temp_dir = TempDir::new().expect("tempdir");
        let marker_path = temp_dir.path().join("launched");
        let cancellation_token = CancellationToken::new();
        cancellation_token.cancel();

        let launch = ExternalAgentLaunch {
            thread_id: ThreadId::new(),
            parent_thread_id: ThreadId::new(),
            author: AgentPath::root(),
            recipient: AgentPath::try_from("/root/external").expect("agent path"),
            role: Some("external".to_string()),
            task_name: Some("external".to_string()),
            initial_operation: Op::UserInput {
                environments: None,
                items: Vec::new(),
                final_output_json_schema: None,
                responsesapi_client_metadata: None,
                additional_context: Default::default(),
                thread_settings: Default::default(),
            },
            backend: ExternalCommandAgentBackendConfig {
                command: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    format!("touch '{}'", marker_path.display()),
                ],
                timeout_ms: 5_000,
                ..Default::default()
            },
            cwd: temp_dir.path().to_path_buf(),
            cancellation_token,
            is_read_only: false,
            preflight_completed: false,
            resolved_command: None,
            hide_provider_metadata: false,
        };

        let err = run_external_agent_inner(&launch)
            .await
            .expect_err("pre-cancelled external agent should fail before launch");
        assert!(err.to_string().contains("cancelled before launch"));
        assert!(!marker_path.exists(), "subprocess should not launch");
    }

    #[test]
    fn raw_cli_invocation_appends_mode_args_and_prompt() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "/bin/echo --base".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                args: vec!["--shared".to_string()],
                args_read_only: vec!["--readonly".to_string()],
                args_write: vec!["--write".to_string()],
                timeout_ms: 5_000,
                ..Default::default()
            },
            true,
        );

        let invocation = build_external_agent_invocation(&launch, "inspect this repo")
            .expect("raw cli invocation should build");

        assert_eq!(invocation.command, PathBuf::from("/bin/echo"));
        assert_eq!(
            invocation.args,
            vec![
                "--base".to_string(),
                "--shared".to_string(),
                "--readonly".to_string(),
                "inspect this repo".to_string(),
            ]
        );
    }

    #[test]
    fn antigravity_invocation_adds_repo_dir_and_prompt_flag() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "agy".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                args_write: vec!["--dangerously-skip-permissions".to_string()],
                launch_family: Some("antigravity".to_string()),
                timeout_ms: 5_000,
                ..Default::default()
            },
            false,
        );

        let invocation = build_external_agent_invocation(&launch, "inspect this repo")
            .expect("antigravity invocation should build");

        assert_eq!(invocation.command, PathBuf::from("agy"));
        assert_eq!(
            invocation.args,
            vec![
                "--dangerously-skip-permissions".to_string(),
                "--add-dir".to_string(),
                temp_dir.path().display().to_string(),
                "-p".to_string(),
                "inspect this repo".to_string(),
            ]
        );
    }

    #[test]
    fn third_party_cli_families_use_prompt_flag() {
        for (launch_family, command, mode_args) in [
            (
                "claude",
                "claude",
                vec!["--dangerously-skip-permissions".to_string()],
            ),
            (
                "copilot",
                "copilot",
                vec![
                    "--autopilot".to_string(),
                    "--yolo".to_string(),
                    "--no-ask-user".to_string(),
                    "-s".to_string(),
                ],
            ),
            ("gemini", "gemini", Vec::new()),
            ("qwen", "qwen", vec!["-y".to_string()]),
        ] {
            let temp_dir = TempDir::new().expect("tempdir");
            let launch = test_launch(
                &temp_dir,
                ExternalCommandAgentBackendConfig {
                    command: command.to_string(),
                    protocol: ExternalCommandProtocol::RawCli,
                    args_write: mode_args.clone(),
                    launch_family: Some(launch_family.to_string()),
                    timeout_ms: 5_000,
                    ..Default::default()
                },
                false,
            );

            let invocation = build_external_agent_invocation(&launch, "inspect this repo")
                .expect("third-party invocation should build");

            let mut expected_args = mode_args;
            expected_args.extend(["-p".to_string(), "inspect this repo".to_string()]);
            assert_eq!(invocation.command, PathBuf::from(command));
            assert_eq!(invocation.args, expected_args, "family {launch_family}");
        }
    }

    #[test]
    fn positional_prompt_families_keep_bare_prompt() {
        for launch_family in ["code", "codex", "cloud"] {
            let temp_dir = TempDir::new().expect("tempdir");
            let launch = test_launch(
                &temp_dir,
                ExternalCommandAgentBackendConfig {
                    command: "coder".to_string(),
                    protocol: ExternalCommandProtocol::RawCli,
                    args: vec!["--model".to_string(), "gpt-5.5".to_string()],
                    args_write: vec![
                        "-s".to_string(),
                        "workspace-write".to_string(),
                        "exec".to_string(),
                        "--skip-git-repo-check".to_string(),
                    ],
                    launch_family: Some(launch_family.to_string()),
                    timeout_ms: 5_000,
                    ..Default::default()
                },
                false,
            );

            let invocation = build_external_agent_invocation(&launch, "inspect this repo")
                .expect("code-family invocation should build");

            assert_eq!(invocation.command, PathBuf::from("coder"));
            assert_eq!(
                invocation.args,
                vec![
                    "--model".to_string(),
                    "gpt-5.5".to_string(),
                    "-s".to_string(),
                    "workspace-write".to_string(),
                    "exec".to_string(),
                    "--skip-git-repo-check".to_string(),
                    "inspect this repo".to_string(),
                ],
                "family {launch_family}"
            );
            assert!(
                !invocation.args.iter().any(|arg| arg == "-p"),
                "family {launch_family} should not use prompt flag"
            );
        }
    }

    #[tokio::test]
    async fn missing_builtin_third_party_cli_reports_install_hint() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "definitely-missing-claude-code-test-command".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                launch_family: Some("claude".to_string()),
                timeout_ms: 5_000,
                ..Default::default()
            },
            /*is_read_only*/ false,
        );
        let err = preflight_external_agent_backend(
            launch.role.as_deref(),
            &launch.backend,
            &launch.cwd,
            launch.is_read_only,
        )
        .await
        .expect_err("missing built-in third-party CLI should fail preflight");

        assert_eq!(err.kind, ExternalAgentFailureKind::CommandMissing);
        let message = err.to_string();
        assert!(message.contains("Claude Code command"), "{message}");
        assert!(
            message.contains("definitely-missing-claude-code-test-command"),
            "{message}"
        );
        assert!(
            message.contains("Install claude-code") && message.contains("on PATH"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn antigravity_preflight_classifies_missing_authentication() {
        let temp_dir = TempDir::new().expect("tempdir");
        let script_path = temp_dir.path().join("fake-agy.sh");
        std::fs::write(
            &script_path,
            r#"if [ "$1" = "--version" ]; then
  echo "Antigravity CLI 1.2.3"
  exit 0
fi
if [ "$1" = "models" ]; then
  echo "Authentication required. Please sign in." >&2
  exit 1
fi
exit 2
"#,
        )
        .expect("write fake Antigravity CLI");
        let backend = ExternalCommandAgentBackendConfig {
            command: format!("/bin/sh {}", script_path.display()),
            protocol: ExternalCommandProtocol::RawCli,
            launch_family: Some("antigravity".to_string()),
            timeout_ms: 5_000,
            ..Default::default()
        };

        let err =
            preflight_external_agent_backend(Some("antigravity"), &backend, temp_dir.path(), true)
                .await
                .expect_err("signed-out Antigravity CLI should fail preflight");

        assert_eq!(err.kind, ExternalAgentFailureKind::AuthenticationRequired);
        assert!(err.to_string().contains("Authentication required"));
    }

    #[tokio::test]
    async fn antigravity_preflight_records_cli_version() {
        let temp_dir = TempDir::new().expect("tempdir");
        let script_path = temp_dir.path().join("fake-agy.sh");
        std::fs::write(
            &script_path,
            r#"if [ "$1" = "--version" ]; then
  echo "Antigravity CLI 1.2.3"
  exit 0
fi
if [ "$1" = "models" ]; then
  echo "Gemini 3.1 Pro"
  exit 0
fi
exit 2
"#,
        )
        .expect("write fake Antigravity CLI");
        let backend = ExternalCommandAgentBackendConfig {
            command: format!("/bin/sh {}", script_path.display()),
            protocol: ExternalCommandProtocol::RawCli,
            launch_family: Some("antigravity".to_string()),
            timeout_ms: 5_000,
            ..Default::default()
        };

        let provenance =
            preflight_external_agent_backend(Some("antigravity"), &backend, temp_dir.path(), true)
                .await
                .expect("authenticated Antigravity CLI should pass preflight");

        assert_eq!(
            provenance.cli_version.as_deref(),
            Some("Antigravity CLI 1.2.3")
        );
        assert_eq!(provenance.provider_family.as_deref(), Some("antigravity"));
    }

    #[tokio::test]
    async fn completed_preflight_is_not_repeated_during_launch() {
        let temp_dir = TempDir::new().expect("tempdir");
        let marker_path = temp_dir.path().join("preflight-reran");
        let script_path = temp_dir.path().join("fake-agy.sh");
        std::fs::write(
            &script_path,
            format!(
                r#"if [ "$1" = "--version" ] || [ "$1" = "models" ]; then
  : > '{}'
  exit 1
fi
echo "RUNTIME_OK"
"#,
                marker_path.display()
            ),
        )
        .expect("write fake Antigravity CLI");
        let mut launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: format!("/bin/sh {}", script_path.display()),
                protocol: ExternalCommandProtocol::RawCli,
                launch_family: Some("antigravity".to_string()),
                timeout_ms: 5_000,
                ..Default::default()
            },
            true,
        );
        launch.preflight_completed = true;

        let response = run_external_agent_inner(&launch)
            .await
            .expect("completed preflight should not run again");

        assert_eq!(response.status, ExternalAgentResponseStatus::Completed);
        assert_eq!(response.final_message.as_deref(), Some("RUNTIME_OK"));
        assert!(!marker_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preflight_resolves_backend_path_and_reuses_exact_command() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().expect("tempdir");
        let bin_dir = temp_dir.path().join("bin");
        tokio::fs::create_dir_all(&bin_dir)
            .await
            .expect("bin dir should be created");
        let command_path = bin_dir.join("fake-claude");
        tokio::fs::write(
            &command_path,
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "Claude Code 2.1.212"
  exit 0
fi
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  echo '{"loggedIn":true,"authMethod":"test"}'
  exit 0
fi
if [ "$1" = "-p" ]; then
  echo "PATH_COMMAND_OK"
  exit 0
fi
exit 2
"#,
        )
        .await
        .expect("fake Claude CLI should be written");
        let mut permissions = std::fs::metadata(&command_path)
            .expect("fake Claude CLI metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&command_path, permissions)
            .expect("fake Claude CLI should be executable");
        let backend = ExternalCommandAgentBackendConfig {
            command: "fake-claude".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            launch_family: Some("claude".to_string()),
            env: HashMap::from([("PATH".to_string(), bin_dir.display().to_string())]),
            timeout_ms: 5_000,
            ..Default::default()
        };

        let provider =
            preflight_external_agent_backend(Some("claude"), &backend, temp_dir.path(), true)
                .await
                .expect("backend PATH command should pass preflight");
        assert_eq!(provider.resolved_command(), Some(command_path.as_path()));

        let mut launch = test_launch(&temp_dir, backend, true);
        launch.preflight_completed = true;
        launch.resolved_command = provider
            .resolved_command()
            .map(std::path::Path::to_path_buf);
        let response = run_external_agent_inner(&launch)
            .await
            .expect("resolved command should launch successfully");
        assert_eq!(response.status, ExternalAgentResponseStatus::Completed);
        assert_eq!(response.final_message.as_deref(), Some("PATH_COMMAND_OK"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_preflight_kills_process_group() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().expect("tempdir");
        let pid_path = temp_dir.path().join("preflight.pid");
        let command_path = temp_dir.path().join("hanging-provider");
        tokio::fs::write(
            &command_path,
            format!("#!/bin/sh\necho $$ > '{}'\nsleep 30\n", pid_path.display()),
        )
        .await
        .expect("hanging provider should be written");
        let mut permissions = std::fs::metadata(&command_path)
            .expect("hanging provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&command_path, permissions)
            .expect("hanging provider should be executable");
        let backend = ExternalCommandAgentBackendConfig::default();

        let error = run_external_agent_preflight_command_with_timeout(
            &backend,
            &command_path,
            &[],
            temp_dir.path(),
            &["--version"],
            "version",
            Duration::from_millis(500),
        )
        .await
        .expect_err("hanging preflight should time out");
        assert_eq!(error.kind, ExternalAgentFailureKind::TimedOut);

        let pid: i32 = tokio::fs::read_to_string(&pid_path)
            .await
            .expect("preflight should record its pid")
            .trim()
            .parse()
            .expect("pid should parse");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let status = Command::new("/bin/kill")
                    .args(["-0", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .expect("kill probe should run");
                if !status.success() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed-out preflight process should be killed");
    }

    #[tokio::test]
    async fn first_party_and_custom_raw_cli_commands_require_executable_commands() {
        for launch_family in [Some("code"), Some("codex"), Some("cloud"), None] {
            let temp_dir = TempDir::new().expect("tempdir");
            let launch = test_launch(
                &temp_dir,
                ExternalCommandAgentBackendConfig {
                    command: "definitely-missing-custom-agent-test-command".to_string(),
                    protocol: ExternalCommandProtocol::RawCli,
                    launch_family: launch_family.map(str::to_string),
                    timeout_ms: 5_000,
                    ..Default::default()
                },
                false,
            );
            let err = preflight_external_agent_backend(
                launch.role.as_deref(),
                &launch.backend,
                &launch.cwd,
                launch.is_read_only,
            )
            .await
            .expect_err("missing external command should fail preflight");
            assert_eq!(err.kind, ExternalAgentFailureKind::CommandMissing);
        }
    }

    #[tokio::test]
    async fn non_github_copilot_command_is_rejected() {
        let temp_dir = TempDir::new().expect("tempdir");
        let command = std::env::current_exe().expect("current test executable");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: command.display().to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                launch_family: Some("copilot".to_string()),
                timeout_ms: 5_000,
                ..Default::default()
            },
            /*is_read_only*/ false,
        );
        let err = preflight_external_agent_backend(
            launch.role.as_deref(),
            &launch.backend,
            &launch.cwd,
            launch.is_read_only,
        )
        .await
        .expect_err("non-GitHub copilot executable should fail preflight");

        assert_eq!(err.kind, ExternalAgentFailureKind::LaunchFailed);
        let message = err.to_string();
        assert!(message.contains("resolved to a different `copilot` executable"));
        assert!(message.contains("Install GitHub Copilot CLI"));
    }

    #[tokio::test]
    async fn raw_cli_auth_failure_is_classified() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "/bin/sh".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                args: vec![
                    "-c".to_string(),
                    "echo 'Authentication required. Please sign in.'; exit 1".to_string(),
                ],
                timeout_ms: 5_000,
                ..Default::default()
            },
            true,
        );

        let err = run_external_agent_inner(&launch)
            .await
            .expect_err("authentication failure should fail the external agent");

        assert_eq!(
            err.detail.kind,
            ExternalAgentFailureKind::AuthenticationRequired
        );
    }

    #[tokio::test]
    async fn raw_cli_rate_limit_failure_is_classified() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "/bin/sh".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                args: vec![
                    "-c".to_string(),
                    "echo 'HTTP 429: quota exceeded' >&2; exit 1".to_string(),
                ],
                timeout_ms: 5_000,
                ..Default::default()
            },
            true,
        );

        let err = run_external_agent_inner(&launch)
            .await
            .expect_err("rate limit should fail the external agent");

        assert_eq!(
            err.detail.kind,
            ExternalAgentFailureKind::QuotaOrRateLimited
        );
    }

    #[tokio::test]
    async fn raw_cli_empty_output_is_classified() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "true".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                timeout_ms: 5_000,
                ..Default::default()
            },
            true,
        );

        let err = run_external_agent_inner(&launch)
            .await
            .expect_err("empty output should fail the external agent");

        assert_eq!(err.detail.kind, ExternalAgentFailureKind::EmptyOutput);
    }

    #[tokio::test]
    async fn malformed_json_output_is_classified() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "/bin/cat".to_string(),
                protocol: ExternalCommandProtocol::Json,
                timeout_ms: 5_000,
                ..Default::default()
            },
            true,
        );

        let err = run_external_agent_inner(&launch)
            .await
            .expect_err("request echo should not parse as an external response");

        assert_eq!(err.detail.kind, ExternalAgentFailureKind::MalformedOutput);
    }

    #[test]
    fn github_copilot_version_output_accepts_official_banner() {
        assert!(github_copilot_version_output(
            b"GitHub Copilot CLI 1.0.71.\n",
            b""
        ));
        assert!(github_copilot_version_output(
            b"",
            b"notice: GitHub Copilot CLI 1.0.71 is ready\n"
        ));
        assert!(!github_copilot_version_output(
            b"copilot version: 1.34.1\n",
            b""
        ));
    }

    #[test]
    fn bounded_preflight_output_preserves_utf8_boundaries() {
        let mut output = vec![b'x'];
        output.extend_from_slice("é".as_bytes());
        output.resize(MAX_PREFLIGHT_MESSAGE_BYTES + 2, b'y');

        let output = bounded_preflight_output(&output, b"");

        assert!(!output.contains('\u{fffd}'));
        assert!(output.len() <= MAX_PREFLIGHT_MESSAGE_BYTES);
    }

    #[test]
    fn antigravity_launch_cwd_uses_private_cache_dir() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                launch_family: Some("antigravity".to_string()),
                ..Default::default()
            },
            false,
        );

        let launch_cwd = external_agent_launch_cwd(&launch);

        assert!(launch_cwd.ends_with("agent-cache/antigravity"));
        assert_ne!(launch_cwd, launch.cwd);
    }

    #[tokio::test]
    async fn antigravity_launch_requires_existing_workspace_dir() {
        let temp_dir = TempDir::new().expect("tempdir");
        let missing_workspace = temp_dir.path().join("missing-workspace");
        let mut launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "/bin/echo".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                launch_family: Some("antigravity".to_string()),
                timeout_ms: 5_000,
                ..Default::default()
            },
            false,
        );
        launch.cwd = missing_workspace.clone();

        let err = run_external_agent_inner(&launch)
            .await
            .expect_err("missing antigravity workspace should fail before spawn");

        assert!(
            err.to_string().contains(&format!(
                "antigravity workspace directory does not exist: {}",
                missing_workspace.display()
            )),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn json_invocation_keeps_command_as_literal_path() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "/tmp/external agent/helper".to_string(),
                args: vec!["--json".to_string()],
                args_read_only: vec!["--readonly".to_string()],
                timeout_ms: 5_000,
                ..Default::default()
            },
            true,
        );

        let invocation = build_external_agent_invocation(&launch, "inspect this repo")
            .expect("json invocation should build");

        assert_eq!(
            invocation.command,
            PathBuf::from("/tmp/external agent/helper")
        );
        assert_eq!(
            invocation.args,
            vec!["--json".to_string(), "--readonly".to_string()]
        );
    }

    #[test]
    fn raw_cli_rejects_invalid_command_quoting() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "coder 'unterminated".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                timeout_ms: 5_000,
                ..Default::default()
            },
            false,
        );

        let err = build_external_agent_invocation(&launch, "inspect this repo")
            .expect_err("invalid raw cli command quoting should be rejected");

        assert!(
            err.to_string().contains("invalid shell quoting"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn raw_cli_uses_argv_prompt_and_configured_env() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "/bin/sh".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                args: vec![
                    "-c".to_string(),
                    "printf '%s|%s|%s' \"$1\" \"$2\" \"$EXTERNAL_AGENT_ENV\"".to_string(),
                    "external-agent-test".to_string(),
                ],
                args_write: vec!["--write-mode".to_string()],
                env: std::collections::HashMap::from([(
                    "EXTERNAL_AGENT_ENV".to_string(),
                    "configured".to_string(),
                )]),
                timeout_ms: 5_000,
                ..Default::default()
            },
            false,
        );

        let response = run_external_agent_inner(&launch)
            .await
            .expect("raw cli helper should complete");

        assert_eq!(response.status, ExternalAgentResponseStatus::Completed);
        assert_eq!(
            response.final_message.as_deref(),
            Some("--write-mode|inspect this repo|configured")
        );
    }

    #[test]
    fn external_agent_process_env_sets_artifact_target_scope() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                env: HashMap::from([
                    ("EXTERNAL_AGENT_ENV".to_string(), "configured".to_string()),
                    (
                        CARGO_TARGET_DIR_ENV_VAR.to_string(),
                        "/tmp/shared-target".to_string(),
                    ),
                    (
                        CODEX_LAB_CARGO_TARGET_DIR_ENV_VAR.to_string(),
                        "/tmp/explicit-target".to_string(),
                    ),
                    (
                        CODEX_LAB_CARGO_TARGET_SCOPE_ENV_VAR.to_string(),
                        "shared".to_string(),
                    ),
                    (
                        CODEX_LAB_CARGO_TARGET_KEY_ENV_VAR.to_string(),
                        "configured-key".to_string(),
                    ),
                ]),
                ..Default::default()
            },
            false,
        );

        let env = external_agent_process_env(&launch);

        assert_eq!(
            env.get("EXTERNAL_AGENT_ENV"),
            Some(&"configured".to_string())
        );
        assert_eq!(
            env.get(CODEX_LAB_CARGO_TARGET_SCOPE_ENV_VAR),
            Some(&EXTERNAL_AGENT_CARGO_TARGET_SCOPE_VALUE.to_string())
        );
        assert_eq!(
            env.get(CODEX_LAB_CARGO_TARGET_KEY_ENV_VAR),
            Some(&launch.thread_id.to_string())
        );
        assert_eq!(env.get(CARGO_TARGET_DIR_ENV_VAR), None);
        assert_eq!(env.get(CODEX_LAB_CARGO_TARGET_DIR_ENV_VAR), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn raw_cli_receives_artifact_target_scope_env() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "/bin/sh".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                args: vec![
                    "-c".to_string(),
                    format!(
                        "printf '%s|%s' \"${}\" \"${}\"",
                        CODEX_LAB_CARGO_TARGET_SCOPE_ENV_VAR, CODEX_LAB_CARGO_TARGET_KEY_ENV_VAR,
                    ),
                ],
                timeout_ms: 5_000,
                ..Default::default()
            },
            false,
        );
        let expected_thread_id = launch.thread_id.to_string();

        let response = run_external_agent_inner(&launch)
            .await
            .expect("raw cli helper should complete");
        let expected = format!("{EXTERNAL_AGENT_CARGO_TARGET_SCOPE_VALUE}|{expected_thread_id}");

        assert_eq!(response.status, ExternalAgentResponseStatus::Completed);
        assert_eq!(response.final_message.as_deref(), Some(expected.as_str()));
    }

    #[tokio::test]
    async fn oversized_output_is_truncated_instead_of_failing_wrapper() {
        let (mut writer, reader) = tokio::io::duplex(256);
        let payload = b"abcdefghijklmnopqrstuvwx".to_vec();
        let writer_task = tokio::spawn(async move {
            writer
                .write_all(&payload)
                .await
                .expect("write oversized payload");
        });

        let output = read_limited_output(reader, 8, "stdout")
            .await
            .expect("oversized output should truncate, not fail");
        writer_task.await.expect("writer task should finish");

        assert!(output.starts_with(EXTERNAL_AGENT_TRUNCATED_MARKER));
        assert!(output.ends_with(b"qrstuvwx"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_subprocess_stdout_keeps_tail_without_sigpipe_failure() {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "/bin/sh".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                args: vec![
                    "-c".to_string(),
                    "python3 - <<'PY'\nimport sys\nsys.stdout.write('a' * 70000)\nsys.stdout.write('tail-marker')\nPY"
                        .to_string(),
                ],
                timeout_ms: 5_000,
                ..Default::default()
            },
            false,
        );

        let response = run_external_agent_inner(&launch)
            .await
            .expect("oversized stdout should truncate without killing the child");

        assert_eq!(response.status, ExternalAgentResponseStatus::Completed);
        let final_message = response.final_message.expect("raw cli final message");
        assert!(final_message.starts_with("[external agent output truncated]"));
        assert!(final_message.ends_with("tail-marker"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_external_agent_background_children() {
        let temp_dir = TempDir::new().expect("tempdir");
        let survived_path = temp_dir.path().join("background-child-survived");
        let script = format!("(sleep 1; touch '{}') & wait", survived_path.display());
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "/bin/sh".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                args: vec!["-c".to_string(), script],
                timeout_ms: 100,
                ..Default::default()
            },
            false,
        );

        let err = run_external_agent_inner(&launch)
            .await
            .expect_err("external agent wrapper should time out");
        assert!(
            err.to_string().contains("timed out"),
            "unexpected error: {err}"
        );

        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert!(
            !survived_path.exists(),
            "timeout should kill background descendants in the external agent process group"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_background_children_after_wrapper_exits() {
        let temp_dir = TempDir::new().expect("tempdir");
        let survived_path = temp_dir.path().join("background-child-survived");
        let script = format!("(sleep 1; touch '{}') &", survived_path.display());
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "/bin/sh".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                args: vec!["-c".to_string(), script],
                timeout_ms: 100,
                ..Default::default()
            },
            false,
        );

        let err = run_external_agent_inner(&launch)
            .await
            .expect_err("external agent descendant should hold stdout open until timeout");
        assert!(
            err.to_string().contains("timed out"),
            "unexpected error: {err}"
        );

        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert!(
            !survived_path.exists(),
            "timeout should kill background descendants after the wrapper exits"
        );
    }
}
