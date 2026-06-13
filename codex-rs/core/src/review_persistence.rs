use std::path::Path;
use std::path::PathBuf;

use codex_auto_review::AutoReviewFindingRecord;
use codex_auto_review::AutoReviewRun;
use codex_auto_review::AutoReviewRunSource;
use codex_auto_review::AutoReviewRunStatus;
use codex_auto_review::AutoReviewRunTarget;
use codex_auto_review::AutoReviewStore;
use codex_auto_review::SCHEMA_VERSION;
use codex_git_utils::collect_git_info;
use codex_git_utils::get_git_repo_root;
use codex_git_utils::merge_base_with_head;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewPersistence;
use codex_protocol::protocol::ReviewTarget;

use crate::turn_timing::now_unix_timestamp_ms;

#[derive(Clone, Debug)]
pub(crate) struct ReviewPersistenceContext {
    run_id: String,
    source: AutoReviewRunSource,
    target: AutoReviewRunTarget,
    review_target: ReviewTarget,
    started_at_unix_secs: i64,
    model: Option<String>,
}

impl ReviewPersistenceContext {
    pub(crate) async fn new(
        run_id: String,
        mode: ReviewPersistence,
        review_target: ReviewTarget,
        cwd: &Path,
        model: Option<String>,
    ) -> Self {
        let target = collect_target(cwd, &review_target).await;
        Self {
            run_id,
            source: match mode {
                ReviewPersistence::ManualAutoReview => AutoReviewRunSource::Manual,
            },
            target,
            review_target,
            started_at_unix_secs: now_unix_secs(),
            model,
        }
    }

    pub(crate) fn save_completed(&self, codex_home: impl AsRef<Path>, output: &ReviewOutputEvent) {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Completed,
            Some(output),
            None,
        );
    }

    pub(crate) fn save_cancelled(&self, codex_home: impl AsRef<Path>) {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Cancelled,
            /*output*/ None,
            Some("review was interrupted before producing structured output".to_string()),
        );
    }

    pub(crate) fn save_failed(&self, codex_home: impl AsRef<Path>, error_summary: String) {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Failed,
            /*output*/ None,
            Some(error_summary),
        );
    }

    fn save_run(
        &self,
        codex_home: impl AsRef<Path>,
        status: AutoReviewRunStatus,
        output: Option<&ReviewOutputEvent>,
        error_summary: Option<String>,
    ) {
        let store = AutoReviewStore::new(codex_home);
        let run = AutoReviewRun {
            schema_version: SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            status,
            source: self.source.clone(),
            target: self.target.clone(),
            review_target: self.review_target.clone(),
            started_at_unix_secs: self.started_at_unix_secs,
            completed_at_unix_secs: Some(now_unix_secs()),
            model: self.model.clone(),
            error_summary,
            findings: output
                .map(|output| finding_records(&output.findings))
                .unwrap_or_default(),
        };
        if let Err(err) = store.save_run(&run) {
            tracing::warn!(
                run_id = %self.run_id,
                error = %err,
                "failed to persist auto review run"
            );
        }
    }
}

async fn collect_target(cwd: &Path, review_target: &ReviewTarget) -> AutoReviewRunTarget {
    let git_info = collect_git_info(cwd).await;
    let repo_root = get_git_repo_root(cwd);
    let base_sha = match (repo_root.as_deref(), review_target) {
        (Some(repo_root), ReviewTarget::BaseBranch { branch }) => {
            match merge_base_with_head(repo_root, branch) {
                Ok(base_sha) => base_sha,
                Err(err) => {
                    tracing::warn!(
                        branch,
                        error = %err,
                        "failed to collect auto review base sha"
                    );
                    None
                }
            }
        }
        _ => None,
    };

    AutoReviewRunTarget {
        branch: git_info.as_ref().and_then(|git| git.branch.clone()),
        head_sha: git_info
            .as_ref()
            .and_then(|git| git.commit_hash.as_ref().map(|sha| sha.0.clone())),
        base_sha,
        worktree_path: repo_root.or_else(|| Some(PathBuf::from(cwd))),
    }
}

fn finding_records(
    findings: &[codex_protocol::protocol::ReviewFinding],
) -> Vec<AutoReviewFindingRecord> {
    findings
        .iter()
        .enumerate()
        .map(|(index, finding)| AutoReviewFindingRecord {
            finding_id: format!("f{}", index + 1),
            finding: finding.clone(),
        })
        .collect()
}

fn now_unix_secs() -> i64 {
    now_unix_timestamp_ms() / 1000
}
