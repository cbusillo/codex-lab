use super::*;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use std::process::Command;

use super::super::project_validation_coordinator::ProjectValidationSuccessCache;

#[test]
fn cargo_success_environment_fingerprint_is_order_independent_and_complete() {
    let first = HashMap::from([
        ("PATH".to_string(), "/first/bin".to_string()),
        ("BUILD_INPUT".to_string(), "alpha".to_string()),
    ]);
    let reordered = HashMap::from([
        ("BUILD_INPUT".to_string(), "alpha".to_string()),
        ("PATH".to_string(), "/first/bin".to_string()),
    ]);
    let changed_path = HashMap::from([
        ("PATH".to_string(), "/second/bin".to_string()),
        ("BUILD_INPUT".to_string(), "alpha".to_string()),
    ]);
    let changed_build_input = HashMap::from([
        ("PATH".to_string(), "/first/bin".to_string()),
        ("BUILD_INPUT".to_string(), "beta".to_string()),
    ]);

    let fingerprint = cargo_success_environment_fingerprint(&first);

    assert_eq!(
        cargo_success_environment_fingerprint(&reordered),
        fingerprint
    );
    assert_ne!(
        cargo_success_environment_fingerprint(&changed_path),
        fingerprint
    );
    assert_ne!(
        cargo_success_environment_fingerprint(&changed_build_input),
        fingerprint
    );
}

#[tokio::test]
async fn cargo_success_cache_misses_after_environment_change() {
    let temp = tempfile::tempdir().expect("create temp directory");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .expect("write Cargo manifest");
    for arguments in [
        vec!["init", "-q"],
        vec!["add", "Cargo.toml"],
        vec![
            "-c",
            "user.name=Project Validation Test",
            "-c",
            "user.email=project-validation@example.com",
            "commit",
            "-qm",
            "initial",
        ],
    ] {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(temp.path())
            .status()
            .expect("run git command");
        assert!(status.success());
    }
    let cwd = AbsolutePathBuf::from_absolute_path(temp.path())
        .expect("temporary directory should be absolute");
    let plan = ValidationCommandPlan {
        kind: ValidationCommandKind::Cargo,
        command: vec![
            "cargo".to_string(),
            "check".to_string(),
            "--manifest-path".to_string(),
            temp.path().join("Cargo.toml").display().to_string(),
            "--target-dir".to_string(),
            temp.path().join("target").display().to_string(),
        ],
        cwd: cwd.clone(),
        execution_cwd: cwd.clone(),
        _execution_cwd_guard: None,
        cargo_toolchain: Some(
            "rust-toolchain.toml\n[toolchain]\nchannel = \"1.95.0\"\n".to_string(),
        ),
        timeout_ms: 5_000,
        changed_file_count: Some(1),
    };
    let first_environment = HashMap::from([
        ("PATH".to_string(), "/first/bin".to_string()),
        ("BUILD_INPUT".to_string(), "alpha".to_string()),
    ]);
    let changed_environment = HashMap::from([
        ("PATH".to_string(), "/first/bin".to_string()),
        ("BUILD_INPUT".to_string(), "beta".to_string()),
    ]);
    let cancellation_token = CancellationToken::new();
    let repository_root = temp.path().to_path_buf();
    let first_key = cargo_validation_success_key(
        &cwd,
        Some(&repository_root),
        &plan,
        Some("cargo 1.95.0 (fixture)"),
        &first_environment,
        &cancellation_token,
    )
    .await
    .expect("first success key");
    let changed_key = cargo_validation_success_key(
        &cwd,
        Some(&repository_root),
        &plan,
        Some("cargo 1.95.0 (fixture)"),
        &changed_environment,
        &cancellation_token,
    )
    .await
    .expect("changed success key");
    let cache = ProjectValidationSuccessCache::default();
    cache.record_successful_validation(first_key.clone()).await;

    assert!(cache.has_successful_validation(&first_key).await);
    assert!(!cache.has_successful_validation(&changed_key).await);
}

