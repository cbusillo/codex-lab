use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Weak;

use tokio::sync::Mutex;
use tokio::sync::OwnedMutexGuard;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::debug;

const MAX_CONCURRENT_CARGO_VALIDATIONS: usize = 2;
const MAX_SUCCESSFUL_VALIDATION_KEYS: usize = 64;

pub(crate) struct ProjectValidationCoordinator {
    leases: Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>,
    cargo_permits: Arc<Semaphore>,
    successful_validations: Mutex<VecDeque<ProjectValidationSuccessKey>>,
}

pub(crate) struct ProjectValidationLeaseGuard {
    _guard: OwnedMutexGuard<()>,
}

pub(crate) struct ProjectValidationCargoPermit {
    _permit: OwnedSemaphorePermit,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ProjectValidationSuccessKey {
    repository_root: PathBuf,
    validation_scope: PathBuf,
    head_commit: String,
    worktree_diff: Option<String>,
    command: Vec<String>,
}

impl ProjectValidationSuccessKey {
    pub(crate) fn new(
        repository_root: PathBuf,
        validation_scope: PathBuf,
        head_commit: String,
        worktree_diff: Option<String>,
        command: Vec<String>,
    ) -> Self {
        Self {
            repository_root,
            validation_scope,
            head_commit,
            worktree_diff,
            command,
        }
    }
}

impl Default for ProjectValidationCoordinator {
    fn default() -> Self {
        Self {
            leases: Mutex::new(HashMap::new()),
            cargo_permits: Arc::new(Semaphore::new(MAX_CONCURRENT_CARGO_VALIDATIONS)),
            successful_validations: Mutex::new(VecDeque::new()),
        }
    }
}

impl ProjectValidationCoordinator {
    pub(crate) async fn acquire(
        &self,
        repo_root: PathBuf,
        cancellation_token: &CancellationToken,
    ) -> Option<ProjectValidationLeaseGuard> {
        if cancellation_token.is_cancelled() {
            return None;
        }
        let lease = {
            let mut leases = self.leases.lock().await;
            leases.retain(|_, lease| lease.strong_count() > 0);
            if let Some(lease) = leases.get(&repo_root).and_then(Weak::upgrade) {
                lease
            } else {
                let lease = Arc::new(Mutex::new(()));
                leases.insert(repo_root.clone(), Arc::downgrade(&lease));
                lease
            }
        };

        if let Ok(guard) = Arc::clone(&lease).try_lock_owned() {
            return Some(ProjectValidationLeaseGuard { _guard: guard });
        }
        debug!(
            repo_root = %repo_root.display(),
            "project validation waiting for repository lease"
        );

        tokio::select! {
            biased;
            _ = cancellation_token.cancelled() => None,
            guard = lease.lock_owned() => Some(ProjectValidationLeaseGuard { _guard: guard }),
        }
    }

    pub(crate) async fn acquire_cargo(
        &self,
        cancellation_token: &CancellationToken,
    ) -> Option<ProjectValidationCargoPermit> {
        if cancellation_token.is_cancelled() {
            return None;
        }
        let permits = Arc::clone(&self.cargo_permits);
        if let Ok(permit) = Arc::clone(&permits).try_acquire_owned() {
            return Some(ProjectValidationCargoPermit { _permit: permit });
        }
        debug!("cargo validation waiting for global concurrency permit");
        tokio::select! {
            biased;
            _ = cancellation_token.cancelled() => None,
            permit = permits.acquire_owned() => permit.ok().map(|permit| ProjectValidationCargoPermit { _permit: permit }),
        }
    }

    pub(crate) async fn has_successful_validation(
        &self,
        key: &ProjectValidationSuccessKey,
    ) -> bool {
        self.successful_validations.lock().await.contains(key)
    }

