use std::path::PathBuf;

use pretty_assertions::assert_eq;

use super::*;

fn test_sqlite_home() -> PathBuf {
    std::env::temp_dir().join(format!("codex-thread-history-{}", uuid::Uuid::new_v4()))
}

fn source(fingerprint: &str) -> ThreadHistorySource {
    ThreadHistorySource {
        rollout_source_id: "rollout-source-1".to_string(),
        rollout_fingerprint: fingerprint.to_string(),
    }
}

fn suffix(
    mutations: Vec<ThreadHistoryMutation>,
    next_rollout_ordinal: u64,
    fingerprint: &str,
) -> ThreadHistorySuffix {
    ThreadHistorySuffix {
        mutations,
        checkpoint: ThreadHistoryCheckpointUpdate {
            next_rollout_byte_offset: next_rollout_ordinal * 100,
            next_rollout_ordinal,
            source: source(fingerprint),
        },
    }
}

fn turn(
    turn_id: &str,
    rollout_ordinal: i64,
    status: ThreadHistoryTurnStatus,
) -> ThreadHistoryMutation {
    ThreadHistoryMutation::UpsertTurn(ThreadHistoryTurnUpsert {
        target: ThreadHistoryTurnTarget::Id(turn_id.to_string()),
        rollout_ordinal,
        status,
        error_json: None,
        started_at: Some(rollout_ordinal * 10),
        completed_at: None,
        duration_ms: None,
    })
}

fn item(
    turn_id: &str,
    item_id: &str,
    rollout_ordinal: i64,
    item_created_at_ms: i64,
    item_json: &str,
) -> ThreadHistoryMutation {
    ThreadHistoryMutation::UpsertItem(ThreadHistoryItemUpsert {
        target: ThreadHistoryTurnTarget::Id(turn_id.to_string()),
        item_id: item_id.to_string(),
        rollout_ordinal,
        item_created_at_ms,
        item_json: item_json.to_string(),
    })
}

#[tokio::test]
async fn repository_opens_lazily_and_applies_integrity_migration() {
    let sqlite_home = test_sqlite_home();
    let repository = ThreadHistoryRepository::new(sqlite_home.clone());
    assert!(!repository.path().exists());

    repository.ensure_open().await.expect("open history db");
    assert!(repository.path().exists());

    let projection_columns = sqlx::query_scalar::<_, String>(
        "SELECT name FROM pragma_table_info('thread_history_projection_state') ORDER BY cid",
    )
    .fetch_all(repository.pool().await.expect("history pool"))
    .await
    .expect("projection columns");
    assert_eq!(
        projection_columns,
        vec![
            "thread_id",
            "next_rollout_byte_offset",
            "next_rollout_ordinal",
            "rollout_source_id",
            "rollout_fingerprint",
            "projection_generation",
            "projection_status",
        ]
    );
    let item_columns = sqlx::query_scalar::<_, String>(
        "SELECT name FROM pragma_table_info('thread_items') ORDER BY cid",
    )
    .fetch_all(repository.pool().await.expect("history pool"))
    .await
    .expect("item columns");
    assert_eq!(
        item_columns.last().map(String::as_str),
        Some("item_created_at_ms")
    );
    let integrity_objects = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE name IN ('idx_thread_history_projection_state_dirty', 'thread_turns_delete_items') ORDER BY name",
    )
    .fetch_all(repository.pool().await.expect("history pool"))
    .await
    .expect("integrity objects");
    assert_eq!(
        integrity_objects,
        vec![
            "idx_thread_history_projection_state_dirty",
            "thread_turns_delete_items",
        ]
    );

    let pool = repository.pool().await.expect("history pool");
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(9_999_i64)
    .bind("future migration")
    .bind(true)
    .bind(vec![1_u8, 2, 3, 4])
    .bind(1_i64)
    .execute(pool)
    .await
    .expect("insert future migration");
    pool.close().await;
    drop(repository);

    ThreadHistoryRepository::new(sqlite_home.clone())
        .ensure_open()
        .await
        .expect("runtime migrator should tolerate a newer applied migration");
    let _ = tokio::fs::remove_dir_all(sqlite_home).await;
}

