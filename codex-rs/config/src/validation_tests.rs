use super::*;
use crate::config_toml::ConfigToml;
use pretty_assertions::assert_eq;

fn groups(input: &str) -> ValidationGroups {
    toml::from_str::<ConfigToml>(input)
        .expect("validation config should deserialize")
        .validation
        .expect("validation config should be present")
        .groups
}

fn project_command(input: &str) -> ProjectValidationCommand {
    toml::from_str::<ConfigToml>(input)
        .expect("validation config should deserialize")
        .validation
        .expect("validation config should be present")
        .project_command
        .expect("project command should be present")
}

fn shellcheck_provider(input: &str) -> ShellcheckValidationProviderConfig {
    toml::from_str::<ConfigToml>(input)
        .expect("validation config should deserialize")
        .validation
        .expect("validation config should be present")
        .providers
        .shellcheck
}

fn cargo_provider(input: &str) -> CargoValidationProviderConfig {
    toml::from_str::<ConfigToml>(input)
        .expect("validation config should deserialize")
        .validation
        .expect("validation config should be present")
        .providers
        .cargo
}

#[test]
fn validation_groups_deserialize_explicit_values() {
    assert_eq!(
        groups("[validation.groups]\nfunctional = false\nstylistic = true\n"),
        ValidationGroups {
            functional: false,
            stylistic: true,
        }
    );
}

#[test]
fn validation_groups_default_matches_deserialization_defaults() {
    assert_eq!(
        ValidationGroups::default(),
        ValidationGroups {
            functional: true,
            stylistic: false,
        }
    );
}

#[test]
fn explicit_groups_table_defaults_functional_checks_on() {
    assert_eq!(
        groups("[validation.groups]\nstylistic = true\n"),
        ValidationGroups {
            functional: true,
            stylistic: true,
        }
    );
}

#[test]
fn serialized_defaults_do_not_disable_partial_validation_overrides() {
    let mut merged =
        toml::Value::try_from(ConfigToml::default()).expect("default config should serialize");
    let override_value = toml::from_str("[validation.groups]\nstylistic = true\n")
        .expect("validation override should deserialize");
    crate::merge_toml_values(&mut merged, &override_value);
    let config: ConfigToml = merged
        .try_into()
        .expect("merged validation config should deserialize");

    assert_eq!(
        config
            .validation
            .expect("validation should be present")
            .groups,
        ValidationGroups {
            functional: true,
            stylistic: true,
        }
    );
}

#[test]
fn project_command_deserializes_argv_and_timeout() {
    assert_eq!(
        project_command(
            "[validation.project_command]\ncommand = [\"just\", \"test\"]\ntimeout_ms = 120000\n"
        ),
        ProjectValidationCommand {
            command: vec!["just".to_string(), "test".to_string()],
            timeout_ms: 120_000,
        }
    );
}

#[test]
fn project_command_defaults_timeout() {
    assert_eq!(
        project_command("[validation.project_command]\ncommand = [\"just\", \"test\"]\n"),
        ProjectValidationCommand {
            command: vec!["just".to_string(), "test".to_string()],
            timeout_ms: DEFAULT_PROJECT_VALIDATION_TIMEOUT_MS,
        }
    );
}

#[test]
fn empty_project_command_table_is_preserved_for_runtime_error_reporting() {
    assert_eq!(
        project_command("[validation.project_command]\n"),
        ProjectValidationCommand::default()
    );
}

#[test]
fn shellcheck_provider_defaults_to_trusted_builtin_command() {
    assert_eq!(
        shellcheck_provider("[validation.groups]\nfunctional = true\n"),
        ShellcheckValidationProviderConfig::default()
    );
}

#[test]
fn shellcheck_provider_deserializes_trusted_override() {
    assert_eq!(
        shellcheck_provider(
            "[validation.providers.shellcheck]\nenabled = false\ncommand = [\"/bin/sh\", \"fixture\"]\ntimeout_ms = 5000\n"
        ),
        ShellcheckValidationProviderConfig {
            enabled: false,
            command: vec!["/bin/sh".to_string(), "fixture".to_string()],
            timeout_ms: 5_000,
        }
    );
}

#[test]
fn cargo_provider_defaults_to_trusted_builtin_command() {
    assert_eq!(
        cargo_provider("[validation.groups]\nfunctional = true\n"),
        CargoValidationProviderConfig::default()
    );
}

#[test]
fn cargo_provider_deserializes_trusted_override() {
    assert_eq!(
        cargo_provider(
            "[validation.providers.cargo]\nenabled = false\ncommand = [\"/usr/bin/cargo\"]\ntimeout_ms = 25000\n"
        ),
        CargoValidationProviderConfig {
            enabled: false,
            command: vec!["/usr/bin/cargo".to_string()],
            timeout_ms: 25_000,
        }
    );
}
