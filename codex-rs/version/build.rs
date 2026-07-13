use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const UNAVAILABLE: &str = "unavailable";

fn main() {
    let version = env::var("CODE_VERSION")
        .ok()
        .and_then(|value| normalized_value(&value))
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let build_profile = env::var("PROFILE")
        .ok()
        .and_then(|value| normalized_value(&value))
        .unwrap_or_else(|| UNAVAILABLE.to_string());
    let source_metadata = explicit_source_metadata().or_else(|| {
        repo_root_from_manifest()
            .and_then(|repo_root| git_source_metadata(repo_root.as_path(), &build_profile))
    });
    let source_metadata = source_metadata.unwrap_or_else(SourceMetadata::unavailable);

    for (name, value) in [
        ("CODE_VERSION", version),
        ("CODEX_BUILD_SOURCE_COMMIT", source_metadata.source_commit),
        ("CODEX_BUILD_DIRTY_STATE", source_metadata.dirty_state),
        ("CODEX_BUILD_PROFILE", build_profile),
        ("CODEX_BUILD_CHANNEL", source_metadata.build_channel),
    ] {
        println!("cargo:rustc-env={name}={value}");
    }

    for name in [
        "CODE_VERSION",
        "CODEX_BUILD_COMMIT",
        "CODEX_BUILD_DIRTY",
        "CODEX_BUILD_CHANNEL",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    if let Some(repo_root) = repo_root_from_manifest() {
        emit_git_rerun_paths(repo_root.as_path());
    }
}

#[derive(Debug)]
struct SourceMetadata {
    source_commit: String,
    dirty_state: String,
    build_channel: String,
}

impl SourceMetadata {
    fn unavailable() -> Self {
        Self {
            source_commit: UNAVAILABLE.to_string(),
            dirty_state: UNAVAILABLE.to_string(),
            build_channel: UNAVAILABLE.to_string(),
        }
    }
}

fn explicit_source_metadata() -> Option<SourceMetadata> {
    let commit = env::var("CODEX_BUILD_COMMIT").ok();
    let dirty = env::var("CODEX_BUILD_DIRTY").ok();
    let channel = env::var("CODEX_BUILD_CHANNEL").ok();
    match (commit, dirty, channel) {
        (Some(commit), Some(dirty), Some(channel)) => Some(
            source_metadata(&commit, &dirty, &channel).unwrap_or_else(SourceMetadata::unavailable),
        ),
        (None, None, None) => None,
        _ => Some(SourceMetadata::unavailable()),
    }
}

fn git_source_metadata(repo_root: &Path, build_profile: &str) -> Option<SourceMetadata> {
    if !git_toplevel_matches(repo_root) {
        return None;
    }

    let dirty_state = git_dirty_state(repo_root)?;
    git_output(repo_root, ["rev-parse", "--verify", "HEAD"]).and_then(|source_commit| {
        source_metadata(&source_commit, &dirty_state, default_channel(build_profile))
    })
}

fn source_metadata(
    source_commit: &str,
    dirty_state: &str,
    build_channel: &str,
) -> Option<SourceMetadata> {
    Some(SourceMetadata {
        source_commit: normalized_commit(source_commit)?,
        dirty_state: normalized_dirty_state(dirty_state)?,
        build_channel: normalized_value(build_channel)?,
    })
}

fn git_toplevel_matches(repo_root: &Path) -> bool {
    let Some(git_root) = git_output(repo_root, ["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    let Ok(expected) = fs::canonicalize(repo_root) else {
        return false;
    };
    let Ok(actual) = fs::canonicalize(git_root) else {
        return false;
    };

    actual == expected
}

fn repo_root_from_manifest() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR")?);
    let root = manifest_dir.parent()?.parent()?.to_path_buf();
    root.join("codex-rs")
        .join("version")
        .eq(&manifest_dir)
        .then_some(root)
}

fn git_dirty_state(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    if output.stdout.is_empty() {
        Some("clean".to_string())
    } else {
        Some("dirty".to_string())
    }
}

fn git_output<const N: usize>(repo_root: &Path, args: [&str; N]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn emit_git_rerun_paths(repo_root: &Path) {
    let Some(git_dir) = git_output(repo_root, ["rev-parse", "--git-dir"]) else {
        return;
    };
    let git_dir = PathBuf::from(git_dir);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        repo_root.join(git_dir)
    };

    let head_path = git_dir.join("HEAD");
    for path in [
        Some(head_path.clone()),
        Some(git_dir.join("index")),
        Some(git_dir.join("packed-refs")),
        head_ref(&head_path).map(|git_ref| git_dir.join(git_ref)),
    ]
    .into_iter()
    .flatten()
    {
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn head_ref(head_path: &Path) -> Option<String> {
    let contents = fs::read_to_string(head_path).ok()?;
    contents
        .trim()
        .strip_prefix("ref:")
        .map(str::trim)
        .filter(|git_ref| git_ref.starts_with("refs/"))
        .map(ToString::to_string)
}

fn normalized_commit(value: &str) -> Option<String> {
    let value = value.trim();
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }

    Some(value.to_ascii_lowercase())
}

fn normalized_dirty_state(value: &str) -> Option<String> {
    match value.trim() {
        "clean" => Some("clean".to_string()),
        "dirty" => Some("dirty".to_string()),
        _ => None,
    }
}

fn normalized_value(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_graphic()))
        .then(|| value.to_string())
}

fn default_channel(build_profile: &str) -> &'static str {
    if build_profile == "release" {
        "release"
    } else {
        "dev"
    }
}
