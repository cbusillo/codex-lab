use super::*;
use codex_protocol::exec_output::StreamOutput;
use tempfile::TempDir;
use tempfile::tempdir;

fn cargo_config(command: &str) -> CargoValidationProviderConfig {
    CargoValidationProviderConfig {
        command: vec![command.to_string()],
        timeout_ms: 25_000,
        ..CargoValidationProviderConfig::default()
    }
}

fn absolute_root(repo: &TempDir) -> AbsolutePathBuf {
    AbsolutePathBuf::try_from(repo.path().to_path_buf()).expect("absolute repository root")
}

fn write_file(repo: &TempDir, path: &str, contents: &str) {
    let path = repo.path().join(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture parent");
    }
    std::fs::write(path, contents).expect("write fixture");
}

async fn resolve(
    repo: &TempDir,
    paths: &[&str],
) -> Result<Option<AutomaticValidationCommand>, AutomaticValidationProviderError> {
    resolve_cargo_validation_command(
        &cargo_config("fake-cargo"),
        &absolute_root(repo),
        &paths.iter().map(PathBuf::from).collect::<Vec<_>>(),
        u32::try_from(paths.len()).expect("changed-file count should fit in u32"),
    )
    .await
}

fn compiler_message(path: &str, line: u64, message: &str, code: &str) -> String {
    serde_json::json!({
        "reason": "compiler-message",
        "message": {
            "level": "error",
            "message": message,
            "code": { "code": code },
            "spans": [{
                "file_name": path,
                "line_start": line,
                "column_start": 7,
                "is_primary": true
            }]
        }
    })
    .to_string()
}

#[test]
fn cargo_changed_file_classification_is_bounded_to_rust_inputs() {
    assert_eq!(
        cargo_changed_file_kind(Path::new("src/lib.rs")),
        Some(CargoChangedFileKind::Source)
    );
    assert_eq!(
        cargo_changed_file_kind(Path::new("Cargo.toml")),
        Some(CargoChangedFileKind::Manifest)
    );
    assert_eq!(
        cargo_changed_file_kind(Path::new("Cargo.lock")),
        Some(CargoChangedFileKind::Lockfile)
    );
    assert_eq!(
        cargo_changed_file_kind(Path::new(".cargo/config.toml")),
        Some(CargoChangedFileKind::Config)
    );
    assert_eq!(cargo_changed_file_kind(Path::new("README.md")), None);
    assert_eq!(cargo_changed_file_kind(Path::new("Cargo.toml.bak")), None);
}

