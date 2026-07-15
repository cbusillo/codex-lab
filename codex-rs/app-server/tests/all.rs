// Single integration test binary that aggregates all test modules.
// The submodules live in `tests/suite/`.
#[cfg(debug_assertions)]
use ctor::ctor;

#[cfg(debug_assertions)]
#[ctor]
fn install_test_keyring_store() {
    assert!(
        codex_keyring_store::tests::install_persisted_default_test_keyring_store(
            codex_keyring_store::tests::shared_test_keyring_root(),
        )
    );
}

mod suite;
