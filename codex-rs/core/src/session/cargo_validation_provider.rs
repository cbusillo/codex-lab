use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use codex_config::CargoValidationProviderConfig;
use codex_config::MAX_VALIDATION_PROVIDER_TIMEOUT_MS;
use codex_protocol::exec_output::ExecToolCallOutput;
use codex_protocol::protocol::ProjectValidationStatus;
use codex_utils_absolute_path::AbsolutePathBuf;
use globset::GlobBuilder;
use serde::Deserialize;

use super::validation_provider::AutomaticValidationCommand;
use super::validation_provider::AutomaticValidationProviderError;
use super::validation_provider::AutomaticValidationProviderErrorKind;
use super::validation_provider::AutomaticValidationProviderKind;
use super::validation_provider::configuration_error;
use super::validation_provider::infrastructure_error;

const CARGO_MAX_CHANGED_FILES: usize = 128;
const CARGO_MAX_DISCOVERY_DEPTH: usize = 32;
const CARGO_MAX_DISCOVERY_DIRECTORIES: usize = 256;
const CARGO_MAX_DISCOVERY_ENTRIES: usize = 1_024;
const CARGO_MAX_CONFIG_BYTES: u64 = 512 * 1024;
const CARGO_MAX_MANIFEST_BYTES: u64 = 512 * 1024;
const CARGO_MAX_TOOLCHAIN_BYTES: u64 = 64 * 1024;
const CARGO_MAX_DIAGNOSTICS: usize = 20;
const CARGO_MAX_DIAGNOSTIC_BYTES: usize = 6 * 1024;
const CARGO_COMMAND_MAX_BYTES: usize = 8 * 1024;
const CARGO_JOBS: &str = "2";
const CARGO_DIAGNOSTICS_TRUNCATED_MARKER: &str = "… cargo diagnostics truncated …";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CargoChangedFileKind {
    Source,
    Manifest,
    Lockfile,
    Config,
}

#[derive(Clone, Debug)]
struct CargoChangedFile {
    path: PathBuf,
    kind: CargoChangedFileKind,
}

#[derive(Clone, Debug)]
struct CargoManifestSummary {
    has_package: bool,
    has_workspace: bool,
    package_workspace: Option<String>,
    workspace_excludes: Vec<String>,
    workspace_members: Vec<String>,
}

#[derive(Clone, Debug)]
struct CargoImpact {
    changed: CargoChangedFile,
    manifest: PathBuf,
    manifest_summary: CargoManifestSummary,
    workspace_manifest: Option<PathBuf>,
}

#[derive(Clone, Debug)]
enum CargoTarget {
    Package {
        manifest: PathBuf,
        all_targets: bool,
        include_tests: bool,
        include_benches: bool,
        include_examples: bool,
    },
    Workspace {
        manifest: PathBuf,
    },
}

impl CargoTarget {
    fn manifest(&self) -> &Path {
        match self {
            Self::Package { manifest, .. } | Self::Workspace { manifest } => manifest,
        }
    }
}

#[derive(Clone, Debug)]
struct CargoToolchainFile {
    name: &'static str,
    contents: String,
}

#[derive(Debug, Deserialize)]
struct CargoJsonMessage {
    reason: String,
    message: Option<RustcDiagnostic>,
}

#[derive(Debug, Deserialize)]
struct RustcDiagnostic {
    level: String,
    message: String,
    code: Option<RustcDiagnosticCode>,
    #[serde(default)]
    spans: Vec<RustcDiagnosticSpan>,
}

#[derive(Debug, Deserialize)]
struct RustcDiagnosticCode {
    code: String,
}

#[derive(Debug, Deserialize)]
struct RustcDiagnosticSpan {
    file_name: String,
    line_start: u64,
    column_start: u64,
    is_primary: bool,
}

pub(super) struct CargoValidationOutput {
    pub(super) status: ProjectValidationStatus,
    pub(super) text: String,
}

