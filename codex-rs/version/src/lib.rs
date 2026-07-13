use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use serde_json::Value;
use serde_json::json;

pub const CODE_VERSION: &str = {
    match option_env!("CODE_VERSION") {
        Some(version) => version,
        None => env!("CARGO_PKG_VERSION"),
    }
};

pub const BUILD_PROVENANCE_SCHEMA_VERSION: u32 = 1;
const UNAVAILABLE: &str = "unavailable";
const BUILD_SOURCE_COMMIT: Option<&str> = option_env!("CODEX_BUILD_SOURCE_COMMIT");
const BUILD_DIRTY_STATE: Option<&str> = option_env!("CODEX_BUILD_DIRTY_STATE");
const BUILD_PROFILE: Option<&str> = option_env!("CODEX_BUILD_PROFILE");
const BUILD_CHANNEL: Option<&str> = option_env!("CODEX_BUILD_CHANNEL");

/// Versioned source and executable identity for the currently running binary.
///
/// The source fields form one atomic tuple: if commit, dirty state, or channel is missing or
/// malformed, all three report `unavailable`. Git-derived dirty state is a build-time snapshot of
/// tracked files only; ignored and untracked files are intentionally excluded. Source archives can
/// provide the tuple through `CODEX_BUILD_COMMIT`, `CODEX_BUILD_DIRTY`, and
/// `CODEX_BUILD_CHANNEL`; Bazel callers pass those variables with `--action_env`. The executable
/// path comes from [`std::env::current_exe`], not `argv[0]`, and may expose local usernames or
/// install paths. Keep that field behind explicit local diagnostics unless its disclosure is
/// separately reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildProvenance {
    pub schema_version: u32,
    pub version: String,
    pub source_commit: String,
    pub dirty_state: DirtyState,
    pub build_profile: String,
    pub build_channel: String,
    pub executable_path: String,
}

impl BuildProvenance {
    pub fn as_json(&self) -> Value {
        json!({
            "schema_version": self.schema_version,
            "version": self.version,
            "source_commit": self.source_commit,
            "dirty_state": self.dirty_state.as_str(),
            "build_profile": self.build_profile,
            "build_channel": self.build_channel,
            "executable_path": self.executable_path,
        })
    }
}

/// Whether the binary was built from a clean, dirty, or unavailable source tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyState {
    Clean,
    Dirty,
    Unavailable,
}

impl DirtyState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Dirty => "dirty",
            Self::Unavailable => UNAVAILABLE,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "clean" => Some(Self::Clean),
            "dirty" => Some(Self::Dirty),
            UNAVAILABLE => Some(Self::Unavailable),
            _ => None,
        }
    }
}

struct BuildProvenanceMetadata<'a> {
    version: &'a str,
    source_commit: Option<&'a str>,
    dirty_state: Option<&'a str>,
    build_profile: Option<&'a str>,
    build_channel: Option<&'a str>,
}

struct SourceMetadata {
    source_commit: String,
    dirty_state: DirtyState,
    build_channel: String,
}

impl SourceMetadata {
    fn unavailable() -> Self {
        Self {
            source_commit: UNAVAILABLE.to_string(),
            dirty_state: DirtyState::Unavailable,
            build_channel: UNAVAILABLE.to_string(),
        }
    }

    fn new(source_commit: &str, dirty_state: &str, build_channel: &str) -> Option<Self> {
        let source_commit = normalized_commit(source_commit)?;
        let dirty_state = DirtyState::parse(dirty_state)?;
        let build_channel = normalized_value(build_channel)?;
        if dirty_state == DirtyState::Unavailable || build_channel == UNAVAILABLE {
            return None;
        }

        Some(Self {
            source_commit,
            dirty_state,
            build_channel,
        })
    }

