use std::ffi::OsStr;

pub(crate) const ENV_VAR: &str = "CODEX_LAB_PINNED_CANDIDATE_WARNING";

const MESSAGE_PREFIX: &str = "Pinned Codex Lab candidate ";
const MAX_MESSAGE_BYTES: usize = 4096;

pub(crate) fn is_valid(value: &str) -> bool {
    value.len() <= MAX_MESSAGE_BYTES && value.starts_with(MESSAGE_PREFIX)
}

pub(crate) fn from_env_value(value: Option<&OsStr>) -> Option<String> {
    let value = value?.to_str()?;
    if !is_valid(value) {
        return None;
    }
    Some(value.to_string())
}

pub(crate) fn find_in(warnings: &[String]) -> Option<&str> {
    warnings
        .iter()
        .map(String::as_str)
        .find(|warning| is_valid(warning))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_bounded_launcher_warning() {
        let warning =
            "Pinned Codex Lab candidate abc (clean) is older than local source def (clean).";

        assert_eq!(
            from_env_value(Some(OsStr::new(warning))),
            Some(warning.to_string())
        );
    }

    #[test]
    fn rejects_untrusted_or_oversized_values() {
        assert_eq!(from_env_value(Some(OsStr::new("unrelated warning"))), None);
        assert_eq!(
            from_env_value(Some(OsStr::new(&format!(
                "{MESSAGE_PREFIX}{}",
                "x".repeat(MAX_MESSAGE_BYTES)
            )))),
            None
        );
    }

    #[test]
    fn finds_only_validated_pinned_warning() {
        let warning =
            "Pinned Codex Lab candidate abc (clean) is older than local source def (clean).";
        let warnings = vec!["unrelated warning".to_string(), warning.to_string()];

        assert_eq!(find_in(&warnings), Some(warning));
    }
}