pub(super) async fn resolve_cargo_validation_command(
    config: &CargoValidationProviderConfig,
    repo_root: &AbsolutePathBuf,
    changed_files: &[PathBuf],
    changed_file_count: u32,
) -> Result<Option<AutomaticValidationCommand>, AutomaticValidationProviderError> {
    let relevant_files = cargo_changed_files(changed_files)?;
    if relevant_files.is_empty() {
        return Ok(None);
    }
    if relevant_files.len() > CARGO_MAX_CHANGED_FILES {
        return Err(infrastructure_error(
            config.command.clone(),
            repo_root.clone(),
            format!("cargo validation matched more than {CARGO_MAX_CHANGED_FILES} changed files"),
        ));
    }

    let mut manifest_cache = HashMap::new();
    let mut impacts = Vec::new();
    for changed in relevant_files {
        validate_changed_cargo_config(config, repo_root, &changed).await?;
        let Some(manifest) = find_nearest_manifest(config, repo_root, &changed).await? else {
            continue;
        };
        let manifest_summary =
            load_manifest_summary(config, repo_root, &manifest, &mut manifest_cache).await?;
        let workspace_manifest =
            find_nearest_workspace_manifest(config, repo_root, &manifest, &mut manifest_cache)
                .await?;
        impacts.push(CargoImpact {
            changed,
            manifest,
            manifest_summary,
            workspace_manifest,
        });
    }
    if impacts.is_empty() {
        return Ok(None);
    }

    let target = select_cargo_target(config, repo_root, &impacts, &manifest_cache)?;
    let toolchain_file = find_nearest_rust_toolchain(config, repo_root, target.manifest()).await?;
    build_cargo_command(config, target, toolchain_file, changed_file_count).map(Some)
}

async fn validate_changed_cargo_config(
    config: &CargoValidationProviderConfig,
    repo_root: &AbsolutePathBuf,
    changed: &CargoChangedFile,
) -> Result<(), AutomaticValidationProviderError> {
    if changed.kind != CargoChangedFileKind::Config {
        return Ok(());
    }
    let path = repo_root.as_ref().join(&changed.path);
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(infrastructure_error(
                config.command.clone(),
                repo_root.clone(),
                format!("failed to read cargo config metadata: {error}"),
            ));
        }
    };
    if metadata.len() > CARGO_MAX_CONFIG_BYTES {
        return Err(configuration_error(
            config.command.clone(),
            repo_root.clone(),
            format!(
                "cargo config {} exceeds the {CARGO_MAX_CONFIG_BYTES}-byte discovery limit",
                path.display()
            ),
        ));
    }
    let contents = tokio::fs::read_to_string(&path).await.map_err(|error| {
        configuration_error(
            config.command.clone(),
            repo_root.clone(),
            format!("failed to read cargo config {}: {error}", path.display()),
        )
    })?;
    toml::from_str::<toml::Value>(&contents).map_err(|error| {
        configuration_error(
            config.command.clone(),
            repo_root.clone(),
            format!("failed to parse cargo config {}: {error}", path.display()),
        )
    })?;
    Ok(())
}

async fn find_nearest_rust_toolchain(
    config: &CargoValidationProviderConfig,
    repo_root: &AbsolutePathBuf,
    manifest: &Path,
) -> Result<Option<CargoToolchainFile>, AutomaticValidationProviderError> {
    let mut directory = manifest.parent().map(Path::to_path_buf).ok_or_else(|| {
        infrastructure_error(
            config.command.clone(),
            repo_root.clone(),
            "cargo manifest has no parent directory for rust toolchain discovery",
        )
    })?;

    for _ in 0..=CARGO_MAX_DISCOVERY_DEPTH {
        if !directory.starts_with(repo_root.as_ref()) {
            return Ok(None);
        }
        for name in ["rust-toolchain", "rust-toolchain.toml"] {
            let path = directory.join(name);
            let metadata = match tokio::fs::metadata(&path).await {
                Ok(metadata) if metadata.is_file() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(infrastructure_error(
                        config.command.clone(),
                        repo_root.clone(),
                        format!("failed to read rust toolchain metadata: {error}"),
                    ));
                }
            };
            if metadata.len() > CARGO_MAX_TOOLCHAIN_BYTES {
                return Err(configuration_error(
                    config.command.clone(),
                    repo_root.clone(),
                    format!(
                        "rust toolchain file {} exceeds the {CARGO_MAX_TOOLCHAIN_BYTES}-byte discovery limit",
                        path.display()
                    ),
                ));
            }
            let contents = tokio::fs::read_to_string(&path).await.map_err(|error| {
                configuration_error(
                    config.command.clone(),
                    repo_root.clone(),
                    format!(
                        "failed to read rust toolchain file {}: {error}",
                        path.display()
                    ),
                )
            })?;
            let document = match toml::from_str::<toml::Value>(&contents) {
                Ok(document) => Some(document),
                Err(error) if name.ends_with(".toml") => {
                    return Err(configuration_error(
                        config.command.clone(),
                        repo_root.clone(),
                        format!(
                            "failed to parse rust toolchain file {}: {error}",
                            path.display()
                        ),
                    ));
                }
                Err(_) => None,
            };
            if let Some(document) = document {
                if document
                    .get("toolchain")
                    .and_then(toml::Value::as_table)
                    .is_some_and(|toolchain| toolchain.contains_key("path"))
                {
                    return Err(configuration_error(
                        config.command.clone(),
                        repo_root.clone(),
                        "cargo validation does not allow repository rust toolchain path overrides",
                    ));
                }
            }
            return Ok(Some(CargoToolchainFile { name, contents }));
        }
        if directory == repo_root.as_ref() || !directory.pop() {
            return Ok(None);
        }
    }
    Ok(None)
}

