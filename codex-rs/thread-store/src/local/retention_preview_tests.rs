use std::fs;
use std::fs::FileTimes;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use codex_protocol::ThreadId;
use codex_protocol::protocol::HistoryPosition;
use codex_rollout::RolloutCompressionMode;
use codex_rollout::RolloutLease;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::ThreadStore;
use crate::local::LocalThreadStore;
use crate::local::test_support::test_config;
use crate::local::test_support::write_archived_session_file;
use crate::local::test_support::write_session_file;

#[tokio::test]
async fn empty_preview_does_not_create_storage_or_lock_state() {
    let home = TempDir::new().expect("temp dir");
    let store = enabled_store(home.path());

    let report = store
        .retention_preview(preview_params(25, None))
        .await
        .expect("empty preview");

    assert_eq!(
        report,
        RetentionPreviewPage {
            schema_version: 1,
            preview_only: true,
            items: Vec::new(),
            next_cursor: None,
            page_totals: RetentionPreviewTotals::default(),
            diagnostics: RetentionPreviewDiagnostics::default(),
        }
    );
    assert_eq!(
        fs::read_dir(home.path()).expect("read empty home").count(),
        0
    );
}

#[tokio::test]
async fn preview_ignores_state_db_only_rows() {
    let home = TempDir::new().expect("temp dir");
    let config = test_config(home.path());
    let state_db = codex_state::StateRuntime::init(
        config.sqlite.clone(),
        config.default_model_provider_id.clone(),
    )
    .await
    .expect("initialize state DB");
    let uuid = Uuid::from_u128(38);
    let thread_id = thread_id(uuid);
    let path = write_compressible_session(home.path(), false, uuid).expect("write session");
    codex_rollout::state_db::reconcile_rollout(
        Some(state_db.as_ref()),
        path.as_path(),
        "test-provider",
        /*builder*/ None,
        &[],
        /*archived_only*/ None,
        /*new_thread_memory_mode*/ None,
    )
    .await;
    assert!(
        state_db
            .get_thread(thread_id)
            .await
            .expect("read state DB before preview")
            .is_some()
    );
    fs::remove_file(path).expect("remove filesystem rollout");
    let store = LocalThreadStore::new(config, Some(state_db.clone()));

    let report = store
        .retention_preview(preview_params(25, None))
        .await
        .expect("state DB preview");

    assert_eq!(report.items, Vec::new());
    assert!(
        state_db
            .get_thread(thread_id)
            .await
            .expect("read state DB after preview")
            .is_some()
    );
}

#[tokio::test]
async fn active_then_archived_candidates_page_without_mutation() {
    let home = TempDir::new().expect("temp dir");
    let active_id = Uuid::from_u128(1);
    let archived_id = Uuid::from_u128(2);
    let active_path =
        write_compressible_session(home.path(), false, active_id).expect("write active session");
    let archived_path =
        write_compressible_session(home.path(), true, archived_id).expect("write archived session");
    set_old_mtime(active_path.as_path()).expect("age active session");
    set_old_mtime(archived_path.as_path()).expect("age archived session");
    let before = [snapshot(&active_path), snapshot(&archived_path)];
    let store = enabled_store(home.path());

    let first = store
        .retention_preview(preview_params(1, None))
        .await
        .expect("first preview page");
    let second = store
        .retention_preview(preview_params(1, first.next_cursor.clone()))
        .await
        .expect("second preview page");

    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].collection, RetentionCollection::Active);
    assert_eq!(first.items[0].disposition, RetentionDisposition::Candidate);
    assert_eq!(first.items[0].reason, RetentionReason::ColdInactive);
    assert_eq!(first.items[0].proposed_action, RetentionAction::Compress);
    assert!(first.items[0].estimated_recoverable_bytes > 0);
    assert_eq!(first.page_totals.candidate_count, 1);
    assert_eq!(first.page_totals.protected_count, 0);
    assert_eq!(
        first.page_totals.current_storage_bytes,
        first.items[0].current_storage_bytes
    );
    assert_eq!(
        first.page_totals.estimated_recoverable_bytes,
        first.items[0].estimated_recoverable_bytes
    );
    assert!(first.next_cursor.is_some());
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].collection, RetentionCollection::Archived);
    assert_eq!(second.items[0].disposition, RetentionDisposition::Candidate);
    assert_eq!(second.next_cursor, None);
    assert_eq!([snapshot(&active_path), snapshot(&archived_path)], before);
    assert!(!home.path().join(".tmp").exists());
    assert!(!home.path().join("thread-writer-locks").exists());
    assert!(!home.path().join("state_5.sqlite").exists());
}

