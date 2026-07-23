use super::*;
use pretty_assertions::assert_ne;
use tempfile::tempdir;

#[test]
fn model_cache_scope_isolates_provider_and_execution_account() {
    let codex_home = tempdir().expect("temp dir should be created");
    let account_a = scoped_models_cache_home(codex_home.path(), "openai", "account-a");
    let account_b = scoped_models_cache_home(codex_home.path(), "openai", "account-b");
    let other_provider = scoped_models_cache_home(codex_home.path(), "custom-openai", "account-a");

    assert_ne!(account_a, account_b);
    assert_ne!(account_a, other_provider);
    assert_ne!(
        account_a.join("models_cache.json"),
        codex_home.path().join("models_cache.json")
    );
}