pub(super) fn classify_cargo_output(output: &ExecToolCallOutput) -> CargoValidationOutput {
    if output.exit_code == 0 {
        return CargoValidationOutput {
            status: ProjectValidationStatus::Passed,
            text: "cargo check passed".to_string(),
        };
    }

    if let Some(text) = render_cargo_diagnostics(&output.stdout.text) {
        return CargoValidationOutput {
            status: ProjectValidationStatus::ActionableFailure,
            text,
        };
    }

    let text = cargo_fallback_output(output);
    let status = if looks_like_cargo_configuration_error(&text) {
        ProjectValidationStatus::ConfigurationError
    } else if looks_like_unstructured_compiler_failure(&text) {
        ProjectValidationStatus::ActionableFailure
    } else {
        ProjectValidationStatus::InfrastructureFailure
    };
    CargoValidationOutput { status, text }
}

pub(super) fn render_cargo_output(output: &ExecToolCallOutput) -> String {
    render_cargo_diagnostics(&output.stdout.text).unwrap_or_else(|| cargo_fallback_output(output))
}

fn cargo_changed_files(
    changed_files: &[PathBuf],
) -> Result<Vec<CargoChangedFile>, AutomaticValidationProviderError> {
    let mut relevant = Vec::new();
    for path in changed_files {
        if !safe_relative_path(path) {
            return Err(AutomaticValidationProviderError {
                kind: AutomaticValidationProviderErrorKind::Infrastructure,
                command: Vec::new(),
                cwd: None,
                message: "cargo validation requires repository-relative changed-file paths"
                    .to_string(),
            });
        }
        if let Some(kind) = cargo_changed_file_kind(path) {
            relevant.push(CargoChangedFile {
                path: path.clone(),
                kind,
            });
        }
    }
    relevant.sort_by(|left, right| left.path.cmp(&right.path));
    relevant.dedup_by(|left, right| left.path == right.path);
    Ok(relevant)
}

fn safe_relative_path(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn cargo_changed_file_kind(path: &Path) -> Option<CargoChangedFileKind> {
    if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
        return Some(CargoChangedFileKind::Source);
    }
    match path.file_name().and_then(|name| name.to_str()) {
        Some("Cargo.toml") => Some(CargoChangedFileKind::Manifest),
        Some("Cargo.lock") => Some(CargoChangedFileKind::Lockfile),
        Some("config" | "config.toml")
            if path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some(".cargo") =>
        {
            Some(CargoChangedFileKind::Config)
        }
        _ => None,
    }
}

async fn find_nearest_manifest(
    config: &CargoValidationProviderConfig,
    repo_root: &AbsolutePathBuf,
    changed: &CargoChangedFile,
) -> Result<Option<PathBuf>, AutomaticValidationProviderError> {
    let absolute = repo_root.as_ref().join(&changed.path);
    let mut directory = match changed.kind {
        CargoChangedFileKind::Config => absolute
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf),
        _ => absolute.parent().map(Path::to_path_buf),
    }
    .ok_or_else(|| {
        infrastructure_error(
            config.command.clone(),
            repo_root.clone(),
            "cargo changed file has no discoverable parent directory",
        )
    })?;
    let config_scope = (changed.kind == CargoChangedFileKind::Config).then(|| directory.clone());

    for _ in 0..=CARGO_MAX_DISCOVERY_DEPTH {
        if !directory.starts_with(repo_root.as_ref()) {
            return Ok(None);
        }
        let candidate = directory.join("Cargo.toml");
        if tokio::fs::metadata(&candidate)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            return Ok(Some(candidate));
        }
        if directory == repo_root.as_ref() || !directory.pop() {
            break;
        }
    }

    let Some(config_scope) = config_scope else {
        return Ok(None);
    };
    find_nearest_descendant_manifest(config, repo_root, &config_scope).await
}

