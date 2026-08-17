use codex_utils_cargo_bin::CargoBinError;
use std::path::PathBuf;

struct RemoveFileOnDrop(PathBuf);

impl Drop for RemoveFileOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn missing_binary_returns_not_found_error() {
    let missing_name = "codex-utils-cargo-bin-missing-test-binary";

    let error = codex_utils_cargo_bin::cargo_bin(missing_name)
        .expect_err("missing binary lookup should return an error");

    assert!(
        matches!(&error, CargoBinError::NotFound { name, .. } if name == missing_name),
        "unexpected error: {error:?}"
    );
}

#[test]
fn target_directory_fallback_finds_existing_binary() {
    if codex_utils_cargo_bin::runfiles_available() {
        return;
    }

    let binary_name = format!("codex-utils-cargo-bin-fallback-test-{}", std::process::id());
    let mut fallback_path = std::env::current_exe().expect("test executable should be available");
    fallback_path.pop();
    if fallback_path.ends_with("deps") {
        fallback_path.pop();
    }
    fallback_path.push(format!("{binary_name}{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&fallback_path, []).expect("fallback test binary should be created");
    let _cleanup = RemoveFileOnDrop(fallback_path.clone());

    let resolved = codex_utils_cargo_bin::cargo_bin(&binary_name)
        .expect("target-directory fallback should resolve the test binary");

    assert!(
        resolved == fallback_path,
        "expected {fallback_path:?}, got {resolved:?}"
    );
}
