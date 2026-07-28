use std::fs;
use std::process::Command;
use std::time::Duration;
use std::time::SystemTime;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;
use crate::AutoReviewStore;
use crate::scoped_store_root;

fn temp_repo() -> TempDir {
    let repo = TempDir::new().expect("temp repo");
    Command::new("git")
        .current_dir(repo.path())
        .args(["init", "--quiet"])
        .status()
        .expect("git init");
    Command::new("git")
        .current_dir(repo.path())
        .args(["config", "user.email", "test@example.com"])
        .status()
        .expect("git config email");
    Command::new("git")
        .current_dir(repo.path())
        .args(["config", "user.name", "Test User"])
        .status()
        .expect("git config name");
    fs::write(repo.path().join("README.md"), "hello\n").expect("write readme");
    Command::new("git")
        .current_dir(repo.path())
        .args(["add", "README.md"])
        .status()
        .expect("git add");
    Command::new("git")
        .current_dir(repo.path())
        .args(["commit", "--quiet", "-m", "initial"])
        .status()
        .expect("git commit");
    repo
}

fn git_head(repo: &TempDir) -> String {
    let output = Command::new("git")
        .current_dir(repo.path())
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("utf8 head")
        .trim()
        .to_string()
}

#[test]
fn coordination_files_share_scoped_review_root_with_auto_review_store() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());

    assert_eq!(
        coordination.root(),
        scoped_store_root(home.path(), repo.path())
            .parent()
            .unwrap()
    );
    assert_eq!(
        coordination.lock_path(),
        coordination.root().join("review.lock")
    );
    assert_eq!(
        coordination.epoch_path(),
        coordination.root().join("snapshot.epoch")
    );
}

#[test]
fn clear_stale_lock_if_dead_is_noop_for_fresh_scope() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());

    assert!(!coordination.root().exists());
    assert!(
        !coordination
            .clear_stale_lock_if_dead()
            .expect("fresh scope should have no stale lock")
    );
    assert!(!coordination.root().exists());
}

#[test]
fn coordination_only_does_not_make_has_store_files_true() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());

    let guard = coordination
        .try_acquire_lock("coordination-only")
        .expect("lock attempt")
        .expect("lock acquired");
    coordination.bump_snapshot_epoch().expect("bump epoch");

    assert!(!AutoReviewStore::has_store_files(home.path()));
    drop(guard);
}

#[test]
fn scoped_locks_separate_repositories_under_one_codex_home() {
    let home = TempDir::new().expect("temp home");
    let repo_a = TempDir::new().expect("repo a");
    let repo_b = TempDir::new().expect("repo b");
    let coord_a = ReviewCoordination::for_scope(home.path(), repo_a.path());
    let coord_b = ReviewCoordination::for_scope(home.path(), repo_b.path());

    let guard_a = coord_a
        .try_acquire_lock("repo-a")
        .expect("lock repo a")
        .expect("repo a lock acquired");
    let guard_b = coord_b
        .try_acquire_lock("repo-b")
        .expect("lock repo b")
        .expect("repo b lock acquired");

    assert_ne!(coord_a.root(), coord_b.root());
    drop(guard_a);
    drop(guard_b);
}

#[test]
fn lock_contention_and_release() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());

    let guard = coordination
        .try_acquire_lock("first")
        .expect("first lock")
        .expect("first lock acquired");
    assert!(
        coordination
            .try_acquire_lock("second")
            .expect("second lock")
            .is_none()
    );

    drop(guard);
    assert!(
        coordination
            .try_acquire_lock("third")
            .expect("third lock")
            .is_some()
    );
}

#[test]
fn lock_records_intent_pid_head_and_epoch() {
    let home = TempDir::new().expect("temp home");
    let repo = temp_repo();
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());
    let epoch = coordination.bump_snapshot_epoch().expect("bump epoch");

    let guard = coordination
        .try_acquire_lock("record-info")
        .expect("lock")
        .expect("lock acquired");
    let info = coordination
        .read_lock_info()
        .expect("lock info")
        .expect("lock info exists");

    assert_eq!(info.pid, std::process::id());
    assert_eq!(info.intent, "record-info");
    assert_eq!(info.git_head, Some(git_head(&repo)));
    assert_eq!(info.snapshot_epoch, epoch);
    assert!(!info.owner_id.is_empty());
    drop(guard);
}

#[test]
fn epoch_bump_is_repo_scoped_and_monotonic() {
    let home = TempDir::new().expect("temp home");
    let repo_a = TempDir::new().expect("repo a");
    let repo_b = TempDir::new().expect("repo b");
    let coord_a = ReviewCoordination::for_scope(home.path(), repo_a.path());
    let coord_b = ReviewCoordination::for_scope(home.path(), repo_b.path());

    let a0 = coord_a.current_snapshot_epoch().expect("a0");
    let b0 = coord_b.current_snapshot_epoch().expect("b0");
    let a1 = coord_a.bump_snapshot_epoch().expect("a1");
    let a2 = coord_a.bump_snapshot_epoch().expect("a2");

    assert!(a1 > a0);
    assert!(a2 > a1);
    assert_eq!(coord_b.current_snapshot_epoch().expect("b current"), b0);
}