    pub(crate) async fn record_successful_validation(&self, key: ProjectValidationSuccessKey) {
        let mut successful_validations = self.successful_validations.lock().await;
        if let Some(index) = successful_validations
            .iter()
            .position(|existing| existing == &key)
        {
            successful_validations.remove(index);
        }
        successful_validations.push_back(key);
        while successful_validations.len() > MAX_SUCCESSFUL_VALIDATION_KEYS {
            successful_validations.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::oneshot;
    use tokio::time::timeout;

    use super::*;

    async fn wait_for_lease_strong_count(
        coordinator: &ProjectValidationCoordinator,
        repo_root: &PathBuf,
        minimum: usize,
    ) {
        timeout(Duration::from_secs(1), async {
            loop {
                let strong_count = coordinator
                    .leases
                    .lock()
                    .await
                    .get(repo_root)
                    .map(Weak::strong_count)
                    .unwrap_or_default();
                if strong_count >= minimum {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("lease strong count should reach expected minimum");
    }

    #[tokio::test]
    async fn same_repo_waits_until_lease_is_released() {
        let coordinator = Arc::new(ProjectValidationCoordinator::default());
        let cancellation_token = CancellationToken::new();
        let repo_root = PathBuf::from("repo");
        let first = coordinator
            .acquire(repo_root.clone(), &cancellation_token)
            .await
            .expect("first lease should be acquired");
        let (acquired_tx, mut acquired_rx) = oneshot::channel();
        let coordinator_for_waiter = Arc::clone(&coordinator);
        let repo_root_for_waiter = repo_root.clone();
        let waiter = tokio::spawn(async move {
            let lease = coordinator_for_waiter
                .acquire(repo_root_for_waiter, &CancellationToken::new())
                .await;
            let _ = acquired_tx.send(());
            lease
        });

        wait_for_lease_strong_count(&coordinator, &repo_root, /*minimum*/ 2).await;
        assert!(acquired_rx.try_recv().is_err());

        drop(first);
        timeout(Duration::from_secs(1), acquired_rx)
            .await
            .expect("waiter should acquire after release")
            .expect("waiter sender should remain available");
        assert!(waiter.await.expect("waiter task should complete").is_some());
    }

    #[tokio::test]
    async fn cancellation_stops_waiting_without_staling_repo() {
        let coordinator = Arc::new(ProjectValidationCoordinator::default());
        let owner_cancellation = CancellationToken::new();
        let repo_root = PathBuf::from("repo");
        let first = coordinator
            .acquire(repo_root.clone(), &owner_cancellation)
            .await
            .expect("first lease should be acquired");
        let waiter_cancellation = CancellationToken::new();
        let coordinator_for_waiter = Arc::clone(&coordinator);
        let waiter_cancellation_for_task = waiter_cancellation.clone();
        let repo_root_for_waiter = repo_root.clone();
        let waiter = tokio::spawn(async move {
            coordinator_for_waiter
                .acquire(repo_root_for_waiter, &waiter_cancellation_for_task)
                .await
        });

        wait_for_lease_strong_count(&coordinator, &repo_root, /*minimum*/ 2).await;
        waiter_cancellation.cancel();
        assert!(waiter.await.expect("waiter task should complete").is_none());
        drop(first);

        let next = coordinator
            .acquire(repo_root, &CancellationToken::new())
            .await;
        assert!(next.is_some());
    }

    #[tokio::test]
    async fn different_repos_acquire_independently() {
        let coordinator = ProjectValidationCoordinator::default();
        let cancellation_token = CancellationToken::new();
        let _first = coordinator
            .acquire(PathBuf::from("repo-a"), &cancellation_token)
            .await
            .expect("first lease should be acquired");
        let second = timeout(
            Duration::from_secs(1),
            coordinator.acquire(PathBuf::from("repo-b"), &cancellation_token),
        )
        .await
        .expect("different repo should not wait");
        assert!(second.is_some());
    }

    #[tokio::test]
    async fn cargo_concurrency_is_globally_bounded_and_cancellable() {
        let coordinator = Arc::new(ProjectValidationCoordinator::default());
        let first = coordinator
            .acquire_cargo(&CancellationToken::new())
            .await
            .expect("first cargo permit should be acquired");
        let second = coordinator
            .acquire_cargo(&CancellationToken::new())
            .await
            .expect("second cargo permit should be acquired");
        let cancellation_token = CancellationToken::new();
        let coordinator_for_waiter = Arc::clone(&coordinator);
        let cancellation_for_waiter = cancellation_token.clone();
        let waiter = tokio::spawn(async move {
            coordinator_for_waiter
                .acquire_cargo(&cancellation_for_waiter)
                .await
        });

        tokio::task::yield_now().await;
        cancellation_token.cancel();
        assert!(
            waiter
                .await
                .expect("cargo waiter should complete")
                .is_none()
        );

        drop(first);
        drop(second);
        assert!(
            coordinator
                .acquire_cargo(&CancellationToken::new())
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn successful_validation_cache_is_bounded_and_refreshes_duplicates() {
        let coordinator = ProjectValidationCoordinator::default();
        for index in 0..=MAX_SUCCESSFUL_VALIDATION_KEYS {
            coordinator
                .record_successful_validation(ProjectValidationSuccessKey::new(
                    PathBuf::from("repo"),
                    PathBuf::from("workspace"),
                    format!("head-{index}"),
                    Some(format!("diff-{index}")),
                    vec!["cargo".to_string(), "check".to_string()],
                ))
                .await;
        }
        assert_eq!(
            coordinator.successful_validations.lock().await.len(),
            MAX_SUCCESSFUL_VALIDATION_KEYS
        );

        let newest = ProjectValidationSuccessKey::new(
            PathBuf::from("repo"),
            PathBuf::from("workspace"),
            format!("head-{MAX_SUCCESSFUL_VALIDATION_KEYS}"),
            Some(format!("diff-{MAX_SUCCESSFUL_VALIDATION_KEYS}")),
            vec!["cargo".to_string(), "check".to_string()],
        );
        assert!(coordinator.has_successful_validation(&newest).await);
        coordinator
            .record_successful_validation(newest.clone())
            .await;
        assert_eq!(
            coordinator.successful_validations.lock().await.back(),
            Some(&newest)
        );
    }
}
