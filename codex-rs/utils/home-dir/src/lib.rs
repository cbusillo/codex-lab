use codex_utils_absolute_path::AbsolutePathBuf;
use dirs::home_dir;
use std::path::PathBuf;

/// Returns the path to the Codex Lab configuration directory, which can be
/// specified by the `CODEX_LAB_HOME` environment variable. If not set, defaults
/// to `~/.codex-lab`.
///
/// - If `CODEX_LAB_HOME` is set, the value must exist and be a directory. The
///   value will be canonicalized and this function will Err otherwise.
/// - If `CODEX_LAB_HOME` is not set, this function does not verify that the
///   directory exists.
pub fn find_codex_home() -> std::io::Result<AbsolutePathBuf> {
    let codex_lab_home_env = std::env::var("CODEX_LAB_HOME")
        .ok()
        .filter(|val| !val.is_empty());
    find_codex_home_from_env(codex_lab_home_env.as_deref())
}

fn find_codex_home_from_env(codex_lab_home_env: Option<&str>) -> std::io::Result<AbsolutePathBuf> {
    // Honor `CODEX_LAB_HOME` when set. `CODE_HOME` and `CODEX_HOME` belong to
    // Every Code and upstream Codex; Codex Lab intentionally does not treat them
    // as home selectors.
    match codex_lab_home_env {
        Some(val) => {
            let path = PathBuf::from(val);
            let metadata = std::fs::metadata(&path).map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("CODEX_LAB_HOME points to {val:?}, but that path does not exist"),
                ),
                _ => std::io::Error::new(
                    err.kind(),
                    format!("failed to read CODEX_LAB_HOME {val:?}: {err}"),
                ),
            })?;

            if !metadata.is_dir() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("CODEX_LAB_HOME points to {val:?}, but that path is not a directory"),
                ))
            } else {
                let canonical = path.canonicalize().map_err(|err| {
                    std::io::Error::new(
                        err.kind(),
                        format!("failed to canonicalize CODEX_LAB_HOME {val:?}: {err}"),
                    )
                })?;
                AbsolutePathBuf::from_absolute_path(canonical)
            }
        }
        None => {
            let mut p = home_dir().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not find home directory",
                )
            })?;
            p.push(".codex-lab");
            AbsolutePathBuf::from_absolute_path(p)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::find_codex_home;
    use super::find_codex_home_from_env;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use dirs::home_dir;
    use pretty_assertions::assert_eq;
    use std::ffi::OsStr;
    use std::ffi::OsString;
    use std::fs;
    use std::io::ErrorKind;
    use std::sync::Mutex;
    use std::sync::MutexGuard;
    use std::sync::OnceLock;
    use std::sync::PoisonError;
    use tempfile::TempDir;

    #[test]
    fn find_codex_lab_home_env_missing_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let missing = temp_home.path().join("missing-codex-lab-home");
        let missing_str = missing
            .to_str()
            .expect("missing codex lab home path should be valid utf-8");

        let err = find_codex_home_from_env(Some(missing_str)).expect_err("missing CODEX_LAB_HOME");
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(
            err.to_string().contains("CODEX_LAB_HOME"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_codex_lab_home_env_file_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let file_path = temp_home.path().join("codex-lab-home.txt");
        fs::write(&file_path, "not a directory").expect("write temp file");
        let file_str = file_path
            .to_str()
            .expect("file codex lab home path should be valid utf-8");

        let err = find_codex_home_from_env(Some(file_str)).expect_err("file CODEX_LAB_HOME");
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("not a directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_codex_lab_home_env_valid_directory_canonicalizes() {
        let temp_home = TempDir::new().expect("temp home");
        let temp_str = temp_home
            .path()
            .to_str()
            .expect("temp codex lab home path should be valid utf-8");

        let resolved = find_codex_home_from_env(Some(temp_str)).expect("valid CODEX_LAB_HOME");
        let expected = temp_home
            .path()
            .canonicalize()
            .expect("canonicalize temp home");
        let expected = AbsolutePathBuf::from_absolute_path(expected).expect("absolute home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn find_codex_home_without_env_uses_default_home_dir() {
        let resolved =
            find_codex_home_from_env(/*codex_lab_home_env*/ None).expect("default Codex Lab home");
        let mut expected = home_dir().expect("home dir");
        expected.push(".codex-lab");
        let expected = AbsolutePathBuf::from_absolute_path(expected).expect("absolute home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn find_codex_home_ignores_legacy_home_env_vars() {
        let _env = EnvGuard::new();
        let temp_home = TempDir::new().expect("temp home");
        let codex_home = temp_home.path().join("codex-home");
        let code_home = temp_home.path().join("code-home");
        fs::create_dir_all(&codex_home).expect("create CODEX_HOME candidate");
        fs::create_dir_all(&code_home).expect("create CODE_HOME candidate");

        // Safety: this test updates process-wide env vars under a single test
        // guard and restores their previous values before returning.
        unsafe {
            std::env::remove_var("CODEX_LAB_HOME");
            std::env::set_var("CODEX_HOME", &codex_home);
            std::env::set_var("CODE_HOME", &code_home);
        }

        let resolved = find_codex_home().expect("default Codex Lab home ignores legacy vars");

        let mut expected = home_dir().expect("home dir");
        expected.push(".codex-lab");
        let expected = AbsolutePathBuf::from_absolute_path(expected).expect("absolute home");
        assert_eq!(resolved, expected);
    }

    struct EnvGuard {
        _guard: MutexGuard<'static, ()>,
        previous_codex_lab_home: Option<OsString>,
        previous_codex_home: Option<OsString>,
        previous_code_home: Option<OsString>,
    }

    impl EnvGuard {
        fn new() -> Self {
            static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            let guard = LOCK
                .get_or_init(Mutex::default)
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            Self {
                _guard: guard,
                previous_codex_lab_home: std::env::var_os("CODEX_LAB_HOME"),
                previous_codex_home: std::env::var_os("CODEX_HOME"),
                previous_code_home: std::env::var_os("CODE_HOME"),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            restore_env_var("CODEX_LAB_HOME", self.previous_codex_lab_home.as_deref());
            restore_env_var("CODEX_HOME", self.previous_codex_home.as_deref());
            restore_env_var("CODE_HOME", self.previous_code_home.as_deref());
        }
    }

    fn restore_env_var(key: &str, value: Option<&OsStr>) {
        match value {
            Some(value) => unsafe {
                std::env::set_var(key, value);
            },
            None => unsafe {
                std::env::remove_var(key);
            },
        }
    }
}