#[test]
fn cargo_validation_managed_profile_adds_only_cache_target_write_access() {
    let temp = tempfile::tempdir().expect("create temp directory");
    let workspace = AbsolutePathBuf::from_absolute_path(temp.path().join("workspace"))
        .expect("workspace should be absolute");
    let cache_target = AbsolutePathBuf::from_absolute_path(temp.path().join("cache/target"))
        .expect("cache target should be absolute");
    let unrelated = AbsolutePathBuf::from_absolute_path(temp.path().join("unrelated"))
        .expect("unrelated path should be absolute");
    let file_system = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry::new(
            FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            FileSystemAccessMode::Read,
        ),
        FileSystemSandboxEntry::new(
            FileSystemPath::Path {
                path: PathUri::from_abs_path(&workspace),
            },
            FileSystemAccessMode::Write,
        ),
    ]);
    let original =
        PermissionProfile::from_runtime_permissions(&file_system, NetworkSandboxPolicy::Enabled);

    let actual = cargo_validation_permission_profile(&original, &workspace, Some(&cache_target));
    let expected_file_system = file_system
        .with_additional_writable_roots(workspace.as_path(), std::slice::from_ref(&cache_target));
    let expected = PermissionProfile::from_runtime_permissions(
        &expected_file_system,
        NetworkSandboxPolicy::Enabled,
    );

    assert_eq!(actual, expected);
    let PermissionProfile::Managed { file_system, .. } = actual else {
        panic!("expected managed permission profile");
    };
    let file_system = file_system.to_sandbox_policy();
    assert!(file_system.can_write_path_with_cwd(cache_target.as_path(), workspace.as_path()));
    assert!(!file_system.can_write_path_with_cwd(unrelated.as_path(), workspace.as_path()));
}

#[test]
fn cargo_validation_external_profile_keeps_ephemeral_target() {
    let temp = tempfile::tempdir().expect("create temp directory");
    let workspace = AbsolutePathBuf::from_absolute_path(temp.path().join("workspace"))
        .expect("workspace should be absolute");
    let cache_target = AbsolutePathBuf::from_absolute_path(temp.path().join("cache/target"))
        .expect("cache target should be absolute");
    let external = PermissionProfile::External {
        network: NetworkSandboxPolicy::Restricted,
    };

    assert!(!persistent_cargo_validation_cache_allowed(&external));
    assert_eq!(
        cargo_validation_permission_profile(&external, &workspace, Some(&cache_target)),
        external
    );
    assert!(persistent_cargo_validation_cache_allowed(
        &PermissionProfile::Disabled
    ));
}

#[test]
fn truncates_command_within_hard_byte_cap() {
    let command = vec!["x".repeat(PROJECT_VALIDATION_COMMAND_MAX_BYTES * 2)];
    let (truncated, did_truncate) = truncate_command(command);
    let command_bytes = truncated.iter().fold(0usize, |total, argument| {
        total.saturating_add(argument.len() + 1)
    });

    assert!(did_truncate);
    assert!(command_bytes <= PROJECT_VALIDATION_COMMAND_MAX_BYTES);
    assert_eq!(
        truncated.last().map(String::as_str),
        Some(COMMAND_TRUNCATED_MARKER)
    );
}

#[test]
fn truncates_output_within_hard_byte_cap() {
    let output = "a".repeat(PROJECT_VALIDATION_OUTPUT_MAX_BYTES * 2);
    let (truncated, did_truncate) = truncate_output(&output);

    assert!(did_truncate);
    assert!(truncated.len() <= PROJECT_VALIDATION_OUTPUT_MAX_BYTES);
    assert!(truncated.contains(OUTPUT_TRUNCATED_MARKER));
    assert!(truncated.starts_with('a'));
    assert!(truncated.ends_with('a'));
}

#[test]
fn truncates_utf8_output_on_character_boundaries() {
    let output = "🦀".repeat(PROJECT_VALIDATION_OUTPUT_MAX_BYTES);
    let (truncated, did_truncate) = truncate_output(&output);

    assert!(did_truncate);
    assert!(truncated.len() <= PROJECT_VALIDATION_OUTPUT_MAX_BYTES);
    assert!(truncated.contains(OUTPUT_TRUNCATED_MARKER));
}
