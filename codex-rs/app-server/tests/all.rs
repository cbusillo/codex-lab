#![allow(clippy::expect_used)]

// Single integration test binary that aggregates all test modules.
// The submodules live in `tests/suite/`.
#[cfg(debug_assertions)]
use ctor::ctor;
#[cfg(debug_assertions)]
use ctor::dtor;
#[cfg(debug_assertions)]
use std::io::Write;

#[cfg(debug_assertions)]
#[ctor]
fn install_test_keyring_store() {
    let install_result = codex_keyring_store::tests::install_persisted_default_test_keyring_store(
        codex_keyring_store::tests::shared_test_keyring_root(),
    );
    if !matches!(install_result, Ok(true)) {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(
            stderr,
            "failed to install persisted app-server test keyring store: {install_result:?}"
        );
        drop(stderr);
        std::process::abort();
    }
}

#[cfg(debug_assertions)]
#[dtor]
fn remove_test_keyring_store() {
    if let Err(error) = codex_keyring_store::tests::remove_shared_test_keyring_root() {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(
            stderr,
            "failed to remove app-server test keyring directory: {error}"
        );
    }
}

mod suite;
