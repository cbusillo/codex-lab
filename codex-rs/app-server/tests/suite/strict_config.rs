use std::process::Command;

use anyhow::Result;
#[cfg(debug_assertions)]
use app_test_support::configure_test_keyring_for_std_command;
use tempfile::TempDir;

#[test]
fn strict_config_rejects_unknown_config_fields_for_standalone_app_server() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"
foo = "bar"
"#,
    )?;

    let mut command = Command::new(codex_utils_cargo_bin::cargo_bin("codex-app-server")?);
    command.env("CODEX_LAB_HOME", codex_home.path()).env(
        "CODEX_APP_SERVER_MANAGED_CONFIG_PATH",
        codex_home.path().join("managed_config.toml"),
    );
    #[cfg(debug_assertions)]
    configure_test_keyring_for_std_command(
        &mut command,
        &codex_home.path().join("app-server-test-keyring"),
    );
    let output = command
        .args(["--strict-config", "--listen", "off"])
        .output()?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("unknown configuration field `foo`"),
        "expected strict config error in stderr, got: {stderr}"
    );

    Ok(())
}
