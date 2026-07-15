use anyhow::Context;
use anyhow::Result;
#[cfg(debug_assertions)]
use app_test_support::USE_TEST_KEYRING_STORE_ARG;
use codex_keyring_store::KeyringStore;
use codex_keyring_store::tests::HermeticTestKeyringStore;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(debug_assertions)]
use std::process::Command;
use tempfile::TempDir;

#[test]
fn persisted_store_round_trips_overwrites_and_deletes() -> Result<()> {
    let root = TempDir::new()?;
    let store = HermeticTestKeyringStore::persisted(root.path().to_path_buf());

    assert_eq!(store.load("service", "account")?, None);
    store.save("service", "account", "first")?;
    assert_eq!(store.load("service", "account")?, Some("first".into()));

    store.save("service", "account", "second")?;
    assert_eq!(store.load("service", "account")?, Some("second".into()));
    assert_eq!(store.load("service", "other")?, None);

    #[cfg(unix)]
    {
        assert_eq!(root.path().metadata()?.permissions().mode() & 0o777, 0o700);
        let service_dir = std::fs::read_dir(root.path())?
            .next()
            .context("persisted test keyring should contain a service directory")??
            .path();
        assert_eq!(service_dir.metadata()?.permissions().mode() & 0o777, 0o700);
        let credential_file = std::fs::read_dir(service_dir)?
            .next()
            .context("persisted test keyring should contain a credential file")??
            .path();
        assert_eq!(
            credential_file.metadata()?.permissions().mode() & 0o777,
            0o600
        );
    }

    assert!(store.delete("service", "account")?);
    assert!(!store.delete("service", "account")?);
    assert_eq!(store.load("service", "account")?, None);
    Ok(())
}

#[test]
fn persisted_store_shares_saved_passphrase_across_instances() -> Result<()> {
    let root = TempDir::new()?;
    let store = HermeticTestKeyringStore::persisted(root.path().to_path_buf());

    assert_eq!(store.load("codex", "secrets|test-home")?, None);
    store.save("codex", "secrets|test-home", "test-passphrase")?;
    let reopened_store = HermeticTestKeyringStore::persisted(root.path().to_path_buf());
    assert_eq!(
        reopened_store.load("codex", "secrets|test-home")?,
        Some("test-passphrase".to_string())
    );
    assert_eq!(store.load("other", "secrets|test-home")?, None);

    let other_root = TempDir::new()?;
    let other_store = HermeticTestKeyringStore::persisted(other_root.path().to_path_buf());
    assert_eq!(other_store.load("codex", "secrets|test-home")?, None);
    Ok(())
}

#[cfg(debug_assertions)]
#[test]
fn test_keyring_flag_requires_directory() -> Result<()> {
    let codex_home = TempDir::new()?;
    let output = Command::new(codex_utils_cargo_bin::cargo_bin("codex-app-server")?)
        .env("CODEX_LAB_HOME", codex_home.path())
        .env_remove("CODEX_APP_SERVER_TEST_KEYRING_DIR")
        .args([USE_TEST_KEYRING_STORE_ARG, "--listen", "off"])
        .output()?;

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains(
            "CODEX_APP_SERVER_TEST_KEYRING_DIR must be set when --use-test-keyring-store is used"
        ),
        "expected missing test keyring directory error in stderr, got: {stderr}"
    );
    Ok(())
}
