use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use codex_keyring_store::tests::MockKeyringStore;
use pretty_assertions::assert_eq;

use super::*;
use crate::SecretsBackendKind;
use crate::SecretsManager;

#[cfg(unix)]
struct PermissionRestoreGuard {
    path: PathBuf,
    permissions: Option<fs::Permissions>,
}

#[cfg(unix)]
impl Drop for PermissionRestoreGuard {
    fn drop(&mut self) {
        if let Some(permissions) = self.permissions.take() {
            let _ = fs::set_permissions(&self.path, permissions);
        }
    }
}

#[test]
fn manager_mutation_reports_whether_ciphertext_changed() -> Result<()> {
    let codex_home = tempfile::tempdir()?;
    let manager = SecretsManager::new_with_keyring_store_and_namespace(
        codex_home.path().to_path_buf(),
        SecretsBackendKind::Local,
        Arc::new(MockKeyringStore::default()),
        LocalSecretsNamespace::CodexAuth,
    );
    let scope = SecretScope::Global;
    let name = SecretName::new("LOGIN_STATE")?;

    assert!(manager.mutate(&scope, &name, |_| {
        Ok(SecretMutation::Set("one".to_string()))
    })?);
    assert_eq!(manager.get(&scope, &name)?, Some("one".to_string()));
    assert!(!manager.mutate(&scope, &name, |_| Ok(SecretMutation::Keep))?);
    assert!(!manager.mutate(&scope, &name, |_| {
        Ok(SecretMutation::Set("one".to_string()))
    })?);
    assert!(manager.mutate(&scope, &name, |_| Ok(SecretMutation::Delete))?);
    assert!(!manager.mutate(&scope, &name, |_| Ok(SecretMutation::Delete))?);
    assert_eq!(manager.get(&scope, &name)?, None);

    let error = manager
        .mutate(&scope, &name, |_| Ok(SecretMutation::Set(String::new())))
        .expect_err("empty replacement must fail");
    assert!(error.to_string().contains("must not be empty"));
    Ok(())
}

#[test]
fn concurrent_mutations_observe_committed_updates() -> Result<()> {
    let codex_home = tempfile::tempdir()?;
    let manager = SecretsManager::new_with_keyring_store_and_namespace(
        codex_home.path().to_path_buf(),
        SecretsBackendKind::Local,
        Arc::new(MockKeyringStore::default()),
        LocalSecretsNamespace::CodexAuth,
    );
    let scope = SecretScope::Global;
    let name = SecretName::new("LOGIN_STATE")?;
    manager.set(&scope, &name, "0")?;

    let (first_started_sender, first_started_receiver) = mpsc::channel();
    let (first_release_sender, first_release_receiver) = mpsc::channel();
    let first_manager = manager.clone();
    let first_scope = scope.clone();
    let first_name = name.clone();
    let first = thread::spawn(move || {
        first_manager.mutate(&first_scope, &first_name, |current| {
            let current = current
                .context("first mutation expected an existing value")?
                .parse::<u64>()?;
            first_started_sender.send(())?;
            first_release_receiver.recv_timeout(Duration::from_secs(/*secs*/ 5))?;
            Ok(SecretMutation::Set((current + 1).to_string()))
        })
    });
    first_started_receiver.recv_timeout(Duration::from_secs(/*secs*/ 5))?;

    let (second_started_sender, second_started_receiver) = mpsc::channel();
    let second_manager = manager.clone();
    let second_scope = scope.clone();
    let second_name = name.clone();
    let second_ready = Arc::new(Barrier::new(/*n*/ 2));
    let second_thread_ready = second_ready.clone();
    let second = thread::spawn(move || {
        second_thread_ready.wait();
        second_manager.mutate(&second_scope, &second_name, |current| {
            second_started_sender.send(())?;
            let current = current
                .context("second mutation expected an existing value")?
                .parse::<u64>()?;
            Ok(SecretMutation::Set((current + 1).to_string()))
        })
    });

    second_ready.wait();
    assert!(
        second_started_receiver
            .recv_timeout(Duration::from_millis(/*millis*/ 100))
            .is_err(),
        "second callback must wait for the first mutation to commit"
    );
    first_release_sender.send(())?;
    assert!(first.join().expect("first mutation thread")?);
    second_started_receiver.recv_timeout(Duration::from_secs(/*secs*/ 5))?;
    assert!(second.join().expect("second mutation thread")?);

    assert_eq!(manager.get(&scope, &name)?, Some("2".to_string()));
    Ok(())
}

#[test]
fn callback_error_preserves_ciphertext() -> Result<()> {
    let codex_home = tempfile::tempdir()?;
    let backend = LocalSecretsBackend::new(
        codex_home.path().to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    );
    let scope = SecretScope::Global;
    let name = SecretName::new("LOGIN_STATE")?;
    backend.set(&scope, &name, "before")?;
    let ciphertext = fs::read(backend.secrets_path())?;

    let error = backend
        .mutate(&scope, &name, &mut |current| -> Result<SecretMutation> {
            assert_eq!(current, Some("before"));
            anyhow::bail!("mutation rejected")
        })
        .expect_err("callback failure must abort the mutation");

    assert!(error.to_string().contains("mutation rejected"));
    assert_eq!(fs::read(backend.secrets_path())?, ciphertext);
    assert_eq!(backend.get(&scope, &name)?, Some("before".to_string()));
    Ok(())
}

#[test]
fn decrypt_error_does_not_invoke_callback_or_rewrite_ciphertext() -> Result<()> {
    let codex_home = tempfile::tempdir()?;
    let backend = LocalSecretsBackend::new(
        codex_home.path().to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    );
    let scope = SecretScope::Global;
    let name = SecretName::new("LOGIN_STATE")?;
    backend.set(&scope, &name, "before")?;
    fs::write(backend.secrets_path(), b"not age ciphertext")?;
    let ciphertext = fs::read(backend.secrets_path())?;
    let mut callback_called = false;

    backend
        .mutate(&scope, &name, &mut |_| {
            callback_called = true;
            Ok(SecretMutation::Keep)
        })
        .expect_err("corrupt ciphertext must fail closed");

    assert!(!callback_called);
    assert_eq!(fs::read(backend.secrets_path())?, ciphertext);
    Ok(())
}

#[cfg(unix)]
#[test]
fn write_error_preserves_existing_ciphertext() -> Result<()> {
    let codex_home = tempfile::tempdir()?;
    let backend = LocalSecretsBackend::new(
        codex_home.path().to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    );
    let scope = SecretScope::Global;
    let name = SecretName::new("LOGIN_STATE")?;
    backend.set(&scope, &name, "before")?;
    let ciphertext = fs::read(backend.secrets_path())?;
    let secrets_dir = backend.secrets_dir();
    let original_permissions = fs::metadata(&secrets_dir)?.permissions();
    let mut read_only_permissions = original_permissions.clone();
    read_only_permissions.set_mode(/*mode*/ 0o500);
    fs::set_permissions(&secrets_dir, read_only_permissions)?;
    let mut permission_guard = PermissionRestoreGuard {
        path: secrets_dir.clone(),
        permissions: Some(original_permissions),
    };

    let mutation = backend.mutate(&scope, &name, &mut |_| {
        Ok(SecretMutation::Set("after".to_string()))
    });
    if let Some(permissions) = permission_guard.permissions.take() {
        fs::set_permissions(&secrets_dir, permissions)?;
    }

    mutation.expect_err("read-only secrets directory must reject the write");
    assert_eq!(fs::read(backend.secrets_path())?, ciphertext);
    assert_eq!(backend.get(&scope, &name)?, Some("before".to_string()));
    Ok(())
}
