use super::*;
use pretty_assertions::assert_eq;

#[test]
fn auth_profile_rejects_explicit_remote_app_server() {
    let endpoint = RemoteAppServerEndpoint::WebSocket {
        websocket_url: "ws://127.0.0.1:4500".to_string(),
        auth_token: None,
    };

    let error = ensure_auth_profile_supports_remote_target(Some("work"), Some(&endpoint))
        .expect_err("remote app servers cannot resolve local auth profiles");

    assert_eq!(
        error.to_string(),
        "--auth-profile cannot be used with a remote app server"
    );
}

#[tokio::test]
async fn auth_profile_config_reloads_preserve_auth_home() {
    let codex_home = tempfile::tempdir().expect("create temporary Codex home");
    let auth_home = tempfile::tempdir().expect("create temporary auth home");
    let cwd = tempfile::tempdir().expect("create temporary cwd");
    let overrides = ConfigOverrides {
        cwd: Some(cwd.path().to_path_buf()),
        ..Default::default()
    };
    let loader_overrides = LoaderOverrides::without_managed_config_for_tests();

    let initial_config = load_config_or_exit(
        ConfigHomes {
            codex_home: codex_home.path().to_path_buf(),
            auth_home: auth_home.path().to_path_buf(),
        },
        Vec::new(),
        overrides.clone(),
        loader_overrides.clone(),
        CloudConfigBundleLoader::default(),
        /*strict_config*/ false,
    )
    .await;
    let reloaded_config = load_config_or_exit(
        ConfigHomes {
            codex_home: codex_home.path().to_path_buf(),
            auth_home: initial_config.auth_home.to_path_buf(),
        },
        Vec::new(),
        overrides,
        loader_overrides,
        CloudConfigBundleLoader::default(),
        /*strict_config*/ false,
    )
    .await;

    assert_eq!(
        initial_config.codex_home,
        AbsolutePathBuf::from_absolute_path(codex_home.path())
            .expect("temporary Codex home should be absolute")
    );
    assert_eq!(
        initial_config.auth_home,
        AbsolutePathBuf::from_absolute_path(auth_home.path())
            .expect("temporary auth home should be absolute")
    );
    assert_eq!(reloaded_config.auth_home, initial_config.auth_home);
}

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