async fn find_nearest_descendant_manifest(
    config: &CargoValidationProviderConfig,
    repo_root: &AbsolutePathBuf,
    scope: &Path,
) -> Result<Option<PathBuf>, AutomaticValidationProviderError> {
    let mut directories = VecDeque::from([(scope.to_path_buf(), 0usize)]);
    let mut scanned_directories = 0usize;
    let mut scanned_entries = 0usize;
    let mut candidate_depth = None;
    let mut candidates = BTreeSet::new();

    while let Some((directory, depth)) = directories.pop_front() {
        if candidate_depth.is_some_and(|candidate_depth| depth > candidate_depth) {
            break;
        }
        scanned_directories = scanned_directories.saturating_add(1);
        if scanned_directories > CARGO_MAX_DISCOVERY_DIRECTORIES {
            return Err(infrastructure_error(
                config.command.clone(),
                repo_root.clone(),
                format!(
                    "cargo config discovery exceeded {CARGO_MAX_DISCOVERY_DIRECTORIES} directories"
                ),
            ));
        }

        let candidate = directory.join("Cargo.toml");
        if tokio::fs::metadata(&candidate)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            candidate_depth = Some(depth);
            candidates.insert(candidate);
            continue;
        }
        if depth == CARGO_MAX_DISCOVERY_DEPTH {
            continue;
        }

        let mut entries = match tokio::fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(infrastructure_error(
                    config.command.clone(),
                    repo_root.clone(),
                    format!(
                        "failed to inspect cargo config scope {}: {error}",
                        directory.display()
                    ),
                ));
            }
        };
        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            infrastructure_error(
                config.command.clone(),
                repo_root.clone(),
                format!(
                    "failed to enumerate cargo config scope {}: {error}",
                    directory.display()
                ),
            )
        })? {
            scanned_entries = scanned_entries.saturating_add(1);
            if scanned_entries > CARGO_MAX_DISCOVERY_ENTRIES {
                return Err(infrastructure_error(
                    config.command.clone(),
                    repo_root.clone(),
                    format!(
                        "cargo config discovery exceeded {CARGO_MAX_DISCOVERY_ENTRIES} directory entries"
                    ),
                ));
            }
            let file_type = entry.file_type().await.map_err(|error| {
                infrastructure_error(
                    config.command.clone(),
                    repo_root.clone(),
                    format!(
                        "failed to inspect cargo config entry {}: {error}",
                        entry.path().display()
                    ),
                )
            })?;
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some(".git" | ".code" | "node_modules" | "target")
            ) {
                continue;
            }
            directories.push_back((entry.path(), depth + 1));
        }
    }

    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.into_iter().next()),
        _ => Err(infrastructure_error(
            config.command.clone(),
            repo_root.clone(),
            "cargo config applies to more than one nearest package or workspace",
        )),
    }
}

async fn find_nearest_workspace_manifest(
    config: &CargoValidationProviderConfig,
    repo_root: &AbsolutePathBuf,
    manifest: &Path,
    manifest_cache: &mut HashMap<PathBuf, CargoManifestSummary>,
) -> Result<Option<PathBuf>, AutomaticValidationProviderError> {
    let mut directory = manifest.parent().map(Path::to_path_buf).ok_or_else(|| {
        infrastructure_error(
            config.command.clone(),
            repo_root.clone(),
            "cargo manifest has no parent directory",
        )
    })?;

    for _ in 0..=CARGO_MAX_DISCOVERY_DEPTH {
        if !directory.starts_with(repo_root.as_ref()) {
            return Ok(None);
        }
        let candidate = directory.join("Cargo.toml");
        if tokio::fs::metadata(&candidate)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            let summary =
                load_manifest_summary(config, repo_root, &candidate, manifest_cache).await?;
            if summary.has_workspace {
                return Ok(Some(candidate));
            }
        }
        if directory == repo_root.as_ref() || !directory.pop() {
            return Ok(None);
        }
    }
    Ok(None)
}