    fn from_compile_env(
        source_commit: Option<&str>,
        dirty_state: Option<&str>,
        build_channel: Option<&str>,
    ) -> Self {
        match (source_commit, dirty_state, build_channel) {
            (Some(UNAVAILABLE), Some(UNAVAILABLE), Some(UNAVAILABLE)) => Self::unavailable(),
            (Some(source_commit), Some(dirty_state), Some(build_channel)) => {
                Self::new(source_commit, dirty_state, build_channel)
                    .unwrap_or_else(Self::unavailable)
            }
            _ => Self::unavailable(),
        }
    }
}

/// Returns the build provenance for the currently running process.
pub fn build_provenance() -> BuildProvenance {
    provenance_from_metadata(
        BuildProvenanceMetadata {
            version: CODE_VERSION,
            source_commit: BUILD_SOURCE_COMMIT,
            dirty_state: BUILD_DIRTY_STATE,
            build_profile: BUILD_PROFILE,
            build_channel: BUILD_CHANNEL,
        },
        std::env::current_exe().ok(),
    )
}

fn provenance_from_metadata(
    metadata: BuildProvenanceMetadata<'_>,
    executable_path: Option<PathBuf>,
) -> BuildProvenance {
    let source = SourceMetadata::from_compile_env(
        metadata.source_commit,
        metadata.dirty_state,
        metadata.build_channel,
    );

    BuildProvenance {
        schema_version: BUILD_PROVENANCE_SCHEMA_VERSION,
        version: metadata.version.to_string(),
        source_commit: source.source_commit,
        dirty_state: source.dirty_state,
        build_profile: normalized_value(metadata.build_profile.unwrap_or(UNAVAILABLE))
            .unwrap_or_else(|| UNAVAILABLE.to_string()),
        build_channel: source.build_channel,
        executable_path: executable_path
            .and_then(|path| path.into_os_string().into_string().ok())
            .filter(|path| !path.trim().is_empty())
            .unwrap_or_else(|| UNAVAILABLE.to_string()),
    }
}

fn normalized_commit(value: &str) -> Option<String> {
    let value = value.trim();
    if value == UNAVAILABLE
        || !matches!(value.len(), 40 | 64)
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }

    Some(value.to_ascii_lowercase())
}

fn normalized_value(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_graphic()))
        .then(|| value.to_string())
}

const ANNOUNCEMENT_TIP: &str = include_str!("../../../announcement_tip.toml");
const MODELS_MANIFEST: &str = include_str!("../../models-manager/models.json");
pub const MIN_WIRE_COMPAT_VERSION_FALLBACK: &str = "0.101.0";

static MIN_WIRE_COMPAT_VERSION: LazyLock<String> = LazyLock::new(|| {
    extract_max_semver(ANNOUNCEMENT_TIP)
        .map_or(MIN_WIRE_COMPAT_VERSION_FALLBACK, |version| {
            max_semver(MIN_WIRE_COMPAT_VERSION_FALLBACK, version)
        })
        .to_string()
});

static MODEL_MINIMUM_CLIENT_VERSIONS: LazyLock<HashMap<String, String>> =
    LazyLock::new(|| parse_model_minimum_client_versions(MODELS_MANIFEST));

const EVERY_CODE_MODEL_MINIMUM_CLIENT_VERSIONS: &[(&str, &str)] = &[
    ("gpt-5.6-sol", "0.144.0"),
    ("gpt-5.6-terra", "0.144.0"),
    ("gpt-5.6-luna", "0.144.0"),
];

fn max_semver<'a>(current: &'a str, candidate: &'a str) -> &'a str {
    let Some(current_triplet) = parse_semver_triplet(current) else {
        return candidate;
    };
    let Some(candidate_triplet) = parse_semver_triplet(candidate) else {
        return current;
    };

    if candidate_triplet > current_triplet {
        candidate
    } else {
        current
    }
}

#[cfg(test)]
#[path = "provenance_tests.rs"]
mod provenance_tests;

#[cfg(test)]
#[path = "../build.rs"]
#[allow(dead_code)]
mod build_script_tests;

