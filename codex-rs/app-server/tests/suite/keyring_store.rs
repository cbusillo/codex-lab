use anyhow::Context;
use anyhow::Result;
use app_test_support::DISABLE_PLUGIN_STARTUP_TASKS_ARG;
use app_test_support::TEST_KEYRING_DIR_ENV_VAR;
use app_test_support::USE_TEST_KEYRING_STORE_ARG;
use codex_keyring_store::DefaultKeyringStore;
use codex_keyring_store::KeyringStore;
use codex_keyring_store::tests::HermeticTestKeyringStore;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::sleep;
use tokio::time::timeout;

const CHILD_PROCESS_ENV_VAR: &str = "CODEX_APP_SERVER_TEST_KEYRING_CHILD";
const CHILD_ACCOUNT_ENV_VAR: &str = "CODEX_APP_SERVER_TEST_KEYRING_CHILD_ACCOUNT";
const CHILD_VALUE_ENV_VAR: &str = "CODEX_APP_SERVER_TEST_KEYRING_CHILD_VALUE";
const CROSS_PROCESS_TEST_NAME: &str =
    "suite::keyring_store::persisted_store_shares_values_across_processes";

#[test]
fn persisted_store_round_trips_overwrites_and_deletes() -> Result<()> {
    let root = TempDir::new()?;
    let store = HermeticTestKeyringStore::persisted(root.path().to_path_buf());
    #[cfg(unix)]
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755))?;

    assert_eq!(store.load("service", "account")?, None);
    store.save("service", "account", "first")?;
    assert_eq!(store.load("service", "account")?, Some("first".into()));

    #[cfg(unix)]
    let (service_dir, credential_file) = {
        let service_dir = std::fs::read_dir(root.path())?
            .next()
            .context("persisted test keyring should contain a service directory")??
            .path();
        let credential_file = std::fs::read_dir(&service_dir)?
            .next()
            .context("persisted test keyring should contain a credential file")??
            .path();
        std::fs::set_permissions(&service_dir, std::fs::Permissions::from_mode(0o755))?;
        std::fs::set_permissions(&credential_file, std::fs::Permissions::from_mode(0o644))?;
        (service_dir, credential_file)
    };

    store.save("service", "account", "second")?;
    assert_eq!(store.load("service", "account")?, Some("second".into()));
    assert_eq!(store.load("service", "other")?, None);

    #[cfg(unix)]
    {
        assert_eq!(root.path().metadata()?.permissions().mode() & 0o777, 0o700);
        assert_eq!(service_dir.metadata()?.permissions().mode() & 0o777, 0o700);
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
fn persisted_store_shares_values_across_instances_and_isolates_roots() -> Result<()> {
    let root = TempDir::new()?;
    let store = HermeticTestKeyringStore::persisted(root.path().to_path_buf());

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

#[test]
fn persisted_store_shares_values_across_processes() -> Result<()> {
    if std::env::var_os(CHILD_PROCESS_ENV_VAR).is_some() {
        let account = std::env::var(CHILD_ACCOUNT_ENV_VAR)?;
        let value = std::env::var(CHILD_VALUE_ENV_VAR)?;
        let store = DefaultKeyringStore;
        assert_eq!(store.load("cross-process", &account)?, Some(value.clone()));
        store.save("cross-process", &format!("{account}-ack"), "child-read")?;
        return Ok(());
    }

    let root = TempDir::new()?;
    let store = HermeticTestKeyringStore::persisted(root.path().to_path_buf());
    let process_id = std::process::id();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let account = format!("account-{process_id}-{timestamp}");
    let value = format!("parent-value-{process_id}-{timestamp}");
    store.save("cross-process", &account, &value)?;
    let output = Command::new(std::env::current_exe()?)
        .env(CHILD_PROCESS_ENV_VAR, "1")
        .env(CHILD_ACCOUNT_ENV_VAR, &account)
        .env(CHILD_VALUE_ENV_VAR, &value)
        .env(TEST_KEYRING_DIR_ENV_VAR, root.path())
        .args(["--exact", CROSS_PROCESS_TEST_NAME, "--nocapture"])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "child keyring test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        store.load("cross-process", &format!("{account}-ack"))?,
        Some("child-read".to_string()),
        "child test did not acknowledge reading from the persisted store"
    );
    Ok(())
}

#[tokio::test]
async fn app_server_flag_initializes_selected_test_store() -> Result<()> {
    let codex_home = TempDir::new()?;
    let keyring_parent = TempDir::new()?;
    let keyring_root = keyring_parent.path().join("app-server-keyring");
    let mut command =
        tokio::process::Command::new(codex_utils_cargo_bin::cargo_bin("codex-app-server")?);
    command
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("CODEX_LAB_HOME", codex_home.path())
        .env(TEST_KEYRING_DIR_ENV_VAR, &keyring_root)
        .args([
            USE_TEST_KEYRING_STORE_ARG,
            DISABLE_PLUGIN_STARTUP_TASKS_ARG,
            "--listen",
            "stdio://",
        ]);
    let mut child = command.spawn()?;
    let initialization_result = timeout(Duration::from_secs(30), async {
        loop {
            if keyring_root.is_dir() {
                anyhow::ensure!(
                    child.try_wait()?.is_none(),
                    "app-server exited before the selected test keyring store was ready"
                );
                return Ok::<_, anyhow::Error>(());
            }
            if let Some(status) = child.try_wait()? {
                anyhow::bail!(
                    "app-server exited with {status} before initializing the selected test keyring store"
                );
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("timed out waiting for app-server test keyring initialization")
    .and_then(std::convert::identity);
    let cleanup_result = async {
        if child.try_wait()?.is_none() {
            child.kill().await?;
            child.wait().await?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    initialization_result?;
    cleanup_result?;
    assert!(keyring_root.is_dir());
    #[cfg(unix)]
    assert_eq!(keyring_root.metadata()?.permissions().mode() & 0o777, 0o700);
    Ok(())
}

#[test]
fn test_keyring_flag_requires_directory() -> Result<()> {
    let codex_home = TempDir::new()?;
    let output = Command::new(codex_utils_cargo_bin::cargo_bin("codex-app-server")?)
        .env("CODEX_LAB_HOME", codex_home.path())
        .env_remove(TEST_KEYRING_DIR_ENV_VAR)
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
