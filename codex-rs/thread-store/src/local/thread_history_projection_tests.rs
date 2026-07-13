use std::fs::File;
use std::io::Write;
use std::path::Path;

use codex_protocol::ThreadId;
use codex_protocol::config_types::ModeKind;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_state::ThreadHistoryCheckpoint;
use tempfile::TempDir;

use super::*;

#[tokio::test]
async fn restart_rebuilds_and_replacements_are_generation_fenced() {
    let home = TempDir::new().expect("temp dir");
    let rollout_path = home.path().join("rollout.jsonl");
    let thread_id = ThreadId::default();
    write_rollout(
        &rollout_path,
        &[started_line(0, "turn-a"), completed_line(1, "turn-a")],
    );
    let controller = ThreadHistoryProjectionController::new(home.path().to_path_buf());
    controller
        .reconcile(
            thread_id,
            &rollout_path,
            ProjectionReconcileMode::VerifyPrefix,
        )
        .await
        .expect("initial projection");
    let initial_checkpoint = checkpoint(&controller, thread_id).await;
    assert!(
        initial_checkpoint
            .rollout_source_id
            .as_deref()
            .is_some_and(|source_id| source_id.starts_with("path-sha256:"))
    );
    assert!(
        !initial_checkpoint
            .rollout_source_id
            .as_deref()
            .is_some_and(|source_id| source_id.contains(home.path().to_string_lossy().as_ref()))
    );
    let db_path = controller.repository().path().to_path_buf();
    drop(controller);
    remove_sqlite_files(&db_path).await;

    let restarted = ThreadHistoryProjectionController::new(home.path().to_path_buf());
    restarted
        .reconcile(
            thread_id,
            &rollout_path,
            ProjectionReconcileMode::VerifyPrefix,
        )
        .await
        .expect("restart rebuild");
    assert_eq!(checkpoint(&restarted, thread_id).await, initial_checkpoint);

    write_rollout(
        &rollout_path,
        &[started_line(0, "turn-b"), completed_line(1, "turn-b")],
    );
    restarted
        .reconcile(
            thread_id,
            &rollout_path,
            ProjectionReconcileMode::VerifyPrefix,
        )
        .await
        .expect("prefix replacement");
    let prefix_checkpoint = checkpoint(&restarted, thread_id).await;
    assert_eq!(checkpoint_state(&prefix_checkpoint), (2, 1));

    let replacement_path = home.path().join("replacement.jsonl");
    write_rollout(
        &replacement_path,
        &[started_line(0, "turn-c"), completed_line(1, "turn-c")],
    );
    restarted
        .reconcile(
            thread_id,
            &replacement_path,
            ProjectionReconcileMode::VerifyPrefix,
        )
        .await
        .expect("source replacement");
    let source_checkpoint = checkpoint(&restarted, thread_id).await;
    assert_eq!(checkpoint_state(&source_checkpoint), (2, 2));
}

async fn checkpoint(
    controller: &ThreadHistoryProjectionController,
    thread_id: ThreadId,
) -> ThreadHistoryCheckpoint {
    controller
        .repository()
        .checkpoint(&thread_id.to_string())
        .await
        .expect("checkpoint")
        .expect("checkpoint row")
}

fn checkpoint_state(checkpoint: &ThreadHistoryCheckpoint) -> (u64, u64) {
    (
        checkpoint.next_rollout_ordinal,
        checkpoint.projection_generation,
    )
}

fn write_rollout(path: &Path, lines: &[RolloutLine]) {
    let mut file = File::create(path).expect("create rollout");
    for line in lines {
        writeln!(
            file,
            "{}",
            serde_json::to_string(line).expect("serialize line")
        )
        .unwrap();
    }
}

fn started_line(ordinal: u64, turn_id: &str) -> RolloutLine {
    line(
        ordinal,
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: turn_id.to_string(),
            trace_id: None,
            started_at: Some(10),
            model_context_window: None,
            collaboration_mode_kind: ModeKind::Default,
        }),
    )
}

fn completed_line(ordinal: u64, turn_id: &str) -> RolloutLine {
    line(
        ordinal,
        EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: turn_id.to_string(),
            last_agent_message: None,
            error: None,
            started_at: Some(10),
            completed_at: Some(20),
            duration_ms: Some(10_000),
            time_to_first_token_ms: None,
        }),
    )
}

fn line(ordinal: u64, event: EventMsg) -> RolloutLine {
    RolloutLine {
        timestamp: "2025-01-03T17:30:00Z".to_string(),
        ordinal: Some(ordinal),
        item: RolloutItem::EventMsg(event),
    }
}

async fn remove_sqlite_files(path: &Path) {
    for path in [
        path.to_path_buf(),
        path.with_extension("sqlite-shm"),
        path.with_extension("sqlite-wal"),
    ] {
        let _ = tokio::fs::remove_file(path).await;
    }
}