#[tokio::test]
async fn suffix_transaction_preserves_first_ordinals_and_rolls_back_atomically() {
    let sqlite_home = test_sqlite_home();
    let repository = ThreadHistoryRepository::new(sqlite_home.clone());
    repository
        .apply_suffix(
            "thread-1",
            &suffix(
                vec![
                    turn(
                        "turn-1",
                        /*rollout_ordinal*/ 1,
                        ThreadHistoryTurnStatus::InProgress,
                    ),
                    item(
                        "turn-1",
                        "item-1",
                        /*rollout_ordinal*/ 2,
                        /*item_created_at_ms*/ 200,
                        r#"{"version":1}"#,
                    ),
                ],
                /*next_rollout_ordinal*/ 3,
                "fingerprint-1",
            ),
        )
        .await
        .expect("apply first suffix");
    repository
        .apply_suffix(
            "thread-1",
            &suffix(
                vec![
                    turn(
                        "turn-1",
                        /*rollout_ordinal*/ 3,
                        ThreadHistoryTurnStatus::Completed,
                    ),
                    item(
                        "turn-1",
                        "item-1",
                        /*rollout_ordinal*/ 4,
                        /*item_created_at_ms*/ 400,
                        r#"{"version":2}"#,
                    ),
                ],
                /*next_rollout_ordinal*/ 5,
                "fingerprint-2",
            ),
        )
        .await
        .expect("apply update suffix");

    let turns = repository
        .turns_keyset(ThreadHistoryTurnsQuery {
            thread_id: "thread-1",
            cursor: None,
            direction: ThreadHistoryPageDirection::Ascending,
            limit: 10,
        })
        .await
        .expect("query turns");
    assert_eq!(
        turns,
        vec![ThreadHistoryTurnRow {
            turn_id: "turn-1".to_string(),
            rollout_ordinal: 1,
            status: ThreadHistoryTurnStatus::Completed,
            error_json: None,
            started_at: Some(30),
            completed_at: None,
            duration_ms: None,
        }]
    );
    let items = repository
        .items_keyset(ThreadHistoryItemsQuery {
            thread_id: "thread-1",
            turn_id: None,
            cursor: None,
            direction: ThreadHistoryPageDirection::Ascending,
            limit: 10,
        })
        .await
        .expect("query items");
    assert_eq!(
        items,
        vec![ThreadHistoryItemRow {
            turn_id: "turn-1".to_string(),
            item_key: "item-1".to_string(),
            item_ordinal: 2,
            item_created_at_ms: 200,
            materialized_thread_item_json: br#"{"version":2}"#.to_vec(),
        }]
    );
    assert_eq!(
        repository.checkpoint("thread-1").await.expect("checkpoint"),
        Some(ThreadHistoryCheckpoint {
            thread_id: "thread-1".to_string(),
            next_rollout_byte_offset: 500,
            next_rollout_ordinal: 5,
            rollout_source_id: Some("rollout-source-1".to_string()),
            rollout_fingerprint: Some("fingerprint-2".to_string()),
            projection_generation: 0,
            projection_status: ThreadHistoryProjectionStatus::Clean,
        })
    );

    let error = repository
        .apply_suffix(
            "thread-1",
            &suffix(
                vec![
                    turn(
                        "turn-2",
                        /*rollout_ordinal*/ 5,
                        ThreadHistoryTurnStatus::InProgress,
                    ),
                    item(
                        "missing-turn",
                        "item-2",
                        /*rollout_ordinal*/ 6,
                        /*item_created_at_ms*/ 600,
                        "{}",
                    ),
                ],
                /*next_rollout_ordinal*/ 7,
                "fingerprint-3",
            ),
        )
        .await
        .expect_err("invalid suffix should roll back");
    assert!(error.to_string().contains("does not exist"));
    assert_eq!(
        repository
            .turns_keyset(ThreadHistoryTurnsQuery {
                thread_id: "thread-1",
                cursor: None,
                direction: ThreadHistoryPageDirection::Ascending,
                limit: 10,
            })
            .await
            .expect("query turns after rollback"),
        turns
    );
    assert_eq!(
        repository
            .checkpoint("thread-1")
            .await
            .expect("checkpoint after rollback")
            .expect("checkpoint row")
            .next_rollout_ordinal,
        5
    );
    let _ = tokio::fs::remove_dir_all(sqlite_home).await;
}