#[tokio::test]
async fn single_package_source_selects_nearest_manifest() {
    let repo = tempdir().expect("create repository");
    write_file(
        &repo,
        "Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    );
    write_file(&repo, "src/lib.rs", "pub fn value() -> u8 { 1 }\n");

    let command = resolve(&repo, &["src/lib.rs"])
        .await
        .expect("resolve cargo provider")
        .expect("cargo provider should match");

    assert_eq!(command.kind, AutomaticValidationProviderKind::Cargo);
    assert_eq!(command.cwd, absolute_root(&repo));
    assert_eq!(
        &command.command[..9],
        [
            "fake-cargo",
            "check",
            "--quiet",
            "--message-format=json",
            "--color",
            "never",
            "--jobs",
            "2",
            "--manifest-path",
        ]
    );
    assert_eq!(
        command.command[9],
        repo.path().join("Cargo.toml").display().to_string()
    );
    assert_eq!(command.command[10], "--target-dir");
    let execution_cwd = command
        .execution_cwd
        .as_ref()
        .expect("cargo execution directory should be isolated");
    let target_dir = execution_cwd.join("target");
    assert_eq!(Path::new(&command.command[11]), target_dir.as_ref());
    assert_eq!(command.command[12], "--locked");
    assert!(!execution_cwd.as_ref().starts_with(repo.path()));
    assert!(execution_cwd.as_ref().is_dir());
}

#[tokio::test]
async fn rust_toolchain_is_staged_in_isolated_directory() {
    let repo = tempdir().expect("create repository");
    write_file(
        &repo,
        "Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    );
    write_file(&repo, "src/lib.rs", "pub fn value() {}\n");
    let toolchain = "[toolchain]\nchannel = \"stable\"\ncomponents = [\"rustfmt\"]\n";
    write_file(&repo, "rust-toolchain.toml", toolchain);

    let command = resolve(&repo, &["src/lib.rs"])
        .await
        .expect("resolve cargo provider")
        .expect("cargo provider should match");
    let execution_cwd = command
        .execution_cwd
        .as_ref()
        .expect("cargo execution directory should be isolated");

    assert_eq!(
        std::fs::read_to_string(execution_cwd.join("rust-toolchain.toml"))
            .expect("read staged toolchain file"),
        toolchain
    );
}

#[tokio::test]
async fn rust_toolchain_path_override_is_configuration_error() {
    let repo = tempdir().expect("create repository");
    write_file(
        &repo,
        "Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    );
    write_file(&repo, "src/lib.rs", "pub fn value() {}\n");
    write_file(
        &repo,
        "rust-toolchain",
        "# local override\n[toolchain]\npath = \"./toolchain\"\n",
    );

    let error = resolve(&repo, &["src/lib.rs"])
        .await
        .expect_err("repository path toolchains should be rejected");

    assert!(matches!(
        error.kind,
        AutomaticValidationProviderErrorKind::Configuration
    ));
    assert!(error.message.contains("path overrides"));
}

#[tokio::test]
async fn package_manifest_change_uses_all_targets() {
    let repo = tempdir().expect("create repository");
    write_file(
        &repo,
        "crate/Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    );

    let command = resolve(&repo, &["crate/Cargo.toml"])
        .await
        .expect("resolve cargo provider")
        .expect("cargo provider should match");

    assert_eq!(command.cwd.as_ref(), repo.path().join("crate"));
    assert!(command.command.contains(&"--all-targets".to_string()));
    assert!(!command.command.contains(&"--workspace".to_string()));
}

#[tokio::test]
async fn package_target_hints_select_changed_nondefault_targets() {
    let repo = tempdir().expect("create repository");
    write_file(
        &repo,
        "crate/Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    );
    for path in [
        "crate/tests/api.rs",
        "crate/benches/latency.rs",
        "crate/examples/demo.rs",
    ] {
        write_file(&repo, path, "fn main() {}\n");
    }

    let command = resolve(
        &repo,
        &[
            "crate/tests/api.rs",
            "crate/benches/latency.rs",
            "crate/examples/demo.rs",
        ],
    )
    .await
    .expect("resolve cargo provider")
    .expect("cargo provider should match");

    for argument in ["--tests", "--benches", "--examples"] {
        assert!(command.command.contains(&argument.to_string()));
    }
}

#[tokio::test]
async fn one_workspace_member_stays_package_scoped() {
    let repo = tempdir().expect("create repository");
    write_file(
        &repo,
        "Cargo.toml",
        "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"2\"\n",
    );
    write_file(
        &repo,
        "a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
    );
    write_file(&repo, "a/src/lib.rs", "pub fn a() {}\n");
    write_file(
        &repo,
        "b/Cargo.toml",
        "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
    );

    let command = resolve(&repo, &["a/src/lib.rs"])
        .await
        .expect("resolve cargo provider")
        .expect("cargo provider should match");

    assert_eq!(command.cwd.as_ref(), repo.path().join("a"));
    assert!(!command.command.contains(&"--workspace".to_string()));
}

#[tokio::test]
async fn multiple_workspace_members_use_workspace_fallback() {
    let repo = tempdir().expect("create repository");
    write_file(
        &repo,
        "Cargo.toml",
        "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"2\"\n",
    );
    for member in ["a", "b"] {
        write_file(
            &repo,
            &format!("{member}/Cargo.toml"),
            &format!("[package]\nname = \"{member}\"\nversion = \"0.1.0\"\n"),
        );
        write_file(
            &repo,
            &format!("{member}/src/lib.rs"),
            "pub fn value() {}\n",
        );
    }

    let command = resolve(&repo, &["a/src/lib.rs", "b/src/lib.rs"])
        .await
        .expect("resolve cargo provider")
        .expect("cargo provider should match");

    assert_eq!(command.cwd, absolute_root(&repo));
    assert!(command.command.contains(&"--workspace".to_string()));
}

#[tokio::test]
async fn workspace_member_globs_support_bounded_fallback() {
    let repo = tempdir().expect("create repository");
    write_file(
        &repo,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
    );
    for member in ["a", "b"] {
        write_file(
            &repo,
            &format!("crates/{member}/Cargo.toml"),
            &format!("[package]\nname = \"{member}\"\nversion = \"0.1.0\"\n"),
        );
        write_file(
            &repo,
            &format!("crates/{member}/src/lib.rs"),
            "pub fn value() {}\n",
        );
    }

    let command = resolve(&repo, &["crates/a/src/lib.rs", "crates/b/src/lib.rs"])
        .await
        .expect("resolve cargo provider")
        .expect("cargo provider should match");

    assert_eq!(command.cwd, absolute_root(&repo));
    assert!(command.command.contains(&"--workspace".to_string()));
}

#[tokio::test]
async fn workspace_fallback_rejects_descendant_nonmember_package() {
    let repo = tempdir().expect("create repository");
    write_file(
        &repo,
        "Cargo.toml",
        "[workspace]\nmembers = [\"a\"]\nresolver = \"2\"\n",
    );
    for package in ["a", "b"] {
        write_file(
            &repo,
            &format!("{package}/Cargo.toml"),
            &format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\n"),
        );
        write_file(
            &repo,
            &format!("{package}/src/lib.rs"),
            "pub fn value() {}\n",
        );
    }

    let error = resolve(&repo, &["a/src/lib.rs", "b/src/lib.rs"])
        .await
        .expect_err("workspace fallback must not skip a nonmember package");

    assert!(matches!(
        error.kind,
        AutomaticValidationProviderErrorKind::Infrastructure
    ));
    assert!(error.message.contains("could not prove"));
}

#[tokio::test]
async fn workspace_lock_and_config_changes_use_workspace_fallback() {
    let repo = tempdir().expect("create repository");
    write_file(
        &repo,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crate\"]\nresolver = \"2\"\n",
    );
    write_file(&repo, "Cargo.lock", "version = 4\n");
    write_file(&repo, ".cargo/config.toml", "[build]\njobs = 1\n");

    let command = resolve(&repo, &["Cargo.lock", ".cargo/config.toml"])
        .await
        .expect("resolve cargo provider")
        .expect("cargo provider should match");

    assert!(command.command.contains(&"--workspace".to_string()));
    assert_eq!(
        command
            .command
            .iter()
            .filter(|argument| argument.as_str() == "--locked")
            .count(),
        1
    );
}

#[tokio::test]
async fn parent_cargo_config_selects_nested_workspace() {
    let repo = tempdir().expect("create repository");
    write_file(&repo, ".cargo/config.toml", "[build]\njobs = 1\n");
    write_file(
        &repo,
        "rust/Cargo.toml",
        "[workspace]\nmembers = [\"crate\"]\nresolver = \"2\"\n",
    );
    write_file(
        &repo,
        "rust/crate/Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    );

    let command = resolve(&repo, &[".cargo/config.toml"])
        .await
        .expect("resolve cargo provider")
        .expect("cargo provider should match nested workspace");

    assert_eq!(command.cwd.as_ref(), repo.path().join("rust"));
    assert!(command.command.contains(&"--workspace".to_string()));
}

#[tokio::test]
async fn parent_cargo_config_rejects_ambiguous_nested_packages() {
    let repo = tempdir().expect("create repository");
    write_file(&repo, ".cargo/config.toml", "[build]\njobs = 1\n");
    for package in ["a", "b"] {
        write_file(
            &repo,
            &format!("{package}/Cargo.toml"),
            &format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\n"),
        );
    }

    let error = resolve(&repo, &[".cargo/config.toml"])
        .await
        .expect_err("ambiguous config scope should fail explicitly");

    assert!(matches!(
        error.kind,
        AutomaticValidationProviderErrorKind::Infrastructure
    ));
    assert!(error.message.contains("more than one nearest"));
}

#[tokio::test]
async fn parent_cargo_config_discovery_caps_directory_entries() {
    let repo = tempdir().expect("create repository");
    write_file(&repo, ".cargo/config.toml", "[build]\njobs = 1\n");
    for index in 0..=CARGO_MAX_DISCOVERY_ENTRIES {
        write_file(&repo, &format!("fixtures/entry-{index}.txt"), "fixture\n");
    }

    let error = resolve(&repo, &[".cargo/config.toml"])
        .await
        .expect_err("directory entry cap should stop discovery");

    assert!(matches!(
        error.kind,
        AutomaticValidationProviderErrorKind::Infrastructure
    ));
    assert!(error.message.contains("directory entries"));
}

#[tokio::test]
async fn nested_cargo_config_selects_deeper_workspace() {
    let repo = tempdir().expect("create repository");
    write_file(&repo, "rust/.cargo/config.toml", "[build]\njobs = 1\n");
    write_file(
        &repo,
        "rust/workspace/Cargo.toml",
        "[workspace]\nmembers = []\nresolver = \"2\"\n",
    );

    let command = resolve(&repo, &["rust/.cargo/config.toml"])
        .await
        .expect("resolve cargo provider")
        .expect("cargo provider should match nested config scope");

    assert_eq!(command.cwd.as_ref(), repo.path().join("rust/workspace"));
    assert!(command.command.contains(&"--workspace".to_string()));
}

#[tokio::test]
async fn malformed_cargo_config_is_configuration_error() {
    let repo = tempdir().expect("create repository");
    write_file(&repo, "Cargo.toml", "[workspace]\nmembers = []\n");
    write_file(&repo, ".cargo/config.toml", "[build\njobs = nope\n");

    let error = resolve(&repo, &[".cargo/config.toml"])
        .await
        .expect_err("malformed cargo config should fail discovery");

    assert!(matches!(
        error.kind,
        AutomaticValidationProviderErrorKind::Configuration
    ));
    assert!(error.message.contains("failed to parse cargo config"));
}

#[tokio::test]
async fn unrelated_or_orphan_rust_files_do_not_select_cargo() {
    let repo = tempdir().expect("create repository");
    write_file(&repo, "README.md", "fixture\n");
    write_file(&repo, "orphan.rs", "pub fn value() {}\n");

    assert!(
        resolve(&repo, &["README.md", "orphan.rs"])
            .await
            .expect("resolve cargo provider")
            .is_none()
    );
}

#[tokio::test]
async fn malformed_manifest_is_configuration_error() {
    let repo = tempdir().expect("create repository");
    write_file(&repo, "Cargo.toml", "[package\nname = nope\n");

    let error = resolve(&repo, &["Cargo.toml"])
        .await
        .expect_err("malformed manifest should fail discovery");

    assert!(matches!(
        error.kind,
        AutomaticValidationProviderErrorKind::Configuration
    ));
    assert!(error.message.contains("failed to parse cargo manifest"));
}

#[tokio::test]
async fn independent_packages_fail_instead_of_silently_narrowing() {
    let repo = tempdir().expect("create repository");
    for package in ["a", "b"] {
        write_file(
            &repo,
            &format!("{package}/Cargo.toml"),
            &format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\n"),
        );
        write_file(
            &repo,
            &format!("{package}/src/lib.rs"),
            "pub fn value() {}\n",
        );
    }

    let error = resolve(&repo, &["a/src/lib.rs", "b/src/lib.rs"])
        .await
        .expect_err("independent packages should exceed one-command scope");

    assert!(matches!(
        error.kind,
        AutomaticValidationProviderErrorKind::Infrastructure
    ));
    assert!(error.message.contains("more than one independent"));
}

#[test]
fn cargo_command_requires_one_trusted_executable() {
    let cwd = AbsolutePathBuf::from_absolute_path("/tmp/repo").expect("absolute cwd");
    let error = build_cargo_command(
        &CargoValidationProviderConfig {
            command: vec!["cargo".to_string(), "+nightly".to_string()],
            ..CargoValidationProviderConfig::default()
        },
        CargoTarget::Workspace {
            manifest: cwd.join("Cargo.toml").into_path_buf(),
        },
        /*toolchain_file*/ None,
        /*changed_file_count*/ 1,
    )
    .expect_err("prefix arguments could invoke a cargo alias");

    assert!(matches!(
        error.kind,
        AutomaticValidationProviderErrorKind::Configuration
    ));
}

#[test]
fn cargo_json_errors_are_compacted_for_correction() {
    let mut output = ExecToolCallOutput {
        exit_code: 101,
        ..Default::default()
    };
    output.stdout = StreamOutput::new(compiler_message(
        "crate/src/lib.rs",
        /*line*/ 12,
        "mismatched types",
        "E0308",
    ));

    let classified = classify_cargo_output(&output);

    assert_eq!(
        classified.status,
        ProjectValidationStatus::ActionableFailure
    );
    assert_eq!(
        classified.text,
        "crate/src/lib.rs:12:7: error[E0308]: mismatched types"
    );
}

#[test]
fn cargo_diagnostic_count_is_capped() {
    let stdout = (0..25)
        .map(|index| {
            compiler_message(
                "src/lib.rs",
                u64::try_from(index + 1).expect("line should fit in u64"),
                &format!("error {index}"),
                "E0001",
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let rendered = render_cargo_diagnostics(&stdout).expect("render diagnostics");

    assert_eq!(rendered.lines().count(), CARGO_MAX_DIAGNOSTICS + 1);
    assert!(rendered.ends_with(CARGO_DIAGNOSTICS_TRUNCATED_MARKER));
}

#[test]
fn cargo_diagnostic_bytes_are_hard_capped() {
    let stdout = compiler_message("src/lib.rs", /*line*/ 1, &"x".repeat(10_000), "E0001");

    let rendered = render_cargo_diagnostics(&stdout).expect("render diagnostics");

    assert!(rendered.len() <= CARGO_MAX_DIAGNOSTIC_BYTES);
    assert!(rendered.ends_with(CARGO_DIAGNOSTICS_TRUNCATED_MARKER));
}

#[test]
fn cargo_noncompiler_failures_keep_distinct_statuses() {
    let mut configuration = ExecToolCallOutput {
        exit_code: 101,
        ..Default::default()
    };
    configuration.stderr =
        StreamOutput::new("error: failed to parse manifest at `/tmp/Cargo.toml`".to_string());
    assert_eq!(
        classify_cargo_output(&configuration).status,
        ProjectValidationStatus::ConfigurationError
    );

    let mut infrastructure = ExecToolCallOutput {
        exit_code: 101,
        ..Default::default()
    };
    infrastructure.stderr =
        StreamOutput::new("error: failed to download dependency index".to_string());
    assert_eq!(
        classify_cargo_output(&infrastructure).status,
        ProjectValidationStatus::InfrastructureFailure
    );
}
