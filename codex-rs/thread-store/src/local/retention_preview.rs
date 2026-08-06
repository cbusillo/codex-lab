use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

use codex_rollout::RolloutCompressionMode;
use codex_rollout::RolloutLease;
use codex_rollout::RolloutReferenceIndex;

use super::LocalThreadStore;
use crate::DEFAULT_RETENTION_PREVIEW_LIMIT;
use crate::ListThreadsParams;
use crate::MAX_RETENTION_PREVIEW_LIMIT;
use crate::RetentionAction;
use crate::RetentionCollection;
use crate::RetentionDisposition;
use crate::RetentionPreviewDiagnostics;
use crate::RetentionPreviewItem;
use crate::RetentionPreviewPage;
use crate::RetentionPreviewParams;
use crate::RetentionPreviewTotals;
use crate::RetentionReason;
use crate::SortDirection;
use crate::StoredThread;
use crate::ThreadSortKey;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

const SCHEMA_VERSION: u32 = 1;
const MAX_REFERENCE_SCAN_ROLLOUTS: usize = 10_000;
const CURSOR_PREFIX: &str = "v1";

pub(super) async fn retention_preview(
    store: &LocalThreadStore,
    params: RetentionPreviewParams,
) -> ThreadStoreResult<RetentionPreviewPage> {
    let limit = params.limit.unwrap_or(DEFAULT_RETENTION_PREVIEW_LIMIT);
    if !(1..=MAX_RETENTION_PREVIEW_LIMIT).contains(&limit) {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!(
                "retention preview limit must be between 1 and {MAX_RETENTION_PREVIEW_LIMIT}"
            ),
        });
    }

    let cursor = params.cursor.as_deref().map(decode_cursor).transpose()?;
    let reference_index = RolloutReferenceIndex::scan_bounded(
        store.config.codex_home.as_path(),
        MAX_REFERENCE_SCAN_ROLLOUTS,
    )
    .await
    .map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to scan rollout references: {err}"),
    })?;

    let diagnostics = RetentionPreviewDiagnostics {
        scanned_rollouts: reference_index.scanned_rollouts(),
        unreadable_rollouts: reference_index.unreadable_rollouts(),
        duplicate_thread_ids: reference_index.duplicate_thread_ids(),
        scan_truncated: reference_index.scan_truncated(),
    };
    let listing_store = LocalThreadStore::new(store.config.clone(), /*state_db*/ None);
    let (threads, next_cursor) = list_preview_threads(&listing_store, cursor, limit).await?;
    let now = SystemTime::now();
    let mut items = Vec::with_capacity(threads.len());
    for (collection, thread) in threads {
        items.push(classify_thread(store, &reference_index, collection, thread, now).await);
    }
    let totals = totals(&items);

    Ok(RetentionPreviewPage {
        schema_version: SCHEMA_VERSION,
        preview_only: true,
        items,
        next_cursor,
        page_totals: totals,
        diagnostics,
    })
}

async fn list_preview_threads(
    store: &LocalThreadStore,
    cursor: Option<PreviewCursor>,
    limit: usize,
) -> ThreadStoreResult<(Vec<(RetentionCollection, StoredThread)>, Option<String>)> {
    let cursor = cursor.unwrap_or(PreviewCursor {
        collection: RetentionCollection::Active,
        inner: None,
    });
    let mut items = Vec::with_capacity(limit);

    if cursor.collection == RetentionCollection::Active {
        let page = super::list_threads::list_threads(
            store,
            list_params(limit, cursor.inner.clone(), /*archived*/ false),
        )
        .await?;
        items.extend(
            page.items
                .into_iter()
                .map(|thread| (RetentionCollection::Active, thread)),
        );
        if let Some(inner) = page.next_cursor {
            return Ok((
                items,
                Some(encode_cursor(&PreviewCursor {
                    collection: RetentionCollection::Active,
                    inner: Some(inner),
                })),
            ));
        }
        if items.len() == limit {
            let archived =
                super::list_threads::list_threads(store, list_params(1, None, /*archived*/ true))
                    .await?;
            return Ok((
                items,
                (!archived.items.is_empty()).then(|| {
                    encode_cursor(&PreviewCursor {
                        collection: RetentionCollection::Archived,
                        inner: None,
                    })
                }),
            ));
        }
    }

    let archived_cursor = if cursor.collection == RetentionCollection::Archived {
        cursor.inner
    } else {
        None
    };
    let remaining = limit.saturating_sub(items.len());
    let page = super::list_threads::list_threads(
        store,
        list_params(remaining, archived_cursor, /*archived*/ true),
    )
    .await?;
    items.extend(
        page.items
            .into_iter()
            .map(|thread| (RetentionCollection::Archived, thread)),
    );
    let next_cursor = page.next_cursor.map(|inner| {
        encode_cursor(&PreviewCursor {
            collection: RetentionCollection::Archived,
            inner: Some(inner),
        })
    });
    Ok((items, next_cursor))
}

