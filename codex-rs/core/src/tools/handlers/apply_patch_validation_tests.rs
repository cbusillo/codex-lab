use super::*;
use codex_apply_patch::AppliedPatchChange;
use codex_apply_patch::AppliedPatchFileChange;
use codex_config::ValidationGroups;
use core_test_support::PathBufExt;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

fn functional_validation() -> ValidationConfig {
    ValidationConfig {
        groups: ValidationGroups {
            functional: true,
            ..Default::default()
        },
    }
}

fn added_file(path: PathBuf, content: &str) -> AppliedPatchChange {
    AppliedPatchChange {
        path,
        change: AppliedPatchFileChange::Add {
            content: content.to_string(),
            overwritten_content: None,
        },
    }
}

fn finding(tool: &str, file: &str, message: String) -> Value {
    json!({"tool": tool, "file": file, "msg": message})
}

fn rendered_summary(changes: &[AppliedPatchChange], cwd: &AbsolutePathBuf) -> (String, Value) {
    let summary = render_validation_summary(changes, &functional_validation(), cwd)
        .expect("structural files should produce a summary");
    let parsed = serde_json::from_str(&summary).expect("summary should be valid JSON");
    (summary, parsed)
}

fn assert_summary(
    changes: &[AppliedPatchChange],
    cwd: &AbsolutePathBuf,
    issues: Vec<Value>,
    checks: &[&str],
    issue_count: usize,
    truncated: bool,
) {
    let (_, parsed) = rendered_summary(changes, cwd);
    assert_eq!(
        parsed["validation"],
        json!({
            "issues": issues,
            "checks": checks,
            "issue_count": issue_count,
            "truncated": truncated,
        })
    );
}

fn assert_skipped(changes: &[AppliedPatchChange], cwd: &AbsolutePathBuf, file: &str, msg: String) {
    assert_summary(
        changes,
        cwd,
        vec![finding("json-parse", file, msg)],
        &["json-parse"],
        1,
        false,
    );
}

#[test]
fn structural_parsers_report_checks_and_findings() {
    let cwd = std::env::temp_dir().join("validation-formats").abs();
    let invalid_toml = "value =";
    let invalid_yaml = "value: [";
    let changes = vec![
        added_file(cwd.join("valid.json").to_path_buf(), "{}"),
        added_file(cwd.join("invalid.toml").to_path_buf(), invalid_toml),
        added_file(cwd.join("invalid.yaml").to_path_buf(), invalid_yaml),
    ];
    let toml_message = format!(
        "invalid TOML: {}",
        toml::from_str::<toml::Value>(invalid_toml).expect_err("fixture should be invalid TOML")
    );
    let yaml_message = format!(
        "invalid YAML: {}",
        serde_yaml::from_str::<serde_yaml::Value>(invalid_yaml)
            .expect_err("fixture should be invalid YAML")
    );

    assert_summary(
        &changes,
        &cwd,
        vec![
            finding("toml-parse", "invalid.toml", toml_message),
            finding("yaml-parse", "invalid.yaml", yaml_message),
        ],
        &["json-parse", "toml-parse", "yaml-parse"],
        2,
        false,
    );
}

#[test]
fn invalid_json_findings_are_deterministic_and_bounded() {
    let cwd = std::env::temp_dir().join("validation-bounded").abs();
    let changes = (1..=13)
        .rev()
        .map(|index| {
            added_file(
                cwd.join(format!("bad-{index:02}.json")).to_path_buf(),
                "{ invalid",
            )
        })
        .collect::<Vec<_>>();
    let message = format!(
        "invalid JSON: {}",
        serde_json::from_str::<Value>("{ invalid").expect_err("fixture should be invalid JSON")
    );
    let issues = (1..=12)
        .map(|index| {
            finding(
                "json-parse",
                &format!("bad-{index:02}.json"),
                message.clone(),
            )
        })
        .collect();

    assert_summary(&changes, &cwd, issues, &["json-parse"], 13, true);
}

#[test]
fn validation_summary_respects_byte_budget() {
    let cwd = std::env::temp_dir().join("validation-byte-budget").abs();
    let long_name = "x".repeat(1_000);
    let long_content = format!("{} =", "long_key".repeat(300));
    let changes = (1..=12)
        .map(|index| {
            added_file(
                cwd.join(format!("{long_name}-{index}.toml")).to_path_buf(),
                &long_content,
            )
        })
        .collect::<Vec<_>>();
    let (summary, parsed) = rendered_summary(&changes, &cwd);

    assert!(summary.len() <= MAX_SUMMARY_BYTES);
    assert_eq!(parsed["validation"]["issue_count"], json!(12));
    assert_eq!(parsed["validation"]["truncated"], json!(true));
    assert!(
        parsed["validation"]["issues"]
            .as_array()
            .is_some_and(|issues| issues.len() < 12)
    );
}

#[test]
fn oversized_structural_file_reports_skipped_validation() {
    let cwd = std::env::temp_dir().join("validation-file-limit").abs();
    let changes = vec![added_file(
        cwd.join("large.json").to_path_buf(),
        &"x".repeat(MAX_FILE_INPUT_BYTES + 1),
    )];

    assert_skipped(
        &changes,
        &cwd,
        "large.json",
        format!("JSON validation skipped: file exceeds {MAX_FILE_INPUT_BYTES}-byte limit"),
    );
}

#[test]
fn total_input_limit_reports_skipped_file() {
    let cwd = std::env::temp_dir().join("validation-total-limit").abs();
    let valid_json = format!("\"{}\"", "x".repeat(MAX_FILE_INPUT_BYTES - 2));
    let changes = (1..=5)
        .map(|index| {
            added_file(
                cwd.join(format!("large-{index}.json")).to_path_buf(),
                &valid_json,
            )
        })
        .collect::<Vec<_>>();

    assert_skipped(
        &changes,
        &cwd,
        "large-5.json",
        format!("JSON validation skipped: patch exceeds {MAX_TOTAL_INPUT_BYTES}-byte input limit"),
    );
}
