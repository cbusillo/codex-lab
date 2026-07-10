use std::collections::HashMap;
use std::sync::LazyLock;

use serde_json::Value;

pub const CODE_VERSION: &str = {
    match option_env!("CODE_VERSION") {
        Some(version) => version,
        None => env!("CARGO_PKG_VERSION"),
    }
};

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