#[test]
fn publish_next_snapshot_epoch_after_does_not_advance_when_publish_fails() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());

    let result = coordination
        .publish_next_snapshot_epoch_after(|snapshot_epoch| {
            assert_eq!(snapshot_epoch, 1);
            false
        })
        .expect("publish callback result");

    assert_eq!(result, None);
    assert_eq!(
        coordination
            .current_snapshot_epoch()
            .expect("current epoch"),
        0
    );
}

#[test]
fn publish_next_snapshot_epoch_after_advances_after_publish_succeeds() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());

    let result = coordination
        .publish_next_snapshot_epoch_after(|snapshot_epoch| {
            assert_eq!(snapshot_epoch, 1);
            true
        })
        .expect("publish callback result");

    assert_eq!(result, Some(1));
    assert_eq!(
        coordination
            .current_snapshot_epoch()
            .expect("current epoch"),
        1
    );
}

#[test]
fn epoch_bump_waits_for_existing_epoch_lock() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());
    fs::create_dir_all(coordination.root()).expect("coordination root");
    let epoch_lock = coordination.root().join("snapshot.epoch.lock");
    fs::write(&epoch_lock, "busy").expect("write epoch lock");

    std::thread::scope(|scope| {
        let handle = scope.spawn(|| coordination.bump_snapshot_epoch().expect("bump epoch"));
        std::thread::sleep(Duration::from_millis(50));
        assert!(epoch_lock.exists());
        fs::remove_file(&epoch_lock).expect("release epoch lock");
        assert!(handle.join().expect("join epoch bump") > 0);
    });
}

#[test]
fn epoch_stale_cleanup_waits_for_cleanup_lock_before_removing() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());
    fs::create_dir_all(coordination.root()).expect("coordination root");
    let epoch_lock = coordination.root().join("snapshot.epoch.lock");
    let cleanup_lock = cleanup_lock_path(&epoch_lock);
    fs::write(&epoch_lock, "stale").expect("write stale epoch lock");
    let stale_time = SystemTime::now() - Duration::from_secs(EPOCH_LOCK_STALE_SECS + 1);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&epoch_lock)
        .expect("open epoch lock");
    file.set_modified(stale_time).expect("set stale mtime");
    fs::write(&cleanup_lock, "busy").expect("write cleanup lock");

    std::thread::scope(|scope| {
        let handle = scope.spawn(|| coordination.bump_snapshot_epoch().expect("bump epoch"));
        std::thread::sleep(Duration::from_millis(50));
        fs::write(&epoch_lock, "fresh").expect("replace epoch lock");
        fs::remove_file(&cleanup_lock).expect("release cleanup lock");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            fs::read_to_string(&epoch_lock).expect("epoch lock"),
            "fresh"
        );
        fs::remove_file(&epoch_lock).expect("release epoch lock");
        assert!(handle.join().expect("join epoch bump") > 0);
    });
}

#[test]
fn epoch_bump_clears_stale_epoch_lock() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());
    fs::create_dir_all(coordination.root()).expect("coordination root");
    let epoch_lock = coordination.root().join("snapshot.epoch.lock");
    fs::write(&epoch_lock, "stale").expect("write epoch lock");
    let stale_time = SystemTime::now() - Duration::from_secs(EPOCH_LOCK_STALE_SECS + 1);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&epoch_lock)
        .expect("open epoch lock");
    file.set_modified(stale_time).expect("set stale mtime");

    let epoch = coordination.bump_snapshot_epoch().expect("bump epoch");

    assert!(epoch > 0);
    assert!(!epoch_lock.exists());
}

#[test]
fn lock_info_survives_epoch_bump_for_stale_detection() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());

    let guard = coordination
        .try_acquire_lock("stale-check")
        .expect("lock")
        .expect("lock acquired");
    let initial = coordination
        .read_lock_info()
        .expect("lock info")
        .expect("lock info exists");
    let current = coordination.bump_snapshot_epoch().expect("bump epoch");
    let still_recorded = coordination
        .read_lock_info()
        .expect("lock info")
        .expect("lock info exists");

    assert_eq!(still_recorded.snapshot_epoch, initial.snapshot_epoch);
    assert!(current > initial.snapshot_epoch);
    drop(guard);
}

#[test]
fn guard_drop_does_not_remove_replaced_lock() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());

    let guard = coordination
        .try_acquire_lock("old")
        .expect("lock")
        .expect("lock acquired");
    let replacement = ReviewLockInfo {
        pid: std::process::id(),
        started_at_unix_secs: 1,
        intent: "new".to_string(),
        git_head: None,
        snapshot_epoch: 0,
        owner_id: "different-owner".to_string(),
    };
    fs::write(
        coordination.lock_path(),
        serde_json::to_string_pretty(&replacement).expect("serialize replacement"),
    )
    .expect("write replacement lock");

    drop(guard);

    let info = coordination
        .read_lock_info()
        .expect("lock info")
        .expect("replacement remains");
    assert_eq!(info.owner_id, "different-owner");
}