#[tokio::test]
async fn exact_active_page_without_archived_rollouts_has_no_next_cursor() {
    let home = TempDir::new().expect("temp dir");
    let active_path =
        write_compressible_session(home.path(), false, Uuid::from_u128(8)).expect("write session");
    set_old_mtime(active_path.as_path()).expect("age session");
    let store = enabled_store(home.path());

    let report = store
        .retention_preview(preview_params(1, None))
        .await
        .expect("preview");

    assert_eq!(report.items.len(), 1);
    assert_eq!(report.next_cursor, None);
}

#[tokio::test]
async fn active_lease_is_protected_without_touching_lock_mtime() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(3);
    let thread_id = thread_id(uuid);
    let path = write_compressible_session(home.path(), false, uuid).expect("write session");
    set_old_mtime(path.as_path()).expect("age session");
    let lease =
        RolloutLease::acquire_shared(home.path(), RolloutCompressionMode::Enabled, thread_id)
            .await
            .expect("acquire lease")
            .expect("enabled lease");
    let lease_path = home
        .path()
        .join(".tmp/rollout-leases")
        .join(format!("{thread_id}.lock"));
    let lease_before = snapshot(&lease_path);
    let store = enabled_store(home.path());

    let report = store
        .retention_preview(preview_params(25, None))
        .await
        .expect("leased preview");

    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].reason, RetentionReason::ActiveRolloutLease);
    assert_eq!(report.items[0].disposition, RetentionDisposition::Protected);
    assert_eq!(snapshot(&lease_path), lease_before);
    drop(lease);
}

#[tokio::test]
async fn active_writer_is_protected_without_touching_lock_state() {
    let home = TempDir::new().expect("temp dir");
    let uuid = Uuid::from_u128(31);
    let thread_id = thread_id(uuid);
    let path = write_compressible_session(home.path(), false, uuid).expect("write session");
    set_old_mtime(path.as_path()).expect("age session");
    let store = enabled_store(home.path());
    let writer_lock = store
        .writer_lock_coordinator
        .acquire(thread_id)
        .expect("acquire writer lock");
    let lock_path = home
        .path()
        .join("thread-writer-locks")
        .join(format!("{thread_id}.lock"));
    let lock_before = snapshot(&lock_path);

    let report = store
        .retention_preview(preview_params(25, None))
        .await
        .expect("writer preview");

    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].reason, RetentionReason::ActiveWriter);
    assert_eq!(report.items[0].disposition, RetentionDisposition::Protected);
    assert_eq!(snapshot(&lock_path), lock_before);
    drop(writer_lock);
}

#[tokio::test]
async fn disabled_compression_protects_active_and_archived_rollouts() {
    let home = TempDir::new().expect("temp dir");
    let active_path =
        write_compressible_session(home.path(), false, Uuid::from_u128(32)).expect("active");
    let archived_path =
        write_compressible_session(home.path(), true, Uuid::from_u128(33)).expect("archived");
    set_old_mtime(active_path.as_path()).expect("age active");
    set_old_mtime(archived_path.as_path()).expect("age archived");
    let store = disabled_store(home.path());

    let report = store
        .retention_preview(preview_params(25, None))
        .await
        .expect("disabled preview");

    assert_eq!(report.items.len(), 2);
    assert!(report.items.iter().all(|item| {
        item.reason == RetentionReason::CompressionDisabled
            && item.disposition == RetentionDisposition::Protected
    }));
    assert_eq!(report.page_totals.candidate_count, 0);
    assert_eq!(report.page_totals.protected_count, 2);
}

