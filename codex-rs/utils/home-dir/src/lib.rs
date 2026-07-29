use codex_utils_absolute_path::AbsolutePathBuf;
use dirs::home_dir;
use std::path::PathBuf;

/// Returns the path to the Codex Lab configuration directory, which can be
/// specified by the `CODEX_LAB_HOME` environment variable. If not set,
/// defaults to `~/.codex-lab`.
///
/// - If `CODEX_LAB_HOME` is set, the value must exist and be a directory. The
///   value will be canonicalized and this function will Err otherwise.
/// - If `CODEX_LAB_HOME` is not set, this function does not verify that the
///   directory exists.
pub fn find_codex_home() -> std::io::Result<AbsolutePathBuf> {
    let codex_lab_home_env = std::env::var("CODEX_LAB_HOME")
        .ok()
        .filter(|val| !val.is_empty());
    find_codex_home_from_env(codex_lab_home_env.as_deref(), home_dir())
}

fn find_codex_home_from_env(
    codex_lab_home_env: Option<&str>,
    default_home: Option<PathBuf>,
) -> std::io::Result<AbsolutePathBuf> {
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
            let mut path = default_home.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not find home directory",
                )
            })?;
            path.push(".codex-lab");
            AbsolutePathBuf::from_absolute_path(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::find_codex_home_from_env;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::io::ErrorKind;
    use tempfile::TempDir;

    #[test]
    fn find_codex_lab_home_env_missing_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let missing = temp_home.path().join("missing-codex-lab-home");
        let missing_str = missing
            .to_str()
            .expect("missing codex lab home path should be valid utf-8");

        let err = find_codex_home_from_env(Some(missing_str), /*default_home*/ None)
            .expect_err("missing CODEX_LAB_HOME");
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(
            err.to_string().contains("CODEX_LAB_HOME"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_codex_lab_home_env_file_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let file_path = temp_home.path().join("codex-home.txt");
        fs::write(&file_path, "not a directory").expect("write temp file");
        let file_str = file_path
            .to_str()
            .expect("file codex lab home path should be valid utf-8");

        let err = find_codex_home_from_env(Some(file_str), /*default_home*/ None)
            .expect_err("file CODEX_LAB_HOME");
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

        let resolved = find_codex_home_from_env(Some(temp_str), /*default_home*/ None)
            .expect("valid CODEX_LAB_HOME");
        let expected = temp_home
            .path()
            .canonicalize()
            .expect("canonicalize temp home");
        let expected = AbsolutePathBuf::from_absolute_path(expected).expect("absolute home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn find_codex_home_without_env_uses_codex_lab_default() {
        let temp_home = TempDir::new().expect("temp home");
        let resolved = find_codex_home_from_env(
            /*codex_lab_home_env*/ None,
            Some(temp_home.path().to_path_buf()),
        )
        .expect("default CODEX_LAB_HOME");
        let expected = temp_home.path().join(".codex-lab");
        let expected = AbsolutePathBuf::from_absolute_path(expected).expect("absolute home");
        assert_eq!(resolved, expected);
    }
}
