use super::*;
use std::process::Command;
use tempfile::tempdir;

#[tokio::test]
async fn shellcheck_matching_uses_extension_and_shebang() {
    let repo = tempdir().expect("create temp repo");
    std::fs::write(repo.path().join("extension.sh"), "printf ok\n")
        .expect("write extension fixture");
    std::fs::write(repo.path().join("shebang"), "#!/bin/sh\nprintf ok\n")
        .expect("write shebang fixture");
    std::fs::write(
        repo.path().join("env-shebang"),
        "#!/usr/bin/env bash\nprintf ok\n",
    )
    .expect("write env shebang fixture");
    std::fs::write(
        repo.path().join("helper.py"),
        "#!/usr/bin/env python3\nprint('ok')\n",
    )
    .expect("write Python fixture");
    std::fs::write(repo.path().join("plain"), "printf ok\n").expect("write plain fixture");

    assert_eq!(
        matching_shellcheck_files(
            repo.path(),
            &[
                PathBuf::from("helper.py"),
                PathBuf::from("plain"),
                PathBuf::from("shebang"),
                PathBuf::from("env-shebang"),
                PathBuf::from("extension.sh"),
            ],
        )
        .await,
        vec![
            PathBuf::from("env-shebang"),
            PathBuf::from("extension.sh"),
            PathBuf::from("shebang"),
        ]
    );
}

#[test]
fn shellcheck_command_appends_fixed_arguments_and_paths() {
    let cwd = AbsolutePathBuf::from_absolute_path("/tmp/repo").expect("absolute cwd");
    let command = build_shellcheck_command(
        &ShellcheckValidationProviderConfig {
            command: vec!["/bin/sh".to_string(), "fixture".to_string()],
            timeout_ms: 5_000,
            ..ShellcheckValidationProviderConfig::default()
        },
        cwd.clone(),
        vec![PathBuf::from("scripts/check.sh")],
        /*changed_file_count*/ 1,
    )
    .expect("build shellcheck command");

    assert_eq!(
        command.command,
        vec!["/bin/sh", "fixture", "-f", "gcc", "scripts/check.sh"]
    );
    assert_eq!(command.cwd, cwd);
    assert_eq!(command.kind, AutomaticValidationProviderKind::Shellcheck);
    assert_eq!(command.timeout_ms, 5_000);
    assert_eq!(command.changed_file_count, 1);
}

#[test]
fn shellcheck_command_rejects_more_than_bounded_file_count() {
    let cwd = AbsolutePathBuf::from_absolute_path("/tmp/repo").expect("absolute cwd");
    let files = (0..=SHELLCHECK_MAX_FILES)
        .map(|index| PathBuf::from(format!("scripts/check-{index}.sh")))
        .collect::<Vec<_>>();
    let changed_file_count = u32::try_from(files.len()).expect("file count should fit in u32");

    let error = build_shellcheck_command(
        &ShellcheckValidationProviderConfig::default(),
        cwd,
        files,
        changed_file_count,
    )
    .expect_err("file cap should reject oversized provider run");

    assert!(matches!(
        error.kind,
        AutomaticValidationProviderErrorKind::Infrastructure
    ));
    assert!(error.message.contains("more than 64 changed files"));
}

#[test]
fn preselection_errors_are_provider_neutral_when_both_are_enabled() {
    let mut config = ValidationConfig::default();

    assert!(configured_provider_command(&config).is_empty());

    config.providers.shellcheck.enabled = false;
    assert_eq!(configured_provider_command(&config), vec!["cargo"]);
}

#[tokio::test]
async fn shellcheck_provider_includes_files_committed_during_turn() {
    let repo = tempdir().expect("create temp repo");
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Test User"],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .expect("run git setup command")
                .success()
        );
    }
    std::fs::write(repo.path().join("README.md"), "baseline\n").expect("write baseline");
    assert!(
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(repo.path())
            .status()
            .expect("stage baseline")
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-qm", "baseline"])
            .current_dir(repo.path())
            .status()
            .expect("commit baseline")
            .success()
    );
    let base_commit = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo.path())
        .output()
        .expect("read baseline commit");
    let base_commit = String::from_utf8(base_commit.stdout)
        .expect("baseline commit should be utf-8")
        .trim()
        .to_string();

    std::fs::create_dir(repo.path().join("scripts")).expect("create scripts directory");
    std::fs::write(
        repo.path().join("scripts/committed.sh"),
        "#!/bin/sh\nprintf ok\n",
    )
    .expect("write committed shell file");
    assert!(
        Command::new("git")
            .args(["add", "scripts/committed.sh"])
            .current_dir(repo.path())
            .status()
            .expect("stage shell file")
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-qm", "add shell file"])
            .current_dir(repo.path())
            .status()
            .expect("commit shell file")
            .success()
    );

    let mut config = ValidationConfig::default();
    config.groups.functional = true;
    config.providers.shellcheck.command = vec!["/bin/sh".to_string(), "fixture".to_string()];
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf()).expect("absolute repo path");
    let resolution = resolve_automatic_validation_provider(&config, &cwd, Some(&base_commit))
        .await
        .expect("resolve provider");
    let AutomaticValidationProviderResolution::Command(command) = resolution else {
        panic!("shellcheck should be selected");
    };

    assert_eq!(
        command.command,
        vec!["/bin/sh", "fixture", "-f", "gcc", "scripts/committed.sh",]
    );
    assert_eq!(command.changed_file_count, 1);
}

#[tokio::test]
async fn cargo_provider_precedes_shellcheck_for_mixed_rust_changes() {
    let repo = tempdir().expect("create temp repo");
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Test User"],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .expect("run git setup command")
                .success()
        );
    }
    std::fs::create_dir_all(repo.path().join("src")).expect("create source directory");
    std::fs::create_dir_all(repo.path().join("scripts")).expect("create scripts directory");
    std::fs::write(
        repo.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .expect("write manifest");
    std::fs::write(repo.path().join("src/lib.rs"), "pub fn value() {}\n")
        .expect("write Rust fixture");
    std::fs::write(repo.path().join("scripts/check.sh"), "#!/bin/sh\n")
        .expect("write shell fixture");
    for args in [vec!["add", "--all"], vec!["commit", "-qm", "baseline"]] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .expect("commit baseline")
                .success()
        );
    }
    std::fs::write(
        repo.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .expect("change Rust fixture");
    std::fs::write(
        repo.path().join("scripts/check.sh"),
        "#!/bin/sh\nprintf ok\n",
    )
    .expect("change shell fixture");

    let mut config = ValidationConfig::default();
    config.groups.functional = true;
    config.providers.cargo.command = vec!["fake-cargo".to_string()];
    config.providers.shellcheck.command = vec!["fake-shellcheck".to_string()];
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf()).expect("absolute repo path");
    let resolution =
        resolve_automatic_validation_provider(&config, &cwd, /*base_commit*/ None)
            .await
            .expect("resolve provider");
    let AutomaticValidationProviderResolution::Command(command) = resolution else {
        panic!("cargo should be selected for mixed Rust and shell changes");
    };

    assert_eq!(command.kind, AutomaticValidationProviderKind::Cargo);
    assert_eq!(command.changed_file_count, 2);
}