#[tokio::test]
async fn fresh_and_compressed_rollouts_are_protected_explicitly() {
    let home = TempDir::new().expect("temp dir");
    let _fresh_path =
        write_compressible_session(home.path(), false, Uuid::from_u128(34)).expect("fresh");
    let compressed_plain =
        write_compressible_session(home.path(), false, Uuid::from_u128(35)).expect("compressed");
    let compressed_path = codex_rollout::compressed_rollout_path(compressed_plain.as_path());
    let encoded = zstd::stream::encode_all(
        fs::read(compressed_plain.as_path())
            .expect("read plain rollout")
            .as_slice(),
        0,
    )
    .expect("compress rollout");
    fs::write(&compressed_path, encoded).expect("write compressed rollout");
    fs::remove_file(&compressed_plain).expect("remove plain rollout");
    set_old_mtime(compressed_path.as_path()).expect("age compressed");
    let store = enabled_store(home.path());

    let report = store
        .retention_preview(preview_params(25, None))
        .await
        .expect("protected preview");

    assert_eq!(report.items.len(), 2);
    let fresh = report
        .items
        .iter()
        .find(|item| item.thread_id == thread_id(Uuid::from_u128(34)))
        .expect("fresh rollout");
    let compressed = report
        .items
        .iter()
        .find(|item| item.thread_id == thread_id(Uuid::from_u128(35)))
        .expect("compressed rollout");
    assert_eq!(fresh.reason, RetentionReason::TooRecent);
    assert_eq!(compressed.reason, RetentionReason::AlreadyCompressed);
}

#[tokio::test]
async fn archived_rollouts_page_deterministically() {
    let home = TempDir::new().expect("temp dir");
    let older_path =
        write_compressible_session(home.path(), true, Uuid::from_u128(36)).expect("older");
    let newer_path =
        write_compressible_session(home.path(), true, Uuid::from_u128(37)).expect("newer");
    let shared_modified = SystemTime::now()
        .checked_sub(Duration::from_secs(8 * 24 * 60 * 60))
        .expect("old time");
    set_mtime(older_path.as_path(), shared_modified).expect("age older archived");
    set_mtime(newer_path.as_path(), shared_modified).expect("age newer archived");
    let store = enabled_store(home.path());

    let first = store
        .retention_preview(preview_params(1, None))
        .await
        .expect("first archived page");
    let repeated_first = store
        .retention_preview(preview_params(1, None))
        .await
        .expect("repeated first archived page");
    let second = store
        .retention_preview(preview_params(1, first.next_cursor.clone()))
        .await
        .expect("second archived page");

    assert_eq!(first, repeated_first);
    assert_eq!(first.items[0].thread_id, thread_id(Uuid::from_u128(37)));
    assert!(first.next_cursor.is_some());
    assert_eq!(second.items[0].thread_id, thread_id(Uuid::from_u128(36)));
    assert_eq!(second.next_cursor, None);
}

#[tokio::test]
async fn fork_reference_and_history_pointer_are_both_protected() {
    let home = TempDir::new().expect("temp dir");
    let source_id = thread_id(Uuid::from_u128(4));
    let child_id = thread_id(Uuid::from_u128(5));
    let source_path =
        write_reference_rollout(home.path(), source_id, None, "12-00-00").expect("write source");
    let history_base = HistoryPosition {
        thread_id: source_id,
        end_ordinal_exclusive: 1,
        end_byte_offset: 1,
    };
    let child_path = write_reference_rollout(home.path(), child_id, Some(history_base), "12-00-01")
        .expect("write child");
    set_old_mtime(source_path.as_path()).expect("age source");
    set_old_mtime(child_path.as_path()).expect("age child");
    let store = enabled_store(home.path());

    let report = store
        .retention_preview(preview_params(25, None))
        .await
        .expect("reference preview");

    assert_eq!(report.items.len(), 2);
    let source = report
        .items
        .iter()
        .find(|item| item.thread_id == source_id)
        .expect("source rollout");
    let child = report
        .items
        .iter()
        .find(|item| item.thread_id == child_id)
        .expect("child rollout");
    assert_eq!(source.reason, RetentionReason::ForkReferenced);
    assert_eq!(child.reason, RetentionReason::ForkHistoryPointer);
}

#[tokio::test]
async fn unreadable_metadata_fail_protects_other_candidates() {
    let home = TempDir::new().expect("temp dir");
    let valid_path = write_compressible_session(home.path(), false, Uuid::from_u128(6))
        .expect("write valid session");
    set_old_mtime(valid_path.as_path()).expect("age valid session");
    let malformed_path = home
        .path()
        .join("sessions/2025/01/03/rollout-2025-01-03T12-00-01-")
        .with_file_name(format!(
            "rollout-2025-01-03T12-00-01-{}.jsonl",
            Uuid::from_u128(7)
        ));
    fs::create_dir_all(malformed_path.parent().expect("malformed parent"))
        .expect("create malformed parent");
    fs::write(&malformed_path, b"not-json\n").expect("write malformed rollout");
    let store = enabled_store(home.path());

    let report = store
        .retention_preview(preview_params(25, None))
        .await
        .expect("uncertain preview");

    assert_eq!(report.diagnostics.unreadable_rollouts, 1);
    assert!(!report.items.is_empty());
    assert!(report.items.iter().all(|item| {
        item.disposition == RetentionDisposition::Protected
            && item.reason == RetentionReason::ReferenceMetadataUncertain
    }));
}