#[test]
fn malformed_lock_is_not_cleared() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());
    fs::create_dir_all(coordination.root()).expect("coordination root");
    fs::write(coordination.lock_path(), "not json").expect("write malformed lock");

    assert!(coordination.read_lock_info().is_err());
    assert!(
        !coordination
            .clear_stale_lock_if_dead()
            .expect("clear stale lock")
    );
    assert!(coordination.lock_path().exists());
    assert!(
        coordination
            .try_acquire_lock("blocked")
            .expect("lock attempt")
            .is_none()
    );
}

#[test]
fn stale_malformed_lock_is_cleared() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());
    fs::create_dir_all(coordination.root()).expect("coordination root");
    fs::write(coordination.lock_path(), "not json").expect("write malformed lock");
    let stale_time = SystemTime::now() - Duration::from_secs(MALFORMED_LOCK_STALE_SECS + 1);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(coordination.lock_path())
        .expect("open malformed lock");
    file.set_modified(stale_time).expect("set stale mtime");

    assert!(
        coordination
            .clear_stale_lock_if_dead()
            .expect("clear stale lock")
    );
    assert!(!coordination.lock_path().exists());
}

#[test]
fn acquisition_clears_stale_malformed_lock() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());
    fs::create_dir_all(coordination.root()).expect("coordination root");
    fs::write(coordination.lock_path(), "not json").expect("write malformed lock");
    let stale_time = SystemTime::now() - Duration::from_secs(MALFORMED_LOCK_STALE_SECS + 1);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(coordination.lock_path())
        .expect("open malformed lock");
    file.set_modified(stale_time).expect("set stale mtime");

    let guard = coordination
        .try_acquire_lock("after-stale-malformed")
        .expect("lock attempt")
        .expect("lock acquired");

    drop(guard);
}

#[test]
fn review_stale_cleanup_waits_for_cleanup_lock_before_removing() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());
    fs::create_dir_all(coordination.root()).expect("coordination root");
    let cleanup_lock = cleanup_lock_path(&coordination.lock_path());
    fs::write(coordination.lock_path(), "not json").expect("write stale malformed lock");
    let stale_time = SystemTime::now() - Duration::from_secs(MALFORMED_LOCK_STALE_SECS + 1);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(coordination.lock_path())
        .expect("open malformed lock");
    file.set_modified(stale_time).expect("set stale mtime");
    fs::write(&cleanup_lock, "busy").expect("write cleanup lock");

    std::thread::scope(|scope| {
        let handle = scope.spawn(|| coordination.clear_stale_lock_if_dead());
        std::thread::sleep(Duration::from_millis(50));
        fs::write(coordination.lock_path(), "fresh").expect("replace review lock");
        fs::remove_file(&cleanup_lock).expect("release cleanup lock");
        assert!(
            !handle
                .join()
                .expect("join cleanup")
                .expect("cleanup result")
        );
    });
    assert_eq!(
        fs::read_to_string(coordination.lock_path()).expect("review lock"),
        "fresh"
    );
}

#[test]
fn lock_creation_failure_removes_partial_lock() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());
    fs::create_dir_all(coordination.root()).expect("coordination root");
    fs::write(coordination.epoch_path(), "not a number").expect("write malformed epoch");

    assert!(coordination.try_acquire_lock("bad-epoch").is_err());
    assert!(!coordination.lock_path().exists());
}

#[test]
#[cfg(unix)]
fn valid_lock_with_dead_pid_is_cleared() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());
    fs::create_dir_all(coordination.root()).expect("coordination root");
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn child");
    let pid = child.id();
    child.wait().expect("wait child");
    assert!(!pid_alive(pid));
    let info = ReviewLockInfo {
        pid,
        started_at_unix_secs: 1,
        intent: "dead".to_string(),
        git_head: None,
        snapshot_epoch: 0,
        owner_id: "dead-owner".to_string(),
    };
    fs::write(
        coordination.lock_path(),
        serde_json::to_string_pretty(&info).expect("serialize lock"),
    )
    .expect("write lock");

    assert!(
        coordination
            .clear_stale_lock_if_dead()
            .expect("clear stale lock")
    );
    assert!(!coordination.lock_path().exists());
}

#[test]
fn valid_lock_with_live_pid_is_not_cleared() {
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let coordination = ReviewCoordination::for_scope(home.path(), repo.path());
    fs::create_dir_all(coordination.root()).expect("coordination root");
    assert!(pid_alive(std::process::id()));
    let info = ReviewLockInfo {
        pid: std::process::id(),
        started_at_unix_secs: 1,
        intent: "live".to_string(),
        git_head: None,
        snapshot_epoch: 0,
        owner_id: "live-owner".to_string(),
    };
    fs::write(
        coordination.lock_path(),
        serde_json::to_string_pretty(&info).expect("serialize lock"),
    )
    .expect("write lock");

    assert!(
        !coordination
            .clear_stale_lock_if_dead()
            .expect("clear stale lock")
    );
    assert!(coordination.lock_path().exists());
}