fn list_params(page_size: usize, cursor: Option<String>, archived: bool) -> ListThreadsParams {
    ListThreadsParams {
        page_size,
        cursor,
        sort_key: ThreadSortKey::RecencyAt,
        sort_direction: SortDirection::Desc,
        allowed_sources: Vec::new(),
        model_providers: None,
        cwd_filters: None,
        section: None,
        archived,
        search_term: None,
        relation_filter: None,
        use_state_db_only: false,
    }
}

async fn classify_thread(
    store: &LocalThreadStore,
    reference_index: &RolloutReferenceIndex,
    collection: RetentionCollection,
    thread: StoredThread,
    now: SystemTime,
) -> RetentionPreviewItem {
    let Some(plain_path) = thread.rollout_path else {
        return protected_item(
            thread.thread_id,
            collection,
            RetentionReason::RolloutPathUnavailable,
            0,
            None,
        );
    };
    let Some(actual_path) = codex_rollout::existing_rollout_path(plain_path.as_path()).await else {
        return protected_item(
            thread.thread_id,
            collection,
            RetentionReason::RolloutPathUnavailable,
            0,
            Some(plain_path),
        );
    };
    let current_storage_bytes = match tokio::fs::metadata(actual_path.as_path()).await {
        Ok(metadata) if metadata.is_file() => metadata.len(),
        _ => {
            return protected_item(
                thread.thread_id,
                collection,
                RetentionReason::RolloutPathUnavailable,
                0,
                Some(actual_path),
            );
        }
    };

    if !reference_index.is_complete() {
        return protected_item(
            thread.thread_id,
            collection,
            RetentionReason::ReferenceMetadataUncertain,
            current_storage_bytes,
            Some(actual_path),
        );
    }
    let meta = match codex_rollout::read_session_meta_line(actual_path.as_path()).await {
        Ok(meta) if meta.meta.id == thread.thread_id => meta,
        _ => {
            return protected_item(
                thread.thread_id,
                collection,
                RetentionReason::RolloutMetadataUncertain,
                current_storage_bytes,
                Some(actual_path),
            );
        }
    };
    if reference_index.reference_count(thread.thread_id) > 0 {
        return protected_item(
            thread.thread_id,
            collection,
            RetentionReason::ForkReferenced,
            current_storage_bytes,
            Some(actual_path),
        );
    }
    if meta.meta.history_base.is_some() {
        return protected_item(
            thread.thread_id,
            collection,
            RetentionReason::ForkHistoryPointer,
            current_storage_bytes,
            Some(actual_path),
        );
    }

    match store
        .writer_lock_coordinator
        .is_active_read_only(thread.thread_id)
    {
        Ok(true) => {
            return protected_item(
                thread.thread_id,
                collection,
                RetentionReason::ActiveWriter,
                current_storage_bytes,
                Some(actual_path),
            );
        }
        Ok(false) => {}
        Err(_) => {
            return protected_item(
                thread.thread_id,
                collection,
                RetentionReason::WriterStateUncertain,
                current_storage_bytes,
                Some(actual_path),
            );
        }
    }
    match RolloutLease::is_active_read_only(store.config.codex_home.as_path(), thread.thread_id)
        .await
    {
        Ok(true) => {
            return protected_item(
                thread.thread_id,
                collection,
                RetentionReason::ActiveRolloutLease,
                current_storage_bytes,
                Some(actual_path),
            );
        }
        Ok(false) => {}
        Err(_) => {
            return protected_item(
                thread.thread_id,
                collection,
                RetentionReason::LeaseStateUncertain,
                current_storage_bytes,
                Some(actual_path),
            );
        }
    }
    if matches!(
        store.config.rollout_compression_mode,
        RolloutCompressionMode::Disabled
    ) {
        return protected_item(
            thread.thread_id,
            collection,
            RetentionReason::CompressionDisabled,
            current_storage_bytes,
            Some(actual_path),
        );
    }
    if actual_path
        .extension()
        .is_some_and(|extension| extension == "zst")
    {
        return protected_item(
            thread.thread_id,
            collection,
            RetentionReason::AlreadyCompressed,
            current_storage_bytes,
            Some(actual_path),
        );
    }
    if !is_cold(actual_path.as_path(), now).await {
        return protected_item(
            thread.thread_id,
            collection,
            RetentionReason::TooRecent,
            current_storage_bytes,
            Some(actual_path),
        );
    }
    let estimated_compressed_bytes =
        match codex_rollout::estimate_compressed_rollout_size(actual_path.as_path()).await {
            Ok(bytes) => bytes,
            Err(_) => {
                return protected_item(
                    thread.thread_id,
                    collection,
                    RetentionReason::CompressionEstimateUnavailable,
                    current_storage_bytes,
                    Some(actual_path),
                );
            }
        };

    RetentionPreviewItem {
        thread_id: thread.thread_id,
        collection,
        disposition: RetentionDisposition::Candidate,
        reason: RetentionReason::ColdInactive,
        current_storage_bytes,
        estimated_recoverable_bytes: current_storage_bytes
            .saturating_sub(estimated_compressed_bytes),
        proposed_action: RetentionAction::Compress,
        proposed_path: Some(codex_rollout::compressed_rollout_path(
            actual_path.as_path(),
        )),
        recovery_path: Some(actual_path),
    }
}

