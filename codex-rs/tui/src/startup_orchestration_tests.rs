use super::*;
use pretty_assertions::assert_eq;

#[test]
fn explicit_workspace_roots_select_exact_workspace_profile() {
    let temp_dir = tempfile::tempdir().expect("create temporary workspace");
    let workspace_base = AbsolutePathBuf::try_from(temp_dir.path().to_path_buf())
        .expect("temporary workspace should be absolute");
    let relative_root = PathBuf::from("nested");
    let expected_root =
        AbsolutePathBuf::resolve_path_against_base(relative_root.clone(), workspace_base.as_path());

    assert_eq!(
        resolve_workspace_root_config_overrides(
            &[relative_root],
            Some(&workspace_base),
            Some(SandboxMode::WorkspaceWrite),
        )
        .expect("workspace root overrides should resolve"),
        WorkspaceRootConfigOverrides {
            sandbox_mode: None,
            default_permissions: Some(BUILT_IN_PERMISSION_PROFILE_WORKSPACE.to_string()),
            workspace_roots: Some(vec![expected_root]),
        }
    );
}

#[test]
fn explicit_workspace_roots_reject_remote_workspaces() {
    let error = resolve_workspace_root_config_overrides(
        &[PathBuf::from("nested")],
        /*config_cwd*/ None,
        Some(SandboxMode::WorkspaceWrite),
    )
    .expect_err("remote workspaces cannot resolve local workspace roots");

    assert_eq!(
        error.to_string(),
        "--workspace-root is unavailable for remote workspaces"
    );
}