async fn load_manifest_summary(
    config: &CargoValidationProviderConfig,
    repo_root: &AbsolutePathBuf,
    manifest: &Path,
    manifest_cache: &mut HashMap<PathBuf, CargoManifestSummary>,
) -> Result<CargoManifestSummary, AutomaticValidationProviderError> {
    if let Some(summary) = manifest_cache.get(manifest) {
        return Ok(summary.clone());
    }
    let metadata = tokio::fs::metadata(manifest).await.map_err(|error| {
        infrastructure_error(
            config.command.clone(),
            repo_root.clone(),
            format!("failed to read cargo manifest metadata: {error}"),
        )
    })?;
    if metadata.len() > CARGO_MAX_MANIFEST_BYTES {
        return Err(configuration_error(
            config.command.clone(),
            repo_root.clone(),
            format!(
                "cargo manifest {} exceeds the {CARGO_MAX_MANIFEST_BYTES}-byte discovery limit",
                manifest.display()
            ),
        ));
    }
    let contents = tokio::fs::read_to_string(manifest).await.map_err(|error| {
        configuration_error(
            config.command.clone(),
            repo_root.clone(),
            format!(
                "failed to read cargo manifest {}: {error}",
                manifest.display()
            ),
        )
    })?;
    let document = toml::from_str::<toml::Value>(&contents).map_err(|error| {
        configuration_error(
            config.command.clone(),
            repo_root.clone(),
            format!(
                "failed to parse cargo manifest {}: {error}",
                manifest.display()
            ),
        )
    })?;
    let package = document.get("package").and_then(toml::Value::as_table);
    let workspace = document.get("workspace").and_then(toml::Value::as_table);
    let package_workspace = match package.and_then(|package| package.get("workspace")) {
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| {
                    configuration_error(
                        config.command.clone(),
                        repo_root.clone(),
                        format!(
                            "cargo manifest {} package.workspace must be a string",
                            manifest.display()
                        ),
                    )
                })?
                .to_string(),
        ),
        None => None,
    };
    let workspace_members = manifest_string_array(workspace, "members").map_err(|message| {
        configuration_error(
            config.command.clone(),
            repo_root.clone(),
            format!("cargo manifest {} {message}", manifest.display()),
        )
    })?;
    let workspace_excludes = manifest_string_array(workspace, "exclude").map_err(|message| {
        configuration_error(
            config.command.clone(),
            repo_root.clone(),
            format!("cargo manifest {} {message}", manifest.display()),
        )
    })?;
    let summary = CargoManifestSummary {
        has_package: package.is_some(),
        has_workspace: workspace.is_some(),
        package_workspace,
        workspace_excludes,
        workspace_members,
    };
    if !summary.has_package && !summary.has_workspace {
        return Err(configuration_error(
            config.command.clone(),
            repo_root.clone(),
            format!(
                "cargo manifest {} has neither a package nor a workspace",
                manifest.display()
            ),
        ));
    }
    manifest_cache.insert(manifest.to_path_buf(), summary.clone());
    Ok(summary)
}

fn manifest_string_array(
    table: Option<&toml::value::Table>,
    key: &str,
) -> Result<Vec<String>, String> {
    let Some(value) = table.and_then(|table| table.get(key)) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(format!("workspace.{key} must be an array of strings"));
    };
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("workspace.{key} must contain only strings"))
        })
        .collect()
}