#[tokio::test]
async fn invalid_cursor_and_limit_are_rejected() {
    let home = TempDir::new().expect("temp dir");
    let store = enabled_store(home.path());

    let cursor_error = store
        .retention_preview(preview_params(25, Some("not-a-cursor".to_string())))
        .await
        .expect_err("invalid cursor");
    let limit_error = store
        .retention_preview(preview_params(101, None))
        .await
        .expect_err("invalid limit");
    let zero_error = store
        .retention_preview(preview_params(0, None))
        .await
        .expect_err("zero limit");

    assert!(matches!(
        cursor_error,
        ThreadStoreError::InvalidRequest { .. }
    ));
    assert!(matches!(
        limit_error,
        ThreadStoreError::InvalidRequest { .. }
    ));
    assert!(matches!(
        zero_error,
        ThreadStoreError::InvalidRequest { .. }
    ));
}

fn enabled_store(home: &Path) -> LocalThreadStore {
    let mut config = test_config(home);
    config.rollout_compression_mode = RolloutCompressionMode::Enabled;
    LocalThreadStore::new(config, /*state_db*/ None)
}

fn disabled_store(home: &Path) -> LocalThreadStore {
    LocalThreadStore::new(test_config(home), /*state_db*/ None)
}

fn preview_params(limit: usize, cursor: Option<String>) -> RetentionPreviewParams {
    RetentionPreviewParams {
        limit: Some(limit),
        cursor,
    }
}

fn write_compressible_session(home: &Path, archived: bool, uuid: Uuid) -> std::io::Result<PathBuf> {
    let path = if archived {
        write_archived_session_file(home, "2025-01-03T12-00-00", uuid)?
    } else {
        write_session_file(home, "2025-01-03T12-00-00", uuid)?
    };
    let mut file = fs::OpenOptions::new().append(true).open(&path)?;
    let padding = "x".repeat(16 * 1024);
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "timestamp": "2025-01-03T12:00:00Z",
            "type": "event_msg",
            "payload": { "type": "agent_message", "message": padding },
        })
    )?;
    Ok(path)
}

fn write_reference_rollout(
    home: &Path,
    thread_id: ThreadId,
    history_base: Option<HistoryPosition>,
    time: &str,
) -> std::io::Result<PathBuf> {
    let directory = home.join("sessions/2025/01/03");
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("rollout-2025-01-03T{time}-{thread_id}.jsonl"));
    let metadata = serde_json::json!({
        "timestamp": "2025-01-03T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "session_id": thread_id,
            "id": thread_id,
            "timestamp": "2025-01-03T12:00:00Z",
            "cwd": home,
            "originator": "test",
            "cli_version": "test",
            "source": "cli",
            "model_provider": "test-provider",
            "history_mode": "paginated",
            "history_base": history_base,
        },
    });
    let user_event = serde_json::json!({
        "timestamp": "2025-01-03T12:00:00Z",
        "type": "event_msg",
        "payload": {
            "type": "user_message",
            "message": "retention preview fixture",
            "kind": "plain",
        },
    });
    fs::write(&path, format!("{metadata}\n{user_event}\n"))?;
    Ok(path)
}

fn set_old_mtime(path: &Path) -> std::io::Result<()> {
    set_mtime_age(path, Duration::from_secs(8 * 24 * 60 * 60))
}

fn set_mtime_age(path: &Path, age: Duration) -> std::io::Result<()> {
    let old = SystemTime::now().checked_sub(age).expect("old time");
    set_mtime(path, old)
}

fn set_mtime(path: &Path, modified: SystemTime) -> std::io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_times(FileTimes::new().set_modified(modified))
}

fn snapshot(path: &Path) -> (u64, SystemTime) {
    let metadata = fs::metadata(path).expect("snapshot metadata");
    (
        metadata.len(),
        metadata.modified().expect("snapshot modified time"),
    )
}

fn thread_id(uuid: Uuid) -> ThreadId {
    ThreadId::from_string(&uuid.to_string()).expect("valid thread id")
}
