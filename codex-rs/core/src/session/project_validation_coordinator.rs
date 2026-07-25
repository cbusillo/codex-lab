use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Weak;

use tokio::sync::Mutex;
use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::CancellationToken;
use tracing::debug;

#[derive(Default)]
pub(crate) struct ProjectValidationCoordinator {
    leases: Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>,
}

pub(crate) struct ProjectValidationLeaseGuard {
    _guard: OwnedMutexGuard<()>,
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
}
