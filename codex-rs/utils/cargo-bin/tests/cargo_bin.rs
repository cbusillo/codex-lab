use codex_utils_cargo_bin::CargoBinError;

#[test]
fn missing_binary_returns_not_found_error() {
    let missing_name = "codex-utils-cargo-bin-missing-test-binary";

    let error = codex_utils_cargo_bin::cargo_bin(missing_name)
        .expect_err("missing binary lookup should return an error");

    assert!(matches!(
        error,
        CargoBinError::NotFound { name, .. } if name == missing_name
    ));
}
