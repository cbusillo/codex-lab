use super::*;
use pretty_assertions::assert_ne;
use tempfile::tempdir;

#[test]
fn model_cache_scope_isolates_provider_config_and_execution_account() {
    let codex_home = tempdir().expect("temp dir should be created");
    let provider =
        ModelProviderInfo::create_openai_provider(Some("https://api.example.test/v1".to_string()));
    let account_a = scoped_models_cache_home(codex_home.path(), "shared", &provider, "account-a");
    let account_b = scoped_models_cache_home(codex_home.path(), "shared", &provider, "account-b");
    let other_provider_id =
        scoped_models_cache_home(codex_home.path(), "other", &provider, "account-a");

    assert_ne!(account_a, account_b);
    assert_ne!(account_a, other_provider_id);
    assert_ne!(
        account_a.join("models_cache.json"),
        codex_home.path().join("models_cache.json")
    );
}
