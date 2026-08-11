use super::*;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use pretty_assertions::assert_eq;

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
                path: workspace.clone(),
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
