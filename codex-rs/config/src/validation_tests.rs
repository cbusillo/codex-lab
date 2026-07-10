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
