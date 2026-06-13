use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewTarget;
use codex_utils_path::write_atomically;
use serde::Deserialize;
use serde::Serialize;

pub const SUMMARY_MAX_FINDINGS: usize = 20;
pub const SUMMARY_MAX_FIELD_BYTES: usize = 240;
pub const SUMMARY_MAX_BYTES: usize = 4096;
pub const DETAIL_MAX_BYTES: usize = 16384;
pub const SCHEMA_VERSION: u32 = 1;

const STORE_DIR: &str = "auto-review";
const RUNS_DIR: &str = "runs";
const OMITTED_TEMPLATE_PREFIX: &str = "... ";
const OMITTED_TEMPLATE_SUFFIX: &str = " more finding(s) omitted";

#[derive(Debug, Clone)]
pub struct AutoReviewStore {
    root: PathBuf,
}

impl AutoReviewStore {
    pub fn new(codex_home: impl AsRef<Path>) -> Self {
        Self {
            root: codex_home.as_ref().join(STORE_DIR),
        }
    }

    pub fn save_run(&self, run: &AutoReviewRun) -> Result<PathBuf> {
        validate_run(run)?;
        let path = self.run_path(&run.run_id)?;
        let json = serde_json::to_string_pretty(run)?;
        write_atomically(&path, &format!("{json}\n"))
            .with_context(|| format!("failed to write auto review run {}", run.run_id))?;
        Ok(path)
    }

    pub fn load_run(&self, run_id: &str) -> Result<AutoReviewRun> {
        let path = self.run_path(run_id)?;
        let json = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read auto review run {run_id}"))?;
        let run = serde_json::from_str(&json)
            .with_context(|| format!("failed to parse auto review run {run_id}"))?;
        validate_run(&run)?;
        Ok(run)
    }

    fn run_path(&self, run_id: &str) -> Result<PathBuf> {
        validate_safe_id(run_id).context("auto review run_id")?;
        Ok(self.root.join(RUNS_DIR).join(format!("{run_id}.json")))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct AutoReviewRun {
    pub schema_version: u32,
    pub run_id: String,
    pub status: AutoReviewRunStatus,
    pub source: AutoReviewRunSource,
    pub target: AutoReviewRunTarget,
    pub review_target: ReviewTarget,
    pub started_at_unix_secs: i64,
    pub completed_at_unix_secs: Option<i64>,
    pub model: Option<String>,
    pub error_summary: Option<String>,
    pub findings: Vec<AutoReviewFindingRecord>,
}

impl AutoReviewRun {
    pub fn visible_findings<'a>(
        &'a self,
        active_target: &AutoReviewRunTarget,
        active_review_target: &ReviewTarget,
    ) -> Vec<&'a AutoReviewFindingRecord> {
        if self.status != AutoReviewRunStatus::Completed {
            return Vec::new();
        }
        if !review_target_matches(&self.review_target, active_review_target) {
            return Vec::new();
        }
        if !self.is_current_for(active_target, active_review_target) {
            return Vec::new();
        }
        self.findings.iter().collect()
    }

    pub fn freshness(&self, active_target: &AutoReviewRunTarget) -> AutoReviewFreshness {
        self.target.freshness(active_target)
    }

    fn is_current_for(
        &self,
        active_target: &AutoReviewRunTarget,
        active_review_target: &ReviewTarget,
    ) -> bool {
        match active_review_target {
            ReviewTarget::Commit { sha, .. } => active_target.head_sha.as_deref() == Some(sha),
            _ => self.freshness(active_target) == AutoReviewFreshness::Current,
        }
    }

    pub fn summary(
        &self,
        active_target: &AutoReviewRunTarget,
        active_review_target: &ReviewTarget,
    ) -> AutoReviewSummary {
        render_summary(self.visible_findings(active_target, active_review_target))
    }

