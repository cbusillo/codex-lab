use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;

use codex_auto_review::AutoReviewDuplicateMatch;
use codex_auto_review::AutoReviewRun;
use codex_auto_review::AutoReviewRunFreshness;
use codex_auto_review::AutoReviewRunSource;
use codex_auto_review::AutoReviewRunStatus;
use codex_auto_review::AutoReviewRunTarget;
use codex_auto_review::AutoReviewStore;
use codex_auto_review::ReviewCoordination;
use codex_auto_review::SCHEMA_VERSION;
use codex_auto_review::finding_digests;
use codex_git_utils::collect_git_info;
use codex_git_utils::get_git_repo_root;
use codex_git_utils::get_worktree_diff_fingerprint;
use codex_git_utils::merge_base_with_head;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewPersistence;
use codex_protocol::protocol::ReviewTarget;

use crate::turn_timing::now_unix_timestamp_ms;

static AUTO_REVIEW_RUN_WRITE_LOCK: Mutex<()> = Mutex::new(());

pub(crate) const AUTO_REVIEW_INTERRUPTED_ERROR_SUMMARY: &str =
    "review was interrupted before producing structured output";

#[derive(Clone, Debug)]
pub(crate) struct ReviewPersistenceContext {
    run_id: String,
    source: AutoReviewRunSource,
    store_scope: PathBuf,
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
        codex_home: &Path,
        cwd: &Path,
        model: Option<String>,
    ) -> Self {
        let target = collect_auto_review_target(codex_home, cwd, &review_target).await;
        let store_scope = target
            .worktree_path
            .clone()
            .unwrap_or_else(|| cwd.to_path_buf());
        Self {
            run_id,
            source: match mode {
                ReviewPersistence::ManualAutoReview => AutoReviewRunSource::Manual,
                ReviewPersistence::BackgroundAutoReview => AutoReviewRunSource::Background,
            },
            store_scope,
            target,
            review_target,
            started_at_unix_secs: now_unix_secs(),
            model,
        }
    }

    pub(crate) fn is_manual(&self) -> bool {
        self.source == AutoReviewRunSource::Manual
    }

    pub(crate) fn is_background(&self) -> bool {
        self.source == AutoReviewRunSource::Background
    }

    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }

    pub(crate) fn store_scope(&self) -> &Path {
        &self.store_scope
    }

    pub(crate) fn target(&self) -> &AutoReviewRunTarget {
        &self.target
    }

    pub(crate) fn with_snapshot_epoch(mut self, snapshot_epoch: u64) -> Self {
        self.target.snapshot_epoch = Some(snapshot_epoch);
        self
    }

    pub(crate) fn worktree_diff_fingerprint(&self) -> Option<&str> {
        self.target.worktree_diff_fingerprint.as_deref()
    }

    pub(crate) fn review_target(&self) -> &ReviewTarget {
        &self.review_target
    }

    pub(crate) fn save_running(&self, codex_home: impl AsRef<Path>) -> bool {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Running,
            /*output*/ None,
            /*error_summary*/ None,
        )
    }

    pub(crate) fn save_pending(&self, codex_home: impl AsRef<Path>) -> bool {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Pending,
            /*output*/ None,
            /*error_summary*/ None,
        )
    }

    pub(crate) fn save_completed(
        &self,
        codex_home: impl AsRef<Path>,
        output: &ReviewOutputEvent,
    ) -> bool {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Completed,
            Some(output),
            None,
        )
    }

    pub(crate) fn save_cancelled(&self, codex_home: impl AsRef<Path>) -> bool {
        self.save_cancelled_with_summary(
            codex_home,
            AUTO_REVIEW_INTERRUPTED_ERROR_SUMMARY.to_string(),
        )
    }

    pub(crate) fn save_cancelled_with_summary(
        &self,
        codex_home: impl AsRef<Path>,
        error_summary: String,
    ) -> bool {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Cancelled,
            /*output*/ None,
            Some(error_summary),
        )
    }

    pub(crate) fn save_superseded_with_summary(
        &self,
        codex_home: impl AsRef<Path>,
        error_summary: String,
        superseded_by: Option<String>,
    ) -> bool {
        self.save_run_with_metadata(
            codex_home,
            AutoReviewRunStatus::Superseded,
            /*output*/ None,
            Some(error_summary),
            AutoReviewRunFreshness::Superseded,
            superseded_by,
            None,
        )
    }

    pub(crate) fn save_failed(&self, codex_home: impl AsRef<Path>, error_summary: String) -> bool {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Failed,
            /*output*/ None,
            Some(error_summary),
        )
    }

    pub(crate) fn save_skipped(&self, codex_home: impl AsRef<Path>, error_summary: String) -> bool {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Skipped,
            /*output*/ None,
            Some(error_summary),
        )
    }

    pub(crate) fn save_skipped_duplicate(
        &self,
        codex_home: impl AsRef<Path>,
        duplicate: &AutoReviewDuplicateMatch,
    ) -> bool {
        let error_summary = format!(
            "equivalent background auto review already exists: {}",
            duplicate.run_id
        );
        self.save_run_with_metadata(
            codex_home,
            AutoReviewRunStatus::Skipped,
            /*output*/ None,
            Some(error_summary),
            AutoReviewRunFreshness::Superseded,
            Some(duplicate.run_id.clone()),
            Some("duplicate_auto_review_scope".to_string()),
        )
    }

    fn save_run(
        &self,
        codex_home: impl AsRef<Path>,
        status: AutoReviewRunStatus,
        output: Option<&ReviewOutputEvent>,
        error_summary: Option<String>,
    ) -> bool {
        self.save_run_with_metadata(
            codex_home,
            status,
            output,
            error_summary,
            AutoReviewRunFreshness::Current,
            None,
            None,
        )
    }

    fn save_run_with_metadata(
        &self,
        codex_home: impl AsRef<Path>,
        status: AutoReviewRunStatus,
        output: Option<&ReviewOutputEvent>,
        error_summary: Option<String>,
        freshness: AutoReviewRunFreshness,
        superseded_by: Option<String>,
        cancel_reason: Option<String>,
    ) -> bool {
        let codex_home = codex_home.as_ref();
        let completed_at_unix_secs = if is_terminal_status(&status) {
            Some(now_unix_secs())
        } else {
            None
        };
        let store = AutoReviewStore::for_scope(codex_home, &self.store_scope);
        let _guard = AUTO_REVIEW_RUN_WRITE_LOCK.lock().unwrap_or_else(|err| {
            tracing::warn!("auto review run write lock was poisoned; continuing");
            err.into_inner()
        });
        if let Ok(existing) = store.load_run(&self.run_id)
            && is_terminal_status(&existing.status)
        {
            if !is_terminal_status(&status) {
                tracing::debug!(
                    run_id = %self.run_id,
                    existing_status = ?existing.status,
                    "skipping non-terminal auto review run write after terminal status"
                );
            }
            return false;
        }
        if let Err(err) = store.list_runs() {
            tracing::warn!(
                run_id = %self.run_id,
                error = %err,
                "failed to read auto review runs before persisting output"
            );
            return false;
        }
        if let Some(output) = output
            && let Err(err) = store.save_output(&self.run_id, output)
        {
            tracing::warn!(
                run_id = %self.run_id,
                error = %err,
                "failed to persist auto review output"
            );
            return false;
        }
        let finding_count = output
            .map(|output| output.findings.len())
            .unwrap_or_default();
        let finding_digests = output.map(finding_digests).unwrap_or_default();
        let omitted_finding_digest_count = finding_count.saturating_sub(finding_digests.len());
        let run = AutoReviewRun {
            schema_version: SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            status,
            freshness,
            source: self.source.clone(),
            target: self.target.clone(),
            review_target: self.review_target.clone(),
            started_at_unix_secs: self.started_at_unix_secs,
            completed_at_unix_secs,
            model: self.model.clone(),
            superseded_by,
            cancel_reason,
            error_summary,
            finding_count,
            finding_digests,
            omitted_finding_digest_count,
        };
        if let Err(err) = store.save_run(&run) {
            tracing::warn!(
                run_id = %self.run_id,
                error = %err,
                "failed to persist auto review run"
            );
            return false;
        }
        true
    }
}

