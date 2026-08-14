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
    let lab_release_version = env::var("CODEX_LAB_RELEASE_VERSION")
        .ok()
        .and_then(|value| normalized_value(&value))
        .unwrap_or_else(|| version.clone());
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
        ("CODEX_LAB_RELEASE_VERSION", lab_release_version),
        ("CODEX_BUILD_SOURCE_COMMIT", source_metadata.source_commit),
        ("CODEX_BUILD_DIRTY_STATE", source_metadata.dirty_state),
        ("CODEX_BUILD_PROFILE", build_profile),
        ("CODEX_BUILD_CHANNEL", source_metadata.build_channel),
    ] {
        println!("cargo:rustc-env={name}={value}");
    }

    for name in [
        "CODE_VERSION",
        "CODEX_LAB_RELEASE_VERSION",
        "CODEX_BUILD_COMMIT",
        "CODEX_BUILD_DIRTY",
        "CODEX_BUILD_CHANNEL",
        "PROFILE",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    if let Some(repo_root) = repo_root_from_manifest() {
        for path in git_rerun_paths(repo_root.as_path()) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
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
    explicit_source_metadata_from_values(commit.as_deref(), dirty.as_deref(), channel.as_deref())
}

fn explicit_source_metadata_from_values(
    commit: Option<&str>,
    dirty: Option<&str>,
    channel: Option<&str>,
) -> Option<SourceMetadata> {
    match (commit, dirty, channel) {
        (Some(commit), Some(dirty), Some(channel)) => Some(
            source_metadata(commit, dirty, channel).unwrap_or_else(SourceMetadata::unavailable),
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
        .args(["status", "--porcelain", "--untracked-files=no"])
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

fn git_rerun_paths(repo_root: &Path) -> Vec<PathBuf> {
    let Some(head_path) = git_path(repo_root, "HEAD") else {
        return Vec::new();
    };
    let mut paths = vec![head_path.clone()];
    paths.extend(git_path(repo_root, "index"));
    paths.extend(git_path(repo_root, "packed-refs"));
    paths.extend(head_ref(&head_path).and_then(|git_ref| git_path(repo_root, git_ref.as_str())));
    if let Some(tracked_files) = git_output(repo_root, ["ls-files"]) {
        paths.extend(
            tracked_files
                .lines()
                .filter(|path| !path.is_empty())
                .map(|path| repo_root.join(path)),
        );
    }
    paths
}

fn git_path(repo_root: &Path, path: &str) -> Option<PathBuf> {
    let path = PathBuf::from(git_output(repo_root, ["rev-parse", "--git-path", path])?);
    Some(if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn explicit_metadata_is_an_atomic_tuple() {
        let complete = explicit_source_metadata_from_values(
            Some("0123456789abcdef0123456789abcdef01234567"),
            Some("clean"),
            Some("dev"),
        )
        .expect("explicit tuple");
        assert_eq!(complete.dirty_state, "clean");

        let partial = explicit_source_metadata_from_values(
            Some("0123456789abcdef0123456789abcdef01234567"),
            /*dirty*/ None,
            Some("dev"),
        )
        .expect("partial tuple must suppress git fallback");
        assert_eq!(partial.source_commit, UNAVAILABLE);
    }

    #[test]
    fn tracked_changes_are_dirty_and_watched() {
        let repo = TestRepo::new();
        repo.write("tracked.txt", "clean\n");
        repo.git(["add", "tracked.txt"]);
        repo.commit();

        assert_eq!(git_dirty_state(repo.path()), Some("clean".to_string()));
        assert!(git_rerun_paths(repo.path()).contains(&repo.path().join("tracked.txt")));

        repo.write("tracked.txt", "dirty\n");
        assert_eq!(git_dirty_state(repo.path()), Some("dirty".to_string()));
    }

    #[test]
    fn linked_worktree_watches_common_branch_ref() {
        let repo = TestRepo::new();
        repo.write("tracked.txt", "clean\n");
        repo.git(["add", "tracked.txt"]);
        repo.commit();
        let worktree = repo.path().with_extension("linked");
        repo.git([
            "worktree",
            "add",
            "-b",
            "linked-test",
            worktree.to_str().expect("UTF-8 test path"),
        ]);

        let branch_ref = git_path(worktree.as_path(), "refs/heads/linked-test")
            .expect("linked worktree branch ref");
        assert!(git_rerun_paths(worktree.as_path()).contains(&branch_ref));

        repo.git([
            "worktree",
            "remove",
            "--force",
            worktree.to_str().expect("UTF-8 test path"),
        ]);
    }

    struct TestRepo {
        path: PathBuf,
    }

    impl TestRepo {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "codex-build-provenance-{}-{id}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create test repo");
            let repo = Self { path };
            repo.git(["init", "--quiet"]);
            repo
        }

        fn path(&self) -> &Path {
            self.path.as_path()
        }

        fn write(&self, relative_path: &str, contents: &str) {
            fs::write(self.path.join(relative_path), contents).expect("write test file");
        }

        fn commit(&self) {
            self.git([
                "-c",
                "user.name=Codex Test",
                "-c",
                "user.email=codex@example.com",
                "commit",
                "--quiet",
                "-m",
                "initial",
            ]);
        }

        fn git<const N: usize>(&self, args: [&str; N]) {
            let status = Command::new("git")
                .arg("-C")
                .arg(&self.path)
                .args(args)
                .status()
                .expect("run git");
            assert!(status.success());
        }
    }

    impl Drop for TestRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
