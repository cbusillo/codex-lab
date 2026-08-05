use super::external_diagnostics::ExternalAgentFailureDetail;
use super::external_diagnostics::ExternalAgentFailureKind;
use crate::config::ExternalCommandAgentBackendConfig;
use codex_config::agent_defaults::agent_model_specs;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const CAPABILITY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const FAILED_DISCOVERY_CACHE_TTL: Duration = Duration::from_secs(30);
const MAX_CAPABILITY_CACHE_ENTRIES: usize = 32;
const MAX_DISCOVERED_MODELS: usize = 32;
const MAX_ACTIVE_CAPABILITY_CATALOGS: usize = 32;
const MAX_MODEL_NAME_BYTES: usize = 128;

/// Identifies where an external-agent capability report came from.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalAgentCapabilitySource {
    StaticCatalog,
    LocalCli,
    NotProbed,
    ConservativeFallback,
}

/// Indicates whether a capability report was freshly probed or reused.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalAgentCapabilityFreshness {
    Fresh,
    Cached,
}

/// One model choice reported for an installed external-agent CLI.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExternalAgentModelCapability {
    pub selector: String,
    pub model: String,
    pub explicit_only: bool,
}

/// Bounded local capability information for an external-agent CLI.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExternalAgentCapabilities {
    pub cli_family: String,
    pub cli_version: Option<String>,
    pub supports_model_selection: bool,
    pub supports_effort_selection: bool,
    pub models: Vec<ExternalAgentModelCapability>,
    pub effort_levels: Vec<String>,
    pub source: ExternalAgentCapabilitySource,
    pub freshness: ExternalAgentCapabilityFreshness,
    pub observed_at_unix_seconds: u64,
    pub failure: Option<ExternalAgentFailureDetail>,
}

impl ExternalAgentCapabilities {
    pub(super) fn conservative(
        cli_family: impl Into<String>,
        cli_version: Option<String>,
        failure: ExternalAgentFailureDetail,
    ) -> Self {
        Self {
            cli_family: cli_family.into(),
            cli_version,
            supports_model_selection: false,
            supports_effort_selection: false,
            models: Vec::new(),
            effort_levels: Vec::new(),
            source: ExternalAgentCapabilitySource::ConservativeFallback,
            freshness: ExternalAgentCapabilityFreshness::Fresh,
            observed_at_unix_seconds: observed_at_unix_seconds(),
            failure: Some(failure),
        }
    }

