use super::*;
use codex_keyring_store::tests::MockKeyringStore;
use pretty_assertions::assert_eq;

#[test]
fn mcp_oauth_cache_reuses_plaintext_and_invalidates_when_ciphertext_changes() -> Result<()> {
    let _cache_lock = MCP_OAUTH_CACHE_TEST_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let codex_home = tempfile::tempdir().expect("tempdir");
    let keyring = Arc::new(MockKeyringStore::default());
    let first = LocalSecretsBackend::new_with_namespace(
        codex_home.path().to_path_buf(),
        keyring.clone(),
        LocalSecretsNamespace::McpOAuth,
    );
    let second = LocalSecretsBackend::new_with_namespace(
        codex_home.path().to_path_buf(),
        keyring,
        LocalSecretsNamespace::McpOAuth,
    );
    let scope = SecretScope::Global;
    let name = SecretName::new("TEST_SECRET")?;
    let cached_file = || {
        Arc::clone(
            &MCP_OAUTH_CACHE
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .expect("MCP OAuth credentials should be cached")
                .file,
        )
    };

    first.set(&scope, &name, "one")?;
    let (first_cached, second_cached) = std::thread::scope(|threads| {
        let first_reader = threads.spawn(|| {
            assert_eq!(first.get(&scope, &name)?, Some("one".to_string()));
            Ok::<_, anyhow::Error>(cached_file())
        });
        let second_reader = threads.spawn(|| {
            assert_eq!(second.get(&scope, &name)?, Some("one".to_string()));
            Ok::<_, anyhow::Error>(cached_file())
        });
        Ok::<_, anyhow::Error>((
            first_reader.join().expect("first credential reader")?,
            second_reader.join().expect("second credential reader")?,
        ))
    })?;
    assert!(Arc::ptr_eq(&first_cached, &second_cached));

    assert_eq!(second.get(&scope, &name)?, Some("one".to_string()));
    assert!(Arc::ptr_eq(&first_cached, &cached_file()));

    first.set(&scope, &name, "two")?;
    assert!(
        MCP_OAUTH_CACHE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_none_or(|cached| cached.path != first.secrets_path())
    );
    assert_eq!(second.get(&scope, &name)?, Some("two".to_string()));
    assert!(!Arc::ptr_eq(&first_cached, &cached_file()));

    assert!(first.delete(&scope, &name)?);
    assert!(
        MCP_OAUTH_CACHE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_none_or(|cached| cached.path != first.secrets_path())
    );
    assert_eq!(second.get(&scope, &name)?, None);

    Ok(())
}