fn protected_item(
    thread_id: codex_protocol::ThreadId,
    collection: RetentionCollection,
    reason: RetentionReason,
    current_storage_bytes: u64,
    recovery_path: Option<PathBuf>,
) -> RetentionPreviewItem {
    RetentionPreviewItem {
        thread_id,
        collection,
        disposition: RetentionDisposition::Protected,
        reason,
        current_storage_bytes,
        estimated_recoverable_bytes: 0,
        proposed_action: RetentionAction::Keep,
        proposed_path: None,
        recovery_path,
    }
}

async fn is_cold(path: &Path, now: SystemTime) -> bool {
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    now.duration_since(modified).unwrap_or_default() >= codex_rollout::ROLLOUT_COMPRESSION_MIN_AGE
}

fn totals(items: &[RetentionPreviewItem]) -> RetentionPreviewTotals {
    let mut totals = RetentionPreviewTotals::default();
    for item in items {
        match item.disposition {
            RetentionDisposition::Candidate => {
                totals.candidate_count = totals.candidate_count.saturating_add(1);
            }
            RetentionDisposition::Protected => {
                totals.protected_count = totals.protected_count.saturating_add(1);
            }
        }
        totals.current_storage_bytes = totals
            .current_storage_bytes
            .saturating_add(item.current_storage_bytes);
        totals.estimated_recoverable_bytes = totals
            .estimated_recoverable_bytes
            .saturating_add(item.estimated_recoverable_bytes);
    }
    totals
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewCursor {
    collection: RetentionCollection,
    inner: Option<String>,
}

fn encode_cursor(cursor: &PreviewCursor) -> String {
    let encoded = serde_json::to_vec(cursor).expect("retention cursor serializes");
    let mut output = String::with_capacity(CURSOR_PREFIX.len() + 1 + encoded.len() * 2);
    output.push_str(CURSOR_PREFIX);
    output.push('-');
    for byte in encoded {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn decode_cursor(cursor: &str) -> ThreadStoreResult<PreviewCursor> {
    let encoded = cursor
        .strip_prefix(&format!("{CURSOR_PREFIX}-"))
        .ok_or_else(|| invalid_cursor(cursor))?;
    if encoded.len() % 2 != 0 {
        return Err(invalid_cursor(cursor));
    }
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid_cursor(cursor))?;
    serde_json::from_slice(&bytes).map_err(|_| invalid_cursor(cursor))
}

fn invalid_cursor(cursor: &str) -> ThreadStoreError {
    ThreadStoreError::InvalidRequest {
        message: format!("invalid retention preview cursor: {cursor}"),
    }
}

#[cfg(test)]
#[path = "retention_preview_tests.rs"]
mod tests;
