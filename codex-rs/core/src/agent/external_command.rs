use crate::agent::AgentControl;
use crate::agent::AgentStatus;
use crate::config::ExternalCommandAgentBackendConfig;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use serde::Deserialize;
use serde::Serialize;
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const MAX_EXTERNAL_AGENT_STDOUT_BYTES: usize = 64 * 1024;
const MAX_EXTERNAL_AGENT_STDERR_BYTES: usize = 8 * 1024;

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
            control.update_external_agent_status(thread_id, AgentStatus::Errored(message.clone()));
            send_completion_to_parent(&launch, &control, message.clone()).await;
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
            control.update_external_agent_status(thread_id, AgentStatus::Errored(message.clone()));
            send_completion_to_parent(&launch, &control, message.clone()).await;
        }
    }
}

async fn run_external_agent_inner(
    launch: &ExternalAgentLaunch,
) -> anyhow::Result<ExternalAgentResponse> {
    if launch.cancellation_token.is_cancelled() {
        return Err(anyhow::anyhow!("external agent cancelled before launch"));
    }

    let request = ExternalAgentRequest {
        protocol_version: 1,
        thread_id: launch.thread_id,
        parent_thread_id: launch.parent_thread_id,
        author: launch.author.to_string(),
        recipient: launch.recipient.to_string(),
        role: launch.role.clone(),
        task_name: launch.task_name.clone(),
        cwd: launch.cwd.display().to_string(),
        message: render_external_agent_message(&launch.initial_operation),
    };
    let request_json = serde_json::to_vec(&request)?;

    let mut command = Command::new(&launch.backend.command);
    command
        .args(&launch.backend.args)
        .current_dir(&launch.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    if launch.cancellation_token.is_cancelled() {
        return Err(anyhow::anyhow!("external agent cancelled before launch"));
    }

    let mut child = command.spawn()?;
    let mut stdin = child.stdin.take();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to open external agent stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to open external agent stderr"))?;

    let interaction = async move {
        let mut stdin = stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to open external agent stdin"))?;
        stdin.write_all(&request_json).await?;
        stdin.write_all(b"\n").await?;
        stdin.shutdown().await?;
        drop(stdin);

        let (stdout, stderr, status) = tokio::try_join!(
            read_limited_output(stdout, MAX_EXTERNAL_AGENT_STDOUT_BYTES, "stdout"),
            read_limited_output(stderr, MAX_EXTERNAL_AGENT_STDERR_BYTES, "stderr"),
            async { child.wait().await.map_err(anyhow::Error::from) },
        )?;

        Ok::<ExternalAgentProcessOutput, anyhow::Error>(ExternalAgentProcessOutput {
            status,
            stdout,
            stderr,
        })
    };

    let output = tokio::select! {
        _ = launch.cancellation_token.cancelled() => {
            return Err(anyhow::anyhow!("external agent cancelled"));
        }
        output = timeout(Duration::from_millis(launch.backend.timeout_ms), interaction) => {
            output.map_err(|_| anyhow::anyhow!("external agent timed out"))??
        },
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let reason = if stderr.is_empty() {
            format!("external agent exited with {}", output.status)
        } else {
            format!("external agent exited with {}: {stderr}", output.status)
        };
        return Err(anyhow::anyhow!(reason));
    }
    let response = serde_json::from_slice(&output.stdout)?;
    Ok(response)
}

async fn read_limited_output<R: AsyncRead + Unpin>(
    reader: R,
    limit: usize,
    stream_name: &str,
) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut reader = reader.take(limit.saturating_add(1) as u64);
    reader.read_to_end(&mut output).await?;
    if output.len() > limit {
        return Err(anyhow::anyhow!(
            "external agent {stream_name} exceeded limit"
        ));
    }
    Ok(output)
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
    use tempfile::TempDir;

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
            },
            cwd: temp_dir.path().to_path_buf(),
            cancellation_token,
        };

        let err = run_external_agent_inner(&launch)
            .await
            .expect_err("pre-cancelled external agent should fail before launch");
        assert!(err.to_string().contains("cancelled before launch"));
        assert!(!marker_path.exists(), "subprocess should not launch");
    }
}
