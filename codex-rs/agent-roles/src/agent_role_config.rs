use codex_config::config_toml::ConfigToml;
use codex_utils_absolute_path::AbsolutePathBufGuard;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use toml::Value as TomlValue;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentRoleConfig {
    /// Human-facing role documentation used in spawn tool guidance.
    /// Required for loaded user-defined roles after deprecated/new metadata precedence resolves.
    pub description: Option<String>,
    /// Path to a role-specific config layer.
    pub config_file: Option<PathBuf>,
    /// Candidate nicknames for agents spawned with this role.
    pub nickname_candidates: Option<Vec<String>>,
    /// Optional backend used instead of spawning an internal Codex thread.
    pub backend: Option<AgentRoleBackendConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRoleBackendConfig {
    ExternalCommand(ExternalCommandAgentBackendConfig),
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Default)]
pub enum ExternalCommandProtocol {
    #[default]
    Json,
    RawCli,
}

impl From<codex_config::config_toml::ExternalCommandProtocolToml> for ExternalCommandProtocol {
    fn from(protocol: codex_config::config_toml::ExternalCommandProtocolToml) -> Self {
        match protocol {
            codex_config::config_toml::ExternalCommandProtocolToml::Json => Self::Json,
            codex_config::config_toml::ExternalCommandProtocolToml::RawCli => Self::RawCli,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalCommandAgentBackendConfig {
    pub command: String,
    pub protocol: ExternalCommandProtocol,
    pub args: Vec<String>,
    pub args_read_only: Vec<String>,
    pub args_write: Vec<String>,
    pub env: HashMap<String, String>,
    pub timeout_ms: u64,
    pub launch_family: Option<String>,
}

impl Default for ExternalCommandAgentBackendConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            protocol: ExternalCommandProtocol::Json,
            args: Vec::new(),
            args_read_only: Vec::new(),
            args_write: Vec::new(),
            env: HashMap::new(),
            timeout_ms: 30_000,
            launch_family: None,
        }
    }
}

impl AgentRoleBackendConfig {
    pub(crate) fn from_toml(backend: codex_config::config_toml::AgentRoleBackendToml) -> Self {
        match backend {
            codex_config::config_toml::AgentRoleBackendToml::ExternalCommand(command) => {
                Self::ExternalCommand(ExternalCommandAgentBackendConfig {
                    command: command.command,
                    protocol: command.protocol.into(),
                    args: command.args.unwrap_or_default(),
                    args_read_only: command.args_read_only.unwrap_or_default(),
                    args_write: command.args_write.unwrap_or_default(),
                    env: command.env.unwrap_or_default(),
                    timeout_ms: command.timeout_ms.unwrap_or(30_000),
                    launch_family: None,
                })
            }
        }
    }
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
struct RawAgentRoleFileToml {
    name: Option<String>,
    description: Option<String>,
    nickname_candidates: Option<Vec<String>>,
    backend: Option<codex_config::config_toml::AgentRoleBackendToml>,
    #[serde(flatten)]
    config: ConfigToml,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAgentRoleFile {
    pub role_name: String,
    pub description: Option<String>,
    pub nickname_candidates: Option<Vec<String>>,
    pub backend: Option<AgentRoleBackendConfig>,
    pub config: TomlValue,
}

pub fn parse_agent_role_file_contents(
    contents: &str,
    role_file_label: &Path,
    config_base_dir: &Path,
    role_name_hint: Option<&str>,
) -> std::io::Result<ResolvedAgentRoleFile> {
    let role_file_toml: TomlValue = toml::from_str(contents).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "failed to parse agent role file at {}: {err}",
                role_file_label.display()
            ),
        )
    })?;
    let _guard = AbsolutePathBufGuard::new(config_base_dir);
    let parsed: RawAgentRoleFileToml = role_file_toml.clone().try_into().map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "failed to deserialize agent role file at {}: {err}",
                role_file_label.display()
            ),
        )
    })?;
    let description = normalize_agent_role_description(
        &format!("agent role file {}.description", role_file_label.display()),
        parsed.description.as_deref(),
    )?;
    validate_agent_role_file_developer_instructions(
        role_file_label,
        parsed.config.developer_instructions.as_deref(),
        role_name_hint.is_none(),
    )?;

    let role_name = parsed
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| role_name_hint.map(ToOwned::to_owned))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "agent role file at {} must define a non-empty `name`",
                    role_file_label.display()
                ),
            )
        })?;

    let nickname_candidates = normalize_agent_role_nickname_candidates(
        &format!(
            "agent role file {}.nickname_candidates",
            role_file_label.display()
        ),
        parsed.nickname_candidates.as_deref(),
    )?;
    let backend = parsed
        .backend
        .map(normalize_agent_role_backend)
        .transpose()?;

    let mut config = role_file_toml;
    let Some(config_table) = config.as_table_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "agent role file at {} must contain a TOML table",
                role_file_label.display()
            ),
        ));
    };
    config_table.remove("name");
    config_table.remove("description");
    config_table.remove("nickname_candidates");
    config_table.remove("backend");

    Ok(ResolvedAgentRoleFile {
        role_name,
        description,
        nickname_candidates,
        backend,
        config,
    })
}

pub(crate) fn normalize_agent_role_backend(
    backend: codex_config::config_toml::AgentRoleBackendToml,
) -> std::io::Result<AgentRoleBackendConfig> {
    let backend = AgentRoleBackendConfig::from_toml(backend);
    match &backend {
        AgentRoleBackendConfig::ExternalCommand(command) => {
            if command.command.trim().is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "external_command backend command must not be empty",
                ));
            }
            if command.timeout_ms == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "external_command backend timeout_ms must be greater than 0",
                ));
            }
        }
    }
    Ok(backend)
}

pub(crate) fn normalize_agent_role_description(
    field_label: &str,
    description: Option<&str>,
) -> std::io::Result<Option<String>> {
    match description.map(str::trim) {
        Some("") => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{field_label} cannot be blank"),
        )),
        Some(description) => Ok(Some(description.to_string())),
        None => Ok(None),
    }
}

fn validate_agent_role_file_developer_instructions(
    role_file_label: &Path,
    developer_instructions: Option<&str>,
    require_present: bool,
) -> std::io::Result<()> {
    match developer_instructions.map(str::trim) {
        Some("") => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "agent role file at {}.developer_instructions cannot be blank",
                role_file_label.display()
            ),
        )),
        Some(_) => Ok(()),
        None if require_present => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "agent role file at {} must define `developer_instructions`",
                role_file_label.display()
            ),
        )),
        None => Ok(()),
    }
}

pub(crate) fn normalize_agent_role_nickname_candidates(
    field_label: &str,
    nickname_candidates: Option<&[String]>,
) -> std::io::Result<Option<Vec<String>>> {
    let Some(nickname_candidates) = nickname_candidates else {
        return Ok(None);
    };

    if nickname_candidates.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{field_label} must contain at least one name"),
        ));
    }

    let mut normalized_candidates = Vec::with_capacity(nickname_candidates.len());
    let mut seen_candidates = BTreeSet::new();

    for nickname in nickname_candidates {
        let normalized_nickname = nickname.trim();
        if normalized_nickname.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{field_label} cannot contain blank names"),
            ));
        }

        if !seen_candidates.insert(normalized_nickname.to_owned()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{field_label} cannot contain duplicates"),
            ));
        }

        if !normalized_nickname
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "{field_label} may only contain ASCII letters, digits, spaces, hyphens, and underscores"
                ),
            ));
        }

        normalized_candidates.push(normalized_nickname.to_owned());
    }

    Ok(Some(normalized_candidates))
}