fn select_cargo_target(
    config: &CargoValidationProviderConfig,
    repo_root: &AbsolutePathBuf,
    impacts: &[CargoImpact],
    manifest_cache: &HashMap<PathBuf, CargoManifestSummary>,
) -> Result<CargoTarget, AutomaticValidationProviderError> {
    let manifests = impacts
        .iter()
        .map(|impact| impact.manifest.clone())
        .collect::<BTreeSet<_>>();
    if manifests.len() == 1 {
        let impact = &impacts[0];
        let workspace_wide = impact.manifest_summary.has_workspace
            && impacts.iter().any(|impact| {
                matches!(
                    impact.changed.kind,
                    CargoChangedFileKind::Manifest
                        | CargoChangedFileKind::Lockfile
                        | CargoChangedFileKind::Config
                )
            });
        if impact.manifest_summary.has_package && !workspace_wide {
            let manifest_root = impact.manifest.parent().ok_or_else(|| {
                infrastructure_error(
                    config.command.clone(),
                    repo_root.clone(),
                    "cargo package manifest has no parent directory",
                )
            })?;
            let mut all_targets = false;
            let mut include_tests = false;
            let mut include_benches = false;
            let mut include_examples = false;
            for impact in impacts {
                all_targets |= impact.changed.kind == CargoChangedFileKind::Manifest;
                let path = repo_root.as_ref().join(&impact.changed.path);
                let relative = path.strip_prefix(manifest_root).unwrap_or(path.as_path());
                include_tests |= first_component_is(relative, "tests");
                include_benches |= first_component_is(relative, "benches");
                include_examples |= first_component_is(relative, "examples");
            }
            return Ok(CargoTarget::Package {
                manifest: impact.manifest.clone(),
                all_targets,
                include_tests,
                include_benches,
                include_examples,
            });
        }
        if impact.manifest_summary.has_workspace {
            return Ok(CargoTarget::Workspace {
                manifest: impact.manifest.clone(),
            });
        }
    }

    let workspace_manifests = impacts
        .iter()
        .filter_map(|impact| impact.workspace_manifest.clone())
        .collect::<BTreeSet<_>>();
    if workspace_manifests.len() == 1
        && impacts
            .iter()
            .all(|impact| impact.workspace_manifest.is_some())
    {
        let workspace_manifest = workspace_manifests
            .into_iter()
            .next()
            .expect("one workspace manifest should be present");
        let Some(workspace_summary) = manifest_cache.get(&workspace_manifest) else {
            return Err(infrastructure_error(
                config.command.clone(),
                repo_root.clone(),
                "cargo workspace manifest summary was unavailable",
            ));
        };
        for impact in impacts {
            if !cargo_workspace_contains_manifest(
                config,
                repo_root,
                &workspace_manifest,
                workspace_summary,
                &impact.manifest,
                &impact.manifest_summary,
            )? {
                return Err(infrastructure_error(
                    config.command.clone(),
                    repo_root.clone(),
                    format!(
                        "cargo validation could not prove {} is a member of workspace {}",
                        impact.manifest.display(),
                        workspace_manifest.display()
                    ),
                ));
            }
        }
        return Ok(CargoTarget::Workspace {
            manifest: workspace_manifest,
        });
    }

    Err(infrastructure_error(
        config.command.clone(),
        repo_root.clone(),
        "cargo validation changes span more than one independent package or workspace",
    ))
}