#[tokio::test]
async fn keyset_cleanup_dirty_state_and_integer_boundaries_are_safe() {
    let sqlite_home = test_sqlite_home();
    let repository = ThreadHistoryRepository::new(sqlite_home.clone());
    repository.mark_dirty("thread-1").await.expect("mark dirty");
    assert_eq!(
        repository.bump_generation("thread-1").await.expect("bump"),
        1
    );

    repository
        .apply_suffix(
            "thread-1",
            &suffix(
                vec![
                    turn(
                        "turn-1",
                        /*rollout_ordinal*/ 1,
                        ThreadHistoryTurnStatus::Completed,
                    ),
                    item(
                        "turn-1", "item-1", /*rollout_ordinal*/ 2,
                        /*item_created_at_ms*/ 20, "{}",
                    ),
                    turn(
                        "turn-2",
                        /*rollout_ordinal*/ 11,
                        ThreadHistoryTurnStatus::Completed,
                    ),
                    item(
                        "turn-2", "item-2", /*rollout_ordinal*/ 12,
                        /*item_created_at_ms*/ 120, "{}",
                    ),
                    turn(
                        "turn-3",
                        /*rollout_ordinal*/ 21,
                        ThreadHistoryTurnStatus::InProgress,
                    ),
                    item(
                        "turn-3", "item-3", /*rollout_ordinal*/ 22,
                        /*item_created_at_ms*/ 220, "{}",
                    ),
                ],
                /*next_rollout_ordinal*/ 30,
                "fingerprint-1",
            ),
        )
        .await
        .expect("apply gapped suffix");
    let checkpoint = repository
        .checkpoint("thread-1")
        .await
        .expect("checkpoint")
        .expect("checkpoint row");
    assert_eq!(checkpoint.projection_generation, 1);
    assert_eq!(
        checkpoint.projection_status,
        ThreadHistoryProjectionStatus::Clean
    );

    let forward = repository
        .items_keyset(ThreadHistoryItemsQuery {
            thread_id: "thread-1",
            turn_id: None,
            cursor: None,
            direction: ThreadHistoryPageDirection::Ascending,
            limit: 2,
        })
        .await
        .expect("forward items");
    assert_eq!(
        forward
            .iter()
            .map(|item| item.item_ordinal)
            .collect::<Vec<_>>(),
        vec![2, 12, 22]
    );
    let backward = repository
        .items_keyset(ThreadHistoryItemsQuery {
            thread_id: "thread-1",
            turn_id: None,
            cursor: Some(22),
            direction: ThreadHistoryPageDirection::Descending,
            limit: 1,
        })
        .await
        .expect("backward items");
    assert_eq!(
        backward
            .iter()
            .map(|item| item.item_ordinal)
            .collect::<Vec<_>>(),
        vec![12, 2]
    );
    let filtered = repository
        .items_keyset(ThreadHistoryItemsQuery {
            thread_id: "thread-1",
            turn_id: Some("turn-2"),
            cursor: None,
            direction: ThreadHistoryPageDirection::Ascending,
            limit: 10,
        })
        .await
        .expect("filtered items");
    assert_eq!(
        filtered
            .iter()
            .map(|item| item.item_key.as_str())
            .collect::<Vec<_>>(),
        vec!["item-2"]
    );

    repository
        .apply_suffix(
            "thread-1",
            &suffix(
                vec![ThreadHistoryMutation::RemoveLatestTurns {
                    rollout_ordinal: 30,
                    count: 2,
                }],
                /*next_rollout_ordinal*/ 31,
                "fingerprint-2",
            ),
        )
        .await
        .expect("remove latest turns");
    assert_eq!(
        repository
            .items_keyset(ThreadHistoryItemsQuery {
                thread_id: "thread-1",
                turn_id: None,
                cursor: None,
                direction: ThreadHistoryPageDirection::Ascending,
                limit: 10,
            })
            .await
            .expect("remaining items")
            .into_iter()
            .map(|item| item.item_key)
            .collect::<Vec<_>>(),
        vec!["item-1"]
    );

    repository
        .delete_thread("thread-1")
        .await
        .expect("delete thread");
    repository
        .delete_thread("thread-1")
        .await
        .expect("repeat delete");
    assert_eq!(
        repository.checkpoint("thread-1").await.expect("checkpoint"),
        None
    );
    let overflow = repository
        .apply_suffix(
            "thread-1",
            &ThreadHistorySuffix {
                mutations: Vec::new(),
                checkpoint: ThreadHistoryCheckpointUpdate {
                    next_rollout_byte_offset: u64::MAX,
                    next_rollout_ordinal: 0,
                    source: source("overflow"),
                },
            },
        )
        .await
        .expect_err("oversized offset must fail before storage");
    assert!(overflow.to_string().contains("signed integer range"));
    assert_eq!(
        repository.checkpoint("thread-1").await.expect("checkpoint"),
        None
    );
    let _ = tokio::fs::remove_dir_all(sqlite_home).await;
}