fn is_terminal_status(status: &AutoReviewRunStatus) -> bool {
    status.is_terminal()
}

pub(crate) async fn collect_auto_review_target(
    codex_home: &Path,
    cwd: &Path,
    review_target: &ReviewTarget,
) -> AutoReviewRunTarget {
    let git_info = collect_git_info(cwd).await;
    let repo_root = get_git_repo_root(cwd);
    let store_scope = repo_root.as_deref().unwrap_or(cwd);
    let snapshot_epoch =
        match ReviewCoordination::for_scope(codex_home, store_scope).current_snapshot_epoch() {
            Ok(0) => None,
            Ok(epoch) => Some(epoch),
            Err(err) => {
                tracing::warn!(error = %err, "failed to collect auto review snapshot epoch");
                None
            }
        };
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

    let worktree_diff_fingerprint = match review_target {
        ReviewTarget::UncommittedChanges | ReviewTarget::CurrentTurnDiff { .. } => {
            get_worktree_diff_fingerprint(cwd).await
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
        snapshot_epoch,
        snapshot_commit: git_info
            .as_ref()
            .and_then(|git| git.commit_hash.as_ref().map(|sha| sha.0.clone())),
        head_at_launch: git_info
            .as_ref()
            .and_then(|git| git.commit_hash.as_ref().map(|sha| sha.0.clone())),
        worktree_diff_fingerprint,
    }
}

fn now_unix_secs() -> i64 {
    now_unix_timestamp_ms() / 1000
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::ReviewPersistence;
    use tempfile::TempDir;

    #[tokio::test]
    async fn save_running_does_not_overwrite_terminal_run() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "late-running".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
        )
        .await;

        persistence.save_cancelled(codex_home.path());
        persistence.save_running(codex_home.path());

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("late-running")
            .expect("load persisted review run");
        assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
    }

    #[tokio::test]
    async fn save_cancelled_with_summary_records_custom_reason() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "custom-cancelled".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
        )
        .await;

        persistence.save_cancelled_with_summary(
            codex_home.path(),
            "background auto review was cancelled by request".to_string(),
        );

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("custom-cancelled")
            .expect("load persisted review run");
        assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
        assert_eq!(
            run.error_summary.as_deref(),
            Some("background auto review was cancelled by request")
        );
    }

    #[tokio::test]
    async fn save_superseded_with_summary_records_supersede_metadata() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "custom-superseded".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
        )
        .await;

        persistence.save_superseded_with_summary(
            codex_home.path(),
            "background auto review was superseded by run replacement-run".to_string(),
            Some("replacement-run".to_string()),
        );
        persistence.save_running(codex_home.path());

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("custom-superseded")
            .expect("load persisted review run");
        assert_eq!(run.status, AutoReviewRunStatus::Superseded);
        assert_eq!(run.freshness, AutoReviewRunFreshness::Superseded);
        assert_eq!(run.superseded_by.as_deref(), Some("replacement-run"));
        assert!(run.completed_at_unix_secs.is_some());
        assert_eq!(
            run.error_summary.as_deref(),
            Some("background auto review was superseded by run replacement-run")
        );
    }

    #[tokio::test]
    async fn save_pending_can_advance_to_running() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "pending-running".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
        )
        .await;

        persistence.save_pending(codex_home.path());
        persistence.save_running(codex_home.path());

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("pending-running")
            .expect("load persisted review run");
        assert_eq!(run.status, AutoReviewRunStatus::Running);
        assert_eq!(run.completed_at_unix_secs, None);
    }

    #[tokio::test]
    async fn save_skipped_blocks_late_running() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "skipped-running".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
        )
        .await;

        persistence.save_skipped(codex_home.path(), "duplicate fingerprint".to_string());
        persistence.save_running(codex_home.path());

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("skipped-running")
            .expect("load persisted review run");
        assert_eq!(run.status, AutoReviewRunStatus::Skipped);
        assert_eq!(run.error_summary.as_deref(), Some("duplicate fingerprint"));
    }

    #[tokio::test]
    async fn save_skipped_duplicate_records_duplicate_metadata() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "duplicate-candidate".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
        )
        .await;
        let duplicate = AutoReviewDuplicateMatch {
            run_id: "existing-run".to_string(),
            status: AutoReviewRunStatus::Completed,
            disposition: codex_auto_review::AutoReviewDuplicateDisposition::ReuseTerminal,
            finding_count: 1,
            model: Some("test-model".to_string()),
        };

        persistence.save_skipped_duplicate(codex_home.path(), &duplicate);

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("duplicate-candidate")
            .expect("load persisted duplicate skip");
        assert_eq!(run.status, AutoReviewRunStatus::Skipped);
        assert_eq!(run.freshness, AutoReviewRunFreshness::Superseded);
        assert_eq!(run.superseded_by.as_deref(), Some("existing-run"));
        assert_eq!(
            run.cancel_reason.as_deref(),
            Some("duplicate_auto_review_scope")
        );
        assert_eq!(
            run.error_summary.as_deref(),
            Some("equivalent background auto review already exists: existing-run")
        );
    }

    #[tokio::test]
    async fn collect_target_records_current_snapshot_epoch() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");

        let target = collect_auto_review_target(
            codex_home.path(),
            cwd.path(),
            &ReviewTarget::UncommittedChanges,
        )
        .await;
        assert_eq!(target.snapshot_epoch, None);

        ReviewCoordination::for_scope(codex_home.path(), cwd.path())
            .bump_snapshot_epoch()
            .expect("bump snapshot epoch");

        let target = collect_auto_review_target(
            codex_home.path(),
            cwd.path(),
            &ReviewTarget::UncommittedChanges,
        )
        .await;

        assert_eq!(target.snapshot_epoch, Some(1));
    }

    #[tokio::test]
    async fn current_turn_diff_target_records_live_worktree_fingerprint() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        std::process::Command::new("git")
            .arg("init")
            .current_dir(cwd.path())
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(cwd.path())
            .output()
            .expect("git config user.email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(cwd.path())
            .output()
            .expect("git config user.name");
        std::fs::write(cwd.path().join("tracked.txt"), "base\n").expect("write base file");
        std::process::Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(cwd.path())
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(cwd.path())
            .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00Z")
            .output()
            .expect("git commit");
        std::fs::write(cwd.path().join("tracked.txt"), "base\nchange\n").expect("write dirty file");
        let expected = get_worktree_diff_fingerprint(cwd.path())
            .await
            .expect("dirty worktree fingerprint");

        let target = collect_auto_review_target(
            codex_home.path(),
            cwd.path(),
            &ReviewTarget::CurrentTurnDiff {
                fingerprint: "sha256:synthetic-turn-diff".to_string(),
            },
        )
        .await;

        assert_eq!(
            target.worktree_diff_fingerprint.as_deref(),
            Some(expected.as_str())
        );
    }
}
