use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io;
use std::path::Path;

use codex_utils_cache::sha1_digest;

pub(super) const CACHE_VERSION: &str = "v1";
const CARGO_COMPILER_WRAPPER_ENVIRONMENT_NAMES: [&str; 2] =
    ["RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct CargoValidationCacheKey(String);

impl CargoValidationCacheKey {
    pub(crate) fn for_command(
        repository_root: &Path,
        checkout_root: &Path,
        toolchain_identity: &str,
        command: &[String],
        environment: &HashMap<String, String>,
    ) -> io::Result<Self> {
        let target = cargo_target_semantics(environment);
        let command = normalized_cargo_command(command, checkout_root);
        let environment = cargo_cache_environment(environment);
        Ok(Self::new(
            repository_root,
            checkout_root,
            toolchain_identity,
            &target,
            &command,
            &environment,
        ))
    }

    pub(crate) fn new(
        repository_root: &Path,
        checkout_root: &Path,
        toolchain: &str,
        target: &str,
        command: &[String],
        environment: &BTreeMap<String, String>,
    ) -> Self {
        let mut material = Vec::new();
        append_key_part(&mut material, CACHE_VERSION.as_bytes());
        append_key_part(
            &mut material,
            repository_root.as_os_str().as_encoded_bytes(),
        );
        append_key_part(&mut material, checkout_root.as_os_str().as_encoded_bytes());
        append_key_part(&mut material, toolchain.as_bytes());
        append_key_part(&mut material, target.as_bytes());
        for argument in command {
            append_key_part(&mut material, argument.as_bytes());
        }
        for (name, value) in environment {
            append_key_part(&mut material, name.as_bytes());
            append_key_part(&mut material, value.as_bytes());
        }
        Self(hex_digest(sha1_digest(&material)))
    }

    pub(crate) fn semantic_id(&self) -> &str {
        &self.0
    }

    pub(super) fn digest(&self) -> &str {
        &self.0
    }
}

pub(crate) fn normalized_cargo_command(command: &[String], repository_root: &Path) -> Vec<String> {
    let mut command = command.to_vec();
    for flag in ["--manifest-path", "--target-dir"] {
        if let Some(value_index) = command
            .iter()
            .position(|argument| argument == flag)
            .and_then(|index| index.checked_add(1))
            && let Some(value) = command.get_mut(value_index)
        {
            if flag == "--target-dir" {
                *value = "target".to_string();
            } else {
                let manifest = Path::new(value);
                *value = manifest
                    .strip_prefix(repository_root)
                    .unwrap_or(manifest)
                    .display()
                    .to_string();
            }
        }
    }
    command
}

pub(crate) fn cargo_toolchain_identity(
    toolchain: Option<&str>,
    resolved_version: &str,
) -> Option<String> {
    let resolved_version = resolved_version.trim();
    if resolved_version.is_empty() {
        return None;
    }
    Some(format!(
        "declared={}\nresolved={}",
        toolchain.unwrap_or("ambient toolchain"),
        resolved_version
    ))
}

pub(crate) fn cargo_toolchain_allows_success_suppression(
    toolchain: Option<&str>,
    environment: &HashMap<String, String>,
) -> bool {
    if let Some(override_channel) = environment_value(environment, "RUSTUP_TOOLCHAIN") {
        return rustup_channel_is_immutable(override_channel);
    }
    let Some((name, contents)) = toolchain.and_then(|toolchain| toolchain.split_once('\n')) else {
        return false;
    };
    let channel = match name {
        "rust-toolchain.toml" => toml::from_str::<toml::Value>(contents)
            .ok()
            .and_then(|value| {
                value
                    .get("toolchain")?
                    .get("channel")?
                    .as_str()
                    .map(ToOwned::to_owned)
            }),
        "rust-toolchain" => contents
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(ToOwned::to_owned),
        _ => None,
    };
    channel.is_some_and(|channel| rustup_channel_is_immutable(&channel))
}

fn rustup_channel_is_immutable(channel: &str) -> bool {
    let channel = channel.trim();
    let version = channel.split('-').next().unwrap_or_default();
    let mut parts = version.split('.');
    if matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(major), Some(minor), Some(patch), None)
            if [major, minor, patch]
                .into_iter()
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    ) {
        return true;
    }

    ["nightly-", "beta-"]
        .into_iter()
        .filter_map(|prefix| channel.strip_prefix(prefix))
        .any(|rest| {
            let Some(date) = rest.get(..10) else {
                return false;
            };
            let suffix = &rest[10..];
            date.as_bytes().get(4) == Some(&b'-')
                && date.as_bytes().get(7) == Some(&b'-')
                && date
                    .bytes()
                    .enumerate()
                    .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
                && (suffix.is_empty() || suffix.starts_with('-'))
        })
}