    pub fn finding_detail(&self, finding_id: &str, max_bytes: usize) -> Result<AutoReviewDetail> {
        if max_bytes == 0 {
            anyhow::bail!("auto review detail max_bytes must be positive");
        }
        if self.status != AutoReviewRunStatus::Completed {
            anyhow::bail!("auto review run is not completed: {}", self.run_id);
        }

        let finding = self
            .findings
            .iter()
            .find(|finding| finding.finding_id == finding_id)
            .with_context(|| format!("unknown auto review finding id: {finding_id}"))?;
        let effective_max_bytes = max_bytes.min(DETAIL_MAX_BYTES);
        let json = serde_json::to_string_pretty(finding)?;
        let original_bytes = json.len();
        let (content, truncated) = truncate_utf8_with_marker(&json, effective_max_bytes);
        let bytes = content.len();

        Ok(AutoReviewDetail {
            finding_id: finding_id.to_string(),
            bytes,
            original_bytes,
            max_bytes: effective_max_bytes,
            truncated,
            content,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewRunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewRunSource {
    Manual,
    Background,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AutoReviewRunTarget {
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub base_sha: Option<String>,
    pub worktree_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_diff_fingerprint: Option<String>,
}

impl AutoReviewRunTarget {
    pub fn freshness(&self, active: &Self) -> AutoReviewFreshness {
        if self.branch != active.branch || self.worktree_path != active.worktree_path {
            return AutoReviewFreshness::Detached;
        }
        if self.head_sha != active.head_sha {
            return AutoReviewFreshness::Stale;
        }
        if self.base_sha != active.base_sha {
            return AutoReviewFreshness::Stale;
        }
        if self.worktree_diff_fingerprint != active.worktree_diff_fingerprint {
            return AutoReviewFreshness::Stale;
        }
        AutoReviewFreshness::Current
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoReviewFreshness {
    Current,
    Stale,
    Detached,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct AutoReviewFindingRecord {
    pub finding_id: String,
    pub finding: ReviewFinding,
}

impl AutoReviewFindingRecord {
    fn summary_line(&self) -> String {
        let priority = self.finding.priority.to_string();
        let title = truncate_utf8(&self.finding.title, SUMMARY_MAX_FIELD_BYTES);
        let location = truncate_utf8(
            &format!(
                "{}:{}-{}",
                self.finding.code_location.absolute_file_path.display(),
                self.finding.code_location.line_range.start,
                self.finding.code_location.line_range.end
            ),
            SUMMARY_MAX_FIELD_BYTES,
        );
        let finding_id = truncate_utf8(&self.finding_id, 80);
        format!("[P{priority}] {finding_id}: {title} ({location})")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoReviewSummary {
    pub content: String,
    pub rendered_findings: usize,
    pub omitted_findings: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoReviewDetail {
    pub finding_id: String,
    pub bytes: usize,
    pub original_bytes: usize,
    pub max_bytes: usize,
    pub truncated: bool,
    pub content: String,
}

fn review_target_matches(stored: &ReviewTarget, active: &ReviewTarget) -> bool {
    match (stored, active) {
        (
            ReviewTarget::Commit {
                sha: stored_sha, ..
            },
            ReviewTarget::Commit {
                sha: active_sha, ..
            },
        ) => stored_sha == active_sha,
        _ => stored == active,
    }
}

fn render_summary(findings: Vec<&AutoReviewFindingRecord>) -> AutoReviewSummary {
    if findings.is_empty() {
        return AutoReviewSummary {
            content: String::new(),
            rendered_findings: 0,
            omitted_findings: 0,
            truncated: false,
        };
    }

    let mut lines = Vec::new();
    let max_rendered_findings = findings.len().min(SUMMARY_MAX_FINDINGS);
    let mut truncated = false;

    for finding in findings.iter().take(max_rendered_findings) {
        let line = finding.summary_line();
        let omitted_after_candidate = findings.len() - (lines.len() + 1);
        let reserved_bytes = if omitted_after_candidate > 0 {
            omitted_line(omitted_after_candidate).len() + 1
        } else {
            0
        };
        let remaining_bytes = SUMMARY_MAX_BYTES.saturating_sub(reserved_bytes);
        let mut candidate_lines = lines.clone();
        candidate_lines.push(line);
        let candidate = candidate_lines.join("\n");
        if candidate.len() > remaining_bytes {
            truncated = true;
            break;
        }
        lines = candidate_lines;
    }

    let omitted_findings = findings.len() - lines.len();
    if omitted_findings > 0 {
        lines.push(omitted_line(omitted_findings));
    }
    let content = truncate_utf8(&lines.join("\n"), SUMMARY_MAX_BYTES);

    AutoReviewSummary {
        content,
        rendered_findings: lines
            .len()
            .saturating_sub(usize::from(omitted_findings > 0)),
        omitted_findings,
        truncated,
    }
}

fn omitted_line(count: usize) -> String {
    format!("{OMITTED_TEMPLATE_PREFIX}{count}{OMITTED_TEMPLATE_SUFFIX}")
}

fn validate_safe_id(value: &str) -> Result<()> {
    if value.is_empty() {
        anyhow::bail!("must be a non-empty string");
    }
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        && !value.trim_matches(['.', '-']).is_empty()
    {
        return Ok(());
    }
    anyhow::bail!("contains unsafe path characters: {value:?}")
}

fn validate_run(run: &AutoReviewRun) -> Result<()> {
    if run.schema_version != SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported auto review schema version: {}",
            run.schema_version
        );
    }
    validate_safe_id(&run.run_id).context("auto review run_id")?;

    let mut finding_ids = BTreeSet::new();
    for finding in &run.findings {
        validate_safe_id(&finding.finding_id).context("auto review finding_id")?;
        if !finding_ids.insert(&finding.finding_id) {
            anyhow::bail!("duplicate auto review finding id: {}", finding.finding_id);
        }
    }
    Ok(())
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

fn truncate_utf8_with_marker(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }
    (truncate_utf8(value, max_bytes), true)
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