fn cargo_workspace_contains_manifest(
    config: &CargoValidationProviderConfig,
    repo_root: &AbsolutePathBuf,
    workspace_manifest: &Path,
    workspace_summary: &CargoManifestSummary,
    package_manifest: &Path,
    package_summary: &CargoManifestSummary,
) -> Result<bool, AutomaticValidationProviderError> {
    if package_manifest == workspace_manifest {
        return Ok(true);
    }
    if !package_summary.has_package {
        return Ok(false);
    }
    let workspace_root = workspace_manifest.parent().ok_or_else(|| {
        infrastructure_error(
            config.command.clone(),
            repo_root.clone(),
            "cargo workspace manifest has no parent directory",
        )
    })?;
    let package_root = package_manifest.parent().ok_or_else(|| {
        infrastructure_error(
            config.command.clone(),
            repo_root.clone(),
            "cargo package manifest has no parent directory",
        )
    })?;

    if let Some(package_workspace) = package_summary.package_workspace.as_deref() {
        let declared_manifest = package_root.join(package_workspace).join("Cargo.toml");
        let declared_manifest =
            dunce::canonicalize(&declared_manifest).unwrap_or(declared_manifest);
        let workspace_manifest = dunce::canonicalize(workspace_manifest)
            .unwrap_or_else(|_| workspace_manifest.to_path_buf());
        if declared_manifest == workspace_manifest {
            return Ok(true);
        }
    }

    let Ok(relative) = package_root.strip_prefix(workspace_root) else {
        return Ok(false);
    };
    let mut relative_components = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => {
                let Some(value) = value.to_str() else {
                    return Err(infrastructure_error(
                        config.command.clone(),
                        repo_root.clone(),
                        "cargo workspace member path must be valid UTF-8",
                    ));
                };
                relative_components.push(value);
            }
            Component::CurDir => {}
            _ => return Ok(false),
        }
    }
    let relative = relative_components.join("/");
    if relative.is_empty() {
        return Ok(workspace_summary.has_package);
    }
    for pattern in &workspace_summary.workspace_excludes {
        if cargo_workspace_pattern_matches(config, repo_root, pattern, &relative)? {
            return Ok(false);
        }
    }
    for pattern in &workspace_summary.workspace_members {
        if cargo_workspace_pattern_matches(config, repo_root, pattern, &relative)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn cargo_workspace_pattern_matches(
    config: &CargoValidationProviderConfig,
    repo_root: &AbsolutePathBuf,
    pattern: &str,
    relative: &str,
) -> Result<bool, AutomaticValidationProviderError> {
    let pattern = pattern.trim_end_matches('/').replace('\\', "/");
    let matcher = GlobBuilder::new(&pattern)
        .literal_separator(true)
        .build()
        .map_err(|error| {
            configuration_error(
                config.command.clone(),
                repo_root.clone(),
                format!("cargo workspace member pattern {pattern:?} is invalid: {error}"),
            )
        })?
        .compile_matcher();
    Ok(matcher.is_match(relative))
}

fn first_component_is(path: &Path, expected: &str) -> bool {
    path.components().find_map(|component| match component {
        Component::Normal(value) => value.to_str(),
        _ => None,
    }) == Some(expected)
}

fn build_cargo_command(
    config: &CargoValidationProviderConfig,
    target: CargoTarget,
    toolchain_file: Option<CargoToolchainFile>,
    changed_file_count: u32,
) -> Result<AutomaticValidationCommand, AutomaticValidationProviderError> {
    let (manifest, workspace, all_targets, include_tests, include_benches, include_examples) =
        match target {
            CargoTarget::Package {
                manifest,
                all_targets,
                include_tests,
                include_benches,
                include_examples,
            } => (
                manifest,
                false,
                all_targets,
                include_tests,
                include_benches,
                include_examples,
            ),
            CargoTarget::Workspace { manifest } => (manifest, true, false, false, false, false),
        };
    let cwd = manifest.parent().map(Path::to_path_buf).ok_or_else(|| {
        AutomaticValidationProviderError {
            kind: AutomaticValidationProviderErrorKind::Infrastructure,
            command: config.command.clone(),
            cwd: None,
            message: "cargo validation manifest has no parent directory".to_string(),
        }
    })?;
    let cwd = AbsolutePathBuf::try_from(cwd).map_err(|error| AutomaticValidationProviderError {
        kind: AutomaticValidationProviderErrorKind::Infrastructure,
        command: config.command.clone(),
        cwd: None,
        message: format!("failed to resolve cargo validation directory: {error}"),
    })?;
    if config.command.len() != 1
        || config
            .command
            .first()
            .is_none_or(|program| program.trim().is_empty())
    {
        return Err(configuration_error(
            config.command.clone(),
            cwd,
            "validation.providers.cargo.command must contain exactly one non-empty executable",
        ));
    }
    if config.timeout_ms == 0 || config.timeout_ms > MAX_VALIDATION_PROVIDER_TIMEOUT_MS {
        return Err(configuration_error(
            config.command.clone(),
            cwd,
            format!(
                "validation.providers.cargo.timeout_ms must be between 1 and {MAX_VALIDATION_PROVIDER_TIMEOUT_MS}"
            ),
        ));
    }
    let execution_cwd_guard = tempfile::Builder::new()
        .prefix("codex-cargo-validation-")
        .tempdir()
        .map_err(|error| {
            infrastructure_error(
                config.command.clone(),
                cwd.clone(),
                format!("failed to create isolated cargo validation directory: {error}"),
            )
        })?;
    let execution_cwd = AbsolutePathBuf::try_from(execution_cwd_guard.path().to_path_buf())
        .map_err(|error| {
            infrastructure_error(
                config.command.clone(),
                cwd.clone(),
                format!("failed to resolve isolated cargo validation directory: {error}"),
            )
        })?;
    if let Some(toolchain_file) = toolchain_file {
        std::fs::write(
            execution_cwd_guard.path().join(toolchain_file.name),
            toolchain_file.contents,
        )
        .map_err(|error| {
            infrastructure_error(
                config.command.clone(),
                cwd.clone(),
                format!("failed to stage rust toolchain file for cargo validation: {error}"),
            )
        })?;
    }
    let Some(manifest) = manifest.to_str() else {
        return Err(infrastructure_error(
            config.command.clone(),
            cwd,
            "cargo validation manifest path must be valid UTF-8",
        ));
    };
    let target_dir = execution_cwd_guard.path().join("target");
    let Some(target_dir) = target_dir.to_str() else {
        return Err(infrastructure_error(
            config.command.clone(),
            cwd,
            "cargo validation target directory path must be valid UTF-8",
        ));
    };

    let mut command = config.command.clone();
    command.extend([
        "check".to_string(),
        "--quiet".to_string(),
        "--message-format=json".to_string(),
        "--color".to_string(),
        "never".to_string(),
        "--jobs".to_string(),
        CARGO_JOBS.to_string(),
        "--manifest-path".to_string(),
        manifest.to_string(),
        "--target-dir".to_string(),
        target_dir.to_string(),
        "--locked".to_string(),
    ]);
    if workspace {
        command.push("--workspace".to_string());
    } else if all_targets {
        command.push("--all-targets".to_string());
    } else {
        if include_tests {
            command.push("--tests".to_string());
        }
        if include_benches {
            command.push("--benches".to_string());
        }
        if include_examples {
            command.push("--examples".to_string());
        }
    }
    let command_bytes = command.iter().fold(0usize, |total, argument| {
        total.saturating_add(argument.len() + 1)
    });
    if command_bytes > CARGO_COMMAND_MAX_BYTES {
        return Err(configuration_error(
            command,
            cwd,
            format!("cargo validation command must not exceed {CARGO_COMMAND_MAX_BYTES} bytes"),
        ));
    }

    Ok(AutomaticValidationCommand {
        kind: AutomaticValidationProviderKind::Cargo,
        command,
        cwd,
        execution_cwd: Some(execution_cwd),
        execution_cwd_guard: Some(execution_cwd_guard),
        timeout_ms: config.timeout_ms,
        changed_file_count,
    })
}

fn render_cargo_diagnostics(stdout: &str) -> Option<String> {
    let content_budget =
        CARGO_MAX_DIAGNOSTIC_BYTES.saturating_sub(CARGO_DIAGNOSTICS_TRUNCATED_MARKER.len() + 1);
    let mut rendered = String::new();
    let mut seen = BTreeSet::new();
    let mut diagnostic_count = 0usize;
    let mut truncated = false;
    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<CargoJsonMessage>(line) else {
            continue;
        };
        if message.reason != "compiler-message" {
            continue;
        }
        let Some(diagnostic) = message.message else {
            continue;
        };
        if diagnostic.level != "error" {
            continue;
        }
        let (line, line_truncated) =
            truncate_utf8(&render_rustc_diagnostic(&diagnostic), content_budget);
        truncated |= line_truncated;
        if !seen.insert(line.clone()) {
            continue;
        }
        let separator_bytes = usize::from(!rendered.is_empty());
        if diagnostic_count >= CARGO_MAX_DIAGNOSTICS
            || rendered.len().saturating_add(line.len() + separator_bytes) > content_budget
        {
            truncated = true;
            continue;
        }
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str(&line);
        diagnostic_count += 1;
    }
    if diagnostic_count == 0 {
        return None;
    }
    if truncated {
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str(CARGO_DIAGNOSTICS_TRUNCATED_MARKER);
    }
    Some(rendered)
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

fn render_rustc_diagnostic(diagnostic: &RustcDiagnostic) -> String {
    let code = diagnostic
        .code
        .as_ref()
        .map(|code| format!("[{}]", code.code))
        .unwrap_or_default();
    let message = diagnostic
        .message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let span = diagnostic
        .spans
        .iter()
        .find(|span| span.is_primary)
        .or_else(|| diagnostic.spans.first());
    match span {
        Some(span) => format!(
            "{}:{}:{}: error{code}: {message}",
            span.file_name, span.line_start, span.column_start
        ),
        None => format!("error{code}: {message}"),
    }
}

fn cargo_fallback_output(output: &ExecToolCallOutput) -> String {
    if !output.stderr.text.trim().is_empty() {
        return output.stderr.text.clone();
    }
    if !output.aggregated_output.text.trim().is_empty() {
        return output.aggregated_output.text.clone();
    }
    if !output.stdout.text.trim().is_empty() {
        return output.stdout.text.clone();
    }
    "cargo check failed without compiler diagnostics".to_string()
}

fn looks_like_cargo_configuration_error(output: &str) -> bool {
    let output = output.to_ascii_lowercase();
    [
        "failed to parse manifest",
        "failed to load manifest",
        "manifest path",
        "workspace member",
        "the lock file",
        "package id specification",
        "no matching package named",
    ]
    .iter()
    .any(|needle| output.contains(needle))
}

fn looks_like_unstructured_compiler_failure(output: &str) -> bool {
    let output = output.to_ascii_lowercase();
    output.contains("error[") || output.contains("could not compile")
}

#[cfg(test)]
#[path = "cargo_validation_provider_tests.rs"]
mod tests;