fn parse_semver_triplet(version: &str) -> Option<(u64, u64, u64)> {
    let trimmed = version.trim().trim_start_matches('v');
    let core = trimmed
        .split_once(['-', '+'])
        .map_or(trimmed, |(version, _)| version);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;

    if parts.next().is_some() {
        return None;
    }

    Some((major, minor, patch))
}

fn extract_max_semver(input: &'static str) -> Option<&'static str> {
    let mut maximum: Option<((u64, u64, u64), &'static str)> = None;

    for token in input.split_whitespace() {
        let candidate = token.trim_matches(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '-' | '+' | 'v'))
        });
        let Some(triplet) = parse_semver_triplet(candidate) else {
            continue;
        };

        if maximum
            .as_ref()
            .is_none_or(|(current_maximum, _)| triplet > *current_maximum)
        {
            maximum = Some((triplet, candidate));
        }
    }

    maximum.map(|(_, version)| version)
}

fn parse_model_minimum_client_versions(input: &str) -> HashMap<String, String> {
    let Ok(root) = serde_json::from_str::<Value>(input) else {
        return HashMap::new();
    };
    let Some(models) = root.get("models").and_then(Value::as_array) else {
        return HashMap::new();
    };
    let mut versions = HashMap::new();

    for model in models {
        let Some(slug) = model.get("slug").and_then(Value::as_str) else {
            continue;
        };
        let Some(candidate) = model.get("minimal_client_version").and_then(Value::as_str) else {
            continue;
        };
        if parse_semver_triplet(candidate).is_some() {
            versions.insert(slug.to_ascii_lowercase(), candidate.to_string());
        }
    }

    for (slug, candidate) in EVERY_CODE_MODEL_MINIMUM_CLIENT_VERSIONS {
        if parse_semver_triplet(candidate).is_some() {
            versions.insert(slug.to_ascii_lowercase(), (*candidate).to_string());
        }
    }

    versions
}

fn wire_compatible_version_for<'a>(version: &'a str, minimum: &'a str) -> &'a str {
    let Some(version_triplet) = parse_semver_triplet(version) else {
        return version;
    };
    let Some(minimum_triplet) = parse_semver_triplet(minimum) else {
        return version;
    };

    if version_triplet < minimum_triplet {
        minimum
    } else {
        version
    }
}

pub fn version() -> &'static str {
    CODE_VERSION
}

pub fn min_wire_compat_version() -> &'static str {
    MIN_WIRE_COMPAT_VERSION.as_str()
}

pub fn wire_compatible_version() -> &'static str {
    wire_compatible_version_for(CODE_VERSION, min_wire_compat_version())
}

pub fn wire_compatible_version_for_model(model: &str) -> String {
    let canonical_model = model.rsplit('/').next().unwrap_or(model).trim();
    let Some(required_version) =
        MODEL_MINIMUM_CLIENT_VERSIONS.get(&canonical_model.to_ascii_lowercase())
    else {
        return wire_compatible_version().to_string();
    };

    max_semver(wire_compatible_version(), required_version).to_string()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn wire_compat_clamps_old_versions() {
        assert_eq!(wire_compatible_version_for("0.6.116", "0.144.0"), "0.144.0");
    }

    #[test]
    fn wire_compat_keeps_new_versions() {
        assert_eq!(wire_compatible_version_for("0.145.0", "0.144.0"), "0.145.0");
    }

    #[test]
    fn model_versions_come_from_manifest_and_every_code_overrides() {
        assert_eq!(wire_compatible_version_for_model("gpt-5.5"), "0.124.0");
        assert_eq!(
            wire_compatible_version_for_model("openai/gpt-5.6-terra"),
            "0.144.0"
        );
        assert_eq!(wire_compatible_version_for_model("gpt-5.6-luna"), "0.144.0");
    }

    #[test]
    fn unknown_models_use_the_global_wire_minimum() {
        assert_eq!(
            wire_compatible_version_for_model("unknown-model"),
            wire_compatible_version()
        );
    }
}
