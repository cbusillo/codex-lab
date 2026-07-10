use codex_apply_patch::AppliedPatchChange;
use codex_apply_patch::AppliedPatchDelta;
use codex_apply_patch::AppliedPatchFileChange;
use codex_config::ValidationConfig;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

const MAX_ISSUES: usize = 12;
const MAX_FILE_BYTES: usize = 240;
const MAX_FILE_INPUT_BYTES: usize = 1024 * 1024;
const MAX_MESSAGE_BYTES: usize = 800;
const MAX_SUMMARY_BYTES: usize = 4 * 1024;
const MAX_TOTAL_INPUT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidationFinding {
    tool: &'static str,
    file: Option<String>,
    message: String,
}

#[derive(Clone, Copy)]
enum StructuralParser {
    Json,
    Toml,
    Yaml,
}

pub(super) fn append_validation_feedback(
    mut content: String,
    delta: Option<&AppliedPatchDelta>,
    config: &ValidationConfig,
    cwd: &AbsolutePathBuf,
) -> String {
    let Some(delta) = delta else {
        return content;
    };
    if !delta.is_exact() {
        return content;
    }
    let Some(summary) = render_validation_summary(delta.changes(), config, cwd) else {
        return content;
    };

    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&summary);
    content
}

fn render_validation_summary(
    changes: &[AppliedPatchChange],
    config: &ValidationConfig,
    cwd: &AbsolutePathBuf,
) -> Option<String> {
    if !config.groups.functional {
        return None;
    }

    let files = final_file_contents(changes);
    let mut checks = BTreeSet::new();
    let mut findings = Vec::new();
    let mut remaining_input_bytes = MAX_TOTAL_INPUT_BYTES;
    for (path, content) in &files {
        validate_file(
            path,
            content,
            cwd,
            &mut remaining_input_bytes,
            &mut checks,
            &mut findings,
        );
    }
    if checks.is_empty() {
        return None;
    }

    findings.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then_with(|| left.tool.cmp(right.tool))
            .then_with(|| left.message.cmp(&right.message))
    });
    let issue_count = findings.len();
    let mut surfaced = findings.into_iter().take(MAX_ISSUES).collect::<Vec<_>>();
    let checks = checks.into_iter().collect::<Vec<_>>();

    loop {
        let truncated = surfaced.len() < issue_count;
        let issues = surfaced
            .iter()
            .map(|finding| {
                json!({
                    "tool": finding.tool,
                    "file": finding.file,
                    "msg": finding.message,
                })
            })
            .collect::<Vec<Value>>();
        let summary = json!({
            "validation": {
                "issues": issues,
                "checks": checks,
                "issue_count": issue_count,
                "truncated": truncated,
            }
        })
        .to_string();
        if summary.len() <= MAX_SUMMARY_BYTES || surfaced.is_empty() {
            return Some(summary);
        }
        surfaced.pop();
    }
}

fn final_file_contents(changes: &[AppliedPatchChange]) -> BTreeMap<PathBuf, &str> {
    let mut files = BTreeMap::new();
    for change in changes {
        match &change.change {
            AppliedPatchFileChange::Add { content, .. } => {
                files.insert(change.path.clone(), content.as_str());
            }
            AppliedPatchFileChange::Delete { .. } => {
                files.remove(&change.path);
            }
            AppliedPatchFileChange::Update {
                move_path,
                new_content,
                ..
            } => {
                if let Some(move_path) = move_path {
                    files.remove(&change.path);
                    files.insert(move_path.clone(), new_content.as_str());
                } else {
                    files.insert(change.path.clone(), new_content.as_str());
                }
            }
        }
    }
    files
}

fn validate_file(
    path: &Path,
    content: &str,
    cwd: &AbsolutePathBuf,
    remaining_input_bytes: &mut usize,
    checks: &mut BTreeSet<&'static str>,
    findings: &mut Vec<ValidationFinding>,
) {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let (tool, label, parser) = match extension.as_deref() {
        Some("json") => ("json-parse", "JSON", StructuralParser::Json),
        Some("toml") => ("toml-parse", "TOML", StructuralParser::Toml),
        Some("yaml" | "yml") => ("yaml-parse", "YAML", StructuralParser::Yaml),
        Some(_) | None => return,
    };
    checks.insert(tool);
    if content.len() > MAX_FILE_INPUT_BYTES {
        findings.push(ValidationFinding {
            tool,
            file: display_path(path, cwd),
            message: format!(
                "{label} validation skipped: file exceeds {MAX_FILE_INPUT_BYTES}-byte limit"
            ),
        });
        return;
    }
    if content.len() > *remaining_input_bytes {
        findings.push(ValidationFinding {
            tool,
            file: display_path(path, cwd),
            message: format!(
                "{label} validation skipped: patch exceeds {MAX_TOTAL_INPUT_BYTES}-byte input limit"
            ),
        });
        return;
    }
    *remaining_input_bytes -= content.len();
    let error = match parser {
        StructuralParser::Json => serde_json::from_str::<serde_json::Value>(content)
            .err()
            .map(|error| error.to_string()),
        StructuralParser::Toml => toml::from_str::<toml::Value>(content)
            .err()
            .map(|error| error.to_string()),
        StructuralParser::Yaml => serde_yaml::from_str::<serde_yaml::Value>(content)
            .err()
            .map(|error| error.to_string()),
    };
    if let Some(error) = error {
        findings.push(ValidationFinding {
            tool,
            file: display_path(path, cwd),
            message: truncate_utf8(&format!("invalid {label}: {error}"), MAX_MESSAGE_BYTES),
        });
    }
}

fn display_path(path: &Path, cwd: &AbsolutePathBuf) -> Option<String> {
    let display_path = if path.is_relative() {
        path
    } else {
        path.strip_prefix(cwd.as_path()).ok()?
    };
    Some(truncate_utf8(
        &display_path.display().to_string(),
        MAX_FILE_BYTES,
    ))
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
#[path = "apply_patch_validation_tests.rs"]
mod tests;