fn cargo_target_semantics(environment: &HashMap<String, String>) -> String {
    if let Some(target) = environment_value(environment, "CARGO_BUILD_TARGET") {
        return format!("configured:{target}");
    }
    let target_environment = if cfg!(target_env = "msvc") {
        "msvc"
    } else if cfg!(target_env = "musl") {
        "musl"
    } else if cfg!(target_env = "gnu") {
        "gnu"
    } else {
        "unknown"
    };
    format!(
        "host:{}:{}:{}:{target_environment}",
        std::env::consts::ARCH,
        std::env::consts::OS,
        std::env::consts::FAMILY,
    )
}

pub(crate) fn cargo_cache_environment(
    environment: &HashMap<String, String>,
) -> BTreeMap<String, String> {
    environment
        .iter()
        .filter(|(name, _)| cargo_environment_affects_artifacts(name))
        .map(|(name, value)| (normalized_environment_name(name), value.clone()))
        .collect()
}

pub(crate) fn cargo_validation_environment(
    mut environment: HashMap<String, String>,
) -> HashMap<String, String> {
    environment.retain(|name, _| !cargo_compiler_wrapper_environment_name(name));
    // Empty wrapper variables override Cargo config without selecting an executable.
    for name in CARGO_COMPILER_WRAPPER_ENVIRONMENT_NAMES {
        environment.insert(name.to_string(), String::new());
    }
    environment
}

fn cargo_environment_affects_artifacts(name: &str) -> bool {
    if cargo_compiler_wrapper_environment_name(name) {
        return false;
    }
    let name = normalized_environment_name(name);
    if name == "RUST_LOG"
        || name == "RUST_BACKTRACE"
        || name == "CARGO_LOG"
        || name.starts_with("CARGO_TERM_")
    {
        return false;
    }
    if name.starts_with("CARGO_")
        || matches!(
            name.as_str(),
            "RUSTC"
                | "RUSTC_BOOTSTRAP"
                | "RUSTDOC"
                | "RUSTFLAGS"
                | "RUSTDOCFLAGS"
                | "RUSTUP_TOOLCHAIN"
                | "SDKROOT"
                | "DEVELOPER_DIR"
                | "MACOSX_DEPLOYMENT_TARGET"
                | "IPHONEOS_DEPLOYMENT_TARGET"
                | "CPATH"
                | "LIBRARY_PATH"
                | "LD_LIBRARY_PATH"
                | "DYLD_LIBRARY_PATH"
                | "CMAKE"
                | "CMAKE_PREFIX_PATH"
                | "CMAKE_TOOLCHAIN_FILE"
                | "CMAKE_GENERATOR"
                | "PKG_CONFIG"
                | "BINDGEN_EXTRA_CLANG_ARGS"
                | "LIBCLANG_PATH"
                | "CLANG_PATH"
                | "VCPKG_ROOT"
                | "PROTOC"
                | "PROTOC_INCLUDE"
        )
        || name.starts_with("PKG_CONFIG_")
        || name.starts_with("OPENSSL_")
    {
        return true;
    }
    [
        "AR",
        "CC",
        "CFLAGS",
        "CPP",
        "CPPFLAGS",
        "CXX",
        "CXXFLAGS",
        "LD",
        "LDFLAGS",
        "NM",
        "OBJC",
        "OBJCFLAGS",
        "RANLIB",
    ]
    .into_iter()
    .any(|base| {
        name == base
            || name
                .strip_prefix(base)
                .is_some_and(|rest| rest.starts_with('_'))
            || ["TARGET_", "HOST_"]
                .into_iter()
                .filter_map(|prefix| name.strip_prefix(prefix))
                .any(|rest| rest == base || rest.starts_with(&format!("{base}_")))
    })
}

fn cargo_compiler_wrapper_environment_name(name: &str) -> bool {
    CARGO_COMPILER_WRAPPER_ENVIRONMENT_NAMES
        .into_iter()
        .any(|expected| environment_name_eq(name, expected))
}

pub(crate) fn environment_value<'a>(
    environment: &'a HashMap<String, String>,
    expected: &str,
) -> Option<&'a str> {
    environment
        .iter()
        .find(|(name, _)| environment_name_eq(name, expected))
        .map(|(_, value)| value.as_str())
}

fn environment_name_eq(name: &str, expected: &str) -> bool {
    if cfg!(windows) {
        name.eq_ignore_ascii_case(expected)
    } else {
        name == expected
    }
}

fn normalized_environment_name(name: &str) -> String {
    if cfg!(windows) {
        name.to_ascii_uppercase()
    } else {
        name.to_string()
    }
}

fn append_key_part(material: &mut Vec<u8>, part: &[u8]) {
    material.extend_from_slice(&part.len().to_le_bytes());
    material.extend_from_slice(part);
}

fn hex_digest(digest: [u8; 20]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(40);
    for byte in digest {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}
