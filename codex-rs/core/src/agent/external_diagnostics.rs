use crate::config::ExternalCommandAgentBackendConfig;
use crate::config::ExternalCommandProtocol;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemSandboxKind;
use codex_protocol::protocol::AgentStatus;
use serde::Serialize;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExternalAgentFailureKind {
    CommandMissing,
    AuthenticationRequired,
    QuotaOrRateLimited,
    UnsupportedMode,
    TimedOut,
    MalformedOutput,
    EmptyOutput,
    LaunchFailed,
    ProviderFailed,
}

impl ExternalAgentFailureKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CommandMissing => "command_missing",
            Self::AuthenticationRequired => "authentication_required",
            Self::QuotaOrRateLimited => "quota_or_rate_limited",
            Self::UnsupportedMode => "unsupported_mode",
            Self::TimedOut => "timed_out",
            Self::MalformedOutput => "malformed_output",
            Self::EmptyOutput => "empty_output",
            Self::LaunchFailed => "launch_failed",
            Self::ProviderFailed => "provider_failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ExternalAgentFailureDetail {
    pub(crate) kind: ExternalAgentFailureKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
}

impl ExternalAgentFailureDetail {
    pub(crate) fn new(kind: ExternalAgentFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: Some(message.into()),
        }
    }

    pub(crate) fn redacted(&self) -> Self {
        Self {
            kind: self.kind,
            message: None,
        }
    }
}

pub(crate) fn redact_external_agent_status(
    status: AgentStatus,
    failure: Option<&ExternalAgentFailureDetail>,
) -> AgentStatus {
    if !matches!(status, AgentStatus::Errored(_)) {
        return status;
    }
    AgentStatus::Errored(match failure {
        Some(failure) => format!("external agent failed: {}", failure.kind.as_str()),
        None => "external agent failed".to_string(),
    })
}

impl fmt::Display for ExternalAgentFailureDetail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.message.as_deref() {
            Some(message) => formatter.write_str(message),
            None => formatter.write_str(self.kind.as_str()),
        }
    }
}

impl std::error::Error for ExternalAgentFailureDetail {}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExternalAgentLaunchMode {
    ReadOnly,
    Write,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExternalAgentProtocol {
    Json,
    RawCli,
}

impl From<ExternalCommandProtocol> for ExternalAgentProtocol {
    fn from(protocol: ExternalCommandProtocol) -> Self {
        match protocol {
            ExternalCommandProtocol::Json => Self::Json,
            ExternalCommandProtocol::RawCli => Self::RawCli,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ExternalAgentProviderProvenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider_family: Option<String>,
    pub(crate) command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cli_version: Option<String>,
    pub(crate) protocol: ExternalAgentProtocol,
    pub(crate) mode: ExternalAgentLaunchMode,
    pub(crate) workspace: String,
    #[serde(skip)]
    resolved_command: Option<PathBuf>,
}

impl ExternalAgentProviderProvenance {
    pub(crate) fn new(
        agent_type: Option<&str>,
        backend: &ExternalCommandAgentBackendConfig,
        workspace: &Path,
        is_read_only: bool,
        cli_version: Option<String>,
    ) -> Self {
        Self {
            agent_type: agent_type.map(str::to_string),
            provider_family: backend.launch_family.clone(),
            command: external_agent_command_name(backend),
            cli_version,
            protocol: backend.protocol.into(),
            mode: if is_read_only {
                ExternalAgentLaunchMode::ReadOnly
            } else {
                ExternalAgentLaunchMode::Write
            },
            workspace: workspace.display().to_string(),
            resolved_command: None,
        }
    }

    pub(crate) fn set_resolved_command(&mut self, resolved_command: PathBuf) {
        self.resolved_command = Some(resolved_command);
    }

    pub(crate) fn resolved_command(&self) -> Option<&Path> {
        self.resolved_command.as_deref()
    }
}

pub(crate) fn permission_profile_is_read_only(profile: &PermissionProfile) -> bool {
    let file_system = profile.file_system_sandbox_policy();
    match file_system.kind {
        FileSystemSandboxKind::Restricted => file_system
            .entries
            .iter()
            .all(|entry| !entry.access.can_write()),
        FileSystemSandboxKind::Unrestricted | FileSystemSandboxKind::ExternalSandbox => false,
    }
}

pub(crate) fn classify_provider_failure_text(text: &str) -> ExternalAgentFailureKind {
    let text = text.to_ascii_lowercase();
    let contains_any = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));

    if contains_any(&[
        "rate limit",
        "rate_limit",
        "too many requests",
        "quota exceeded",
        "insufficient_quota",
        "resource exhausted",
        "http 429",
        "status 429",
        "api error: 429",
    ]) {
        return ExternalAgentFailureKind::QuotaOrRateLimited;
    }
    if contains_any(&[
        "authentication required",
        "not authenticated",
        "unauthorized",
        "auth failed",
        "invalid api key",
        "invalid token",
        "please sign in",
        "please log in",
        "login required",
        "sign in to",
        "log in to",
        "waiting for authentication",
    ]) {
        return ExternalAgentFailureKind::AuthenticationRequired;
    }
    if contains_any(&[
        "unknown command",
        "unknown flag",
        "unknown option",
        "unrecognized option",
        "unsupported mode",
        "invalid option",
    ]) {
        return ExternalAgentFailureKind::UnsupportedMode;
    }
    ExternalAgentFailureKind::ProviderFailed
}

fn external_agent_command_name(backend: &ExternalCommandAgentBackendConfig) -> String {
    let command = match backend.protocol {
        ExternalCommandProtocol::Json => backend.command.trim().to_string(),
        ExternalCommandProtocol::RawCli => shlex::split(backend.command.trim())
            .and_then(|parts| parts.into_iter().next())
            .unwrap_or_default(),
    };
    Path::new(&command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&command)
        .to_string()
}

#[cfg(test)]
#[path = "external_diagnostics_tests.rs"]
mod tests;