    pub(super) fn not_probed(cli_family: impl Into<String>, cli_version: Option<String>) -> Self {
        Self {
            cli_family: cli_family.into(),
            cli_version,
            supports_model_selection: false,
            supports_effort_selection: false,
            models: Vec::new(),
            effort_levels: Vec::new(),
            source: ExternalAgentCapabilitySource::NotProbed,
            freshness: ExternalAgentCapabilityFreshness::Fresh,
            observed_at_unix_seconds: observed_at_unix_seconds(),
            failure: None,
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(super) struct ExternalAgentCapabilityCacheKey {
    resolved_command: PathBuf,
    command_args: Vec<String>,
    cli_family: String,
    cli_version: Option<String>,
}

impl ExternalAgentCapabilityCacheKey {
    pub(super) fn new(
        resolved_command: &Path,
        command_args: &[String],
        cli_family: &str,
        cli_version: Option<&str>,
    ) -> Self {
        Self {
            resolved_command: resolved_command.to_path_buf(),
            command_args: command_args.to_vec(),
            cli_family: cli_family.to_string(),
            cli_version: cli_version.map(str::to_string),
        }
    }
}

#[derive(Debug, Clone)]
struct CachedCapabilities {
    capabilities: ExternalAgentCapabilities,
    cached_at: Instant,
}

#[derive(Debug, Clone)]
struct ActiveCapabilityCatalogEntry {
    models: Vec<ExternalAgentModelCapability>,
    recorded_at: Instant,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(super) struct ExternalAgentDiscoveryCacheKey {
    command: String,
    protocol: crate::config::ExternalCommandProtocol,
    args: Vec<String>,
    args_read_only: Vec<String>,
    args_write: Vec<String>,
    env: BTreeMap<String, String>,
    timeout_ms: u64,
    launch_family: Option<String>,
    workspace: PathBuf,
}

impl ExternalAgentDiscoveryCacheKey {
    pub(super) fn new(backend: &ExternalCommandAgentBackendConfig, workspace: &Path) -> Self {
        Self {
            command: backend.command.clone(),
            protocol: backend.protocol,
            args: backend.args.clone(),
            args_read_only: backend.args_read_only.clone(),
            args_write: backend.args_write.clone(),
            env: backend
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            timeout_ms: backend.timeout_ms,
            launch_family: backend.launch_family.clone(),
            workspace: workspace.to_path_buf(),
        }
    }
}

#[derive(Debug, Clone)]
struct CachedDiscovery {
    capabilities: ExternalAgentCapabilities,
    cached_at: Instant,
}

static CAPABILITY_CACHE: LazyLock<
    Mutex<HashMap<ExternalAgentCapabilityCacheKey, CachedCapabilities>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

static ACTIVE_CAPABILITY_CATALOG: LazyLock<
    Mutex<BTreeMap<(String, String), ActiveCapabilityCatalogEntry>>,
> = LazyLock::new(|| Mutex::new(BTreeMap::new()));

static DISCOVERY_CACHE: LazyLock<Mutex<HashMap<ExternalAgentDiscoveryCacheKey, CachedDiscovery>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn cached_discovery(
    key: &ExternalAgentDiscoveryCacheKey,
) -> Option<ExternalAgentCapabilities> {
    let mut cache = DISCOVERY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cached = cache.get(key)?;
    let ttl = if cached.capabilities.failure.is_some() {
        FAILED_DISCOVERY_CACHE_TTL
    } else {
        CAPABILITY_CACHE_TTL
    };
    if cached.cached_at.elapsed() > ttl {
        cache.remove(key);
        return None;
    }
    let mut capabilities = cached.capabilities.clone();
    capabilities.freshness = ExternalAgentCapabilityFreshness::Cached;
    Some(capabilities)
}

pub(super) fn cache_discovery(
    key: ExternalAgentDiscoveryCacheKey,
    capabilities: &ExternalAgentCapabilities,
) {
    let mut cache = DISCOVERY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.len() >= MAX_CAPABILITY_CACHE_ENTRIES
        && !cache.contains_key(&key)
        && let Some(oldest_key) = cache
            .iter()
            .min_by_key(|(_, cached)| cached.cached_at)
            .map(|(key, _)| key.clone())
    {
        cache.remove(&oldest_key);
    }
    cache.insert(
        key,
        CachedDiscovery {
            capabilities: capabilities.clone(),
            cached_at: Instant::now(),
        },
    );
}

pub(crate) fn record_active_capability_catalog(
    backend: &ExternalCommandAgentBackendConfig,
    workspace: &Path,
    capabilities: &ExternalAgentCapabilities,
) {
    if capabilities.cli_family != "antigravity" {
        return;
    }
    let mut models = capabilities.models.clone();
    models.sort_by(|left, right| left.selector.cmp(&right.selector));
    models.dedup_by(|left, right| left.selector == right.selector);
    let mut catalog = ACTIVE_CAPABILITY_CATALOG
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = (
        backend.command.trim().to_string(),
        workspace.display().to_string(),
    );
    if catalog.len() >= MAX_ACTIVE_CAPABILITY_CATALOGS && !catalog.contains_key(&key) {
        catalog.pop_first();
    }
    catalog.insert(
        key,
        ActiveCapabilityCatalogEntry {
            models: models.into_iter().take(MAX_DISCOVERED_MODELS).collect(),
            recorded_at: Instant::now(),
        },
    );
}

#[cfg(test)]
pub(crate) fn clear_active_capability_catalog() {
    ACTIVE_CAPABILITY_CATALOG
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

pub(crate) fn discovered_antigravity_selectors(
    backend: &ExternalCommandAgentBackendConfig,
    workspace: &Path,
) -> Vec<ExternalAgentModelCapability> {
    let mut catalog = ACTIVE_CAPABILITY_CATALOG
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = (
        backend.command.trim().to_string(),
        workspace.display().to_string(),
    );
    let Some(entry) = catalog.get(&key) else {
        return Vec::new();
    };
    if entry.recorded_at.elapsed() > CAPABILITY_CACHE_TTL {
        catalog.remove(&key);
        return Vec::new();
    }
    entry.models.clone()
}

pub(crate) fn looks_like_antigravity_selector(selector: &str) -> bool {
    selector
        .strip_prefix("antigravity-")
        .is_some_and(|model| !model.is_empty())
}

pub(crate) fn is_valid_antigravity_model_name(model: &str) -> bool {
    !model.is_empty()
        && !model.starts_with('-')
        && model.len() <= MAX_MODEL_NAME_BYTES
        && model
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.:/+".contains(character))
}

pub(super) fn cached_capabilities(
    key: &ExternalAgentCapabilityCacheKey,
) -> Option<ExternalAgentCapabilities> {
    let mut cache = CAPABILITY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cached = cache.get(key)?;
    if cached.cached_at.elapsed() > CAPABILITY_CACHE_TTL {
        cache.remove(key);
        return None;
    }

    let mut capabilities = cached.capabilities.clone();
    capabilities.freshness = ExternalAgentCapabilityFreshness::Cached;
    Some(capabilities)
}

pub(super) fn cache_capabilities(
    key: ExternalAgentCapabilityCacheKey,
    capabilities: &ExternalAgentCapabilities,
) {
    if capabilities.failure.is_some() {
        return;
    }

    let mut cache = CAPABILITY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.len() >= MAX_CAPABILITY_CACHE_ENTRIES
        && !cache.contains_key(&key)
        && let Some(oldest_key) = cache
            .iter()
            .min_by_key(|(_, cached)| cached.cached_at)
            .map(|(key, _)| key.clone())
    {
        cache.remove(&oldest_key);
    }
    cache.insert(
        key,
        CachedCapabilities {
            capabilities: capabilities.clone(),
            cached_at: Instant::now(),
        },
    );
}

pub(super) fn claude_capabilities(
    cli_version: Option<String>,
    help_output: &[u8],
    help_truncated: bool,
) -> ExternalAgentCapabilities {
    if help_truncated {
        return ExternalAgentCapabilities::conservative(
            "claude",
            cli_version,
            ExternalAgentFailureDetail::new(
                ExternalAgentFailureKind::MalformedOutput,
                "Claude Code capability output exceeded the local probe limit",
            ),
        );
    }

    let supports_model_selection = help_supports_flag(help_output, "--model");
    let supports_effort_selection = help_supports_flag(help_output, "--effort");
    let models = agent_model_specs()
        .iter()
        .filter(|spec| spec.family == "claude")
        .filter_map(|spec| {
            let ["--model", model] = spec.model_args else {
                return None;
            };
            Some(ExternalAgentModelCapability {
                selector: spec.slug.to_string(),
                model: (*model).to_string(),
                explicit_only: spec.explicit_only,
            })
        })
        .take(MAX_DISCOVERED_MODELS)
        .collect();

    ExternalAgentCapabilities {
        cli_family: "claude".to_string(),
        cli_version,
        supports_model_selection,
        supports_effort_selection,
        models,
        effort_levels: if supports_effort_selection {
            ["low", "medium", "high", "xhigh", "max"]
                .map(str::to_string)
                .to_vec()
        } else {
            Vec::new()
        },
        source: ExternalAgentCapabilitySource::StaticCatalog,
        freshness: ExternalAgentCapabilityFreshness::Fresh,
        observed_at_unix_seconds: observed_at_unix_seconds(),
        failure: None,
    }
}

pub(super) fn antigravity_capabilities(
    cli_version: Option<String>,
    models_output: &[u8],
    models_truncated: bool,
    help_output: &[u8],
    help_truncated: bool,
) -> ExternalAgentCapabilities {
    if models_truncated || help_truncated {
        return ExternalAgentCapabilities::conservative(
            "antigravity",
            cli_version,
            ExternalAgentFailureDetail::new(
                ExternalAgentFailureKind::MalformedOutput,
                "Antigravity capability output exceeded the local probe limit",
            ),
        );
    }

    let model_names = match parse_antigravity_models(models_output) {
        Ok(models) => models,
        Err(failure) => {
            return ExternalAgentCapabilities::conservative("antigravity", cli_version, failure);
        }
    };
    let supports_model_selection = help_supports_flag(help_output, "--model");
    let supports_effort_selection = help_supports_flag(help_output, "--effort");
    let models = model_names
        .into_iter()
        .map(|model| ExternalAgentModelCapability {
            selector: format!("antigravity-{model}"),
            model,
            explicit_only: false,
        })
        .collect();

    ExternalAgentCapabilities {
        cli_family: "antigravity".to_string(),
        cli_version,
        supports_model_selection,
        supports_effort_selection,
        models,
        effort_levels: if supports_effort_selection {
            ["low", "medium", "high"].map(str::to_string).to_vec()
        } else {
            Vec::new()
        },
        source: ExternalAgentCapabilitySource::LocalCli,
        freshness: ExternalAgentCapabilityFreshness::Fresh,
        observed_at_unix_seconds: observed_at_unix_seconds(),
        failure: None,
    }
}

pub(super) fn validate_requested_capabilities(
    backend: &ExternalCommandAgentBackendConfig,
    command_args: &[String],
    is_read_only: bool,
    capabilities: &ExternalAgentCapabilities,
) -> Result<(), ExternalAgentFailureDetail> {
    let mut args = command_args.to_vec();
    args.extend(backend.args.iter().cloned());
    args.extend(if is_read_only {
        backend.args_read_only.iter().cloned()
    } else {
        backend.args_write.iter().cloned()
    });
    let requested_model = requested_flag_value(&args, "--model");
    let requested_effort = requested_flag_value(&args, "--effort");

    if requested_model.is_none() && requested_effort.is_none() {
        return Ok(());
    }
    if let Some(failure) = capabilities.failure.as_ref() {
        return Err(ExternalAgentFailureDetail::new(
            failure.kind,
            format!(
                "cannot validate the requested {} capability: {}",
                capabilities.cli_family, failure
            ),
        ));
    }

    if let Some(model) = requested_model {
        if !capabilities.supports_model_selection {
            return Err(unsupported_flag(capabilities, "--model", &model));
        }
        if !capabilities
            .models
            .iter()
            .any(|available| available.model.eq_ignore_ascii_case(&model))
        {
            let available = capabilities
                .models
                .iter()
                .map(|available| available.model.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ExternalAgentFailureDetail::new(
                ExternalAgentFailureKind::UnsupportedMode,
                format!(
                    "{} model `{model}` was not reported by the installed CLI. Available models: {available}",
                    capabilities.cli_family
                ),
            ));
        }
    }

    if let Some(effort) = requested_effort {
        if !capabilities.supports_effort_selection {
            return Err(unsupported_flag(capabilities, "--effort", &effort));
        }
        if !capabilities
            .effort_levels
            .iter()
            .any(|available| available.eq_ignore_ascii_case(&effort))
        {
            return Err(ExternalAgentFailureDetail::new(
                ExternalAgentFailureKind::UnsupportedMode,
                format!(
                    "{} effort `{effort}` is unsupported. Available efforts: {}",
                    capabilities.cli_family,
                    capabilities.effort_levels.join(", ")
                ),
            ));
        }
    }

    Ok(())
}

fn parse_antigravity_models(output: &[u8]) -> Result<Vec<String>, ExternalAgentFailureDetail> {
    let output = String::from_utf8_lossy(output);
    let mut models = Vec::new();
    let mut seen = HashSet::new();

    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if !is_valid_antigravity_model_name(line) {
            return Err(ExternalAgentFailureDetail::new(
                ExternalAgentFailureKind::MalformedOutput,
                "Antigravity returned a malformed model identifier",
            ));
        }
        if seen.insert(line.to_ascii_lowercase()) {
            models.push(line.to_string());
        }
        if models.len() > MAX_DISCOVERED_MODELS {
            return Err(ExternalAgentFailureDetail::new(
                ExternalAgentFailureKind::MalformedOutput,
                format!(
                    "Antigravity returned more than {MAX_DISCOVERED_MODELS} models; refusing the unbounded result"
                ),
            ));
        }
    }

    if models.is_empty() {
        return Err(ExternalAgentFailureDetail::new(
            ExternalAgentFailureKind::EmptyOutput,
            "Antigravity returned no available models",
        ));
    }
    Ok(models)
}

fn help_supports_flag(output: &[u8], flag: &str) -> bool {
    String::from_utf8_lossy(output).lines().any(|line| {
        line.split_whitespace()
            .any(|token| token.trim_end_matches([',', ':']) == flag)
    })
}

fn requested_flag_value(args: &[String], flag: &str) -> Option<String> {
    let inline_prefix = format!("{flag}=");
    let mut value = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == flag {
            value = args.next().cloned();
        } else if let Some(inline) = arg.strip_prefix(&inline_prefix) {
            value = Some(inline.to_string());
        }
    }
    value.filter(|value| !value.is_empty())
}

fn unsupported_flag(
    capabilities: &ExternalAgentCapabilities,
    flag: &str,
    value: &str,
) -> ExternalAgentFailureDetail {
    let version = capabilities
        .cli_version
        .as_deref()
        .map(|version| format!(" {version}"))
        .unwrap_or_default();
    ExternalAgentFailureDetail::new(
        ExternalAgentFailureKind::UnsupportedMode,
        format!(
            "{} CLI{version} does not advertise {flag}; requested value `{value}` cannot be used",
            capabilities.cli_family
        ),
    )
}

fn observed_at_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
pub(super) fn clear_capability_cache() {
    CAPABILITY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    DISCOVERY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

#[cfg(test)]
#[path = "external_capabilities_tests.rs"]
mod tests;
