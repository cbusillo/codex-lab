use std::fs::File;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::bail;
use chrono::DateTime;
use codex_app_server_protocol::ThreadHistoryProjectionMutation;
use codex_app_server_protocol::ThreadHistoryTurnTarget as ProjectedTurnTarget;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::project_rollout_line;
use codex_protocol::ThreadId;
use codex_protocol::protocol::RolloutLine;
use codex_state::ThreadHistoryCheckpointUpdate;
use codex_state::ThreadHistoryItemUpsert;
use codex_state::ThreadHistoryMutation;
use codex_state::ThreadHistoryProjectionStatus;
use codex_state::ThreadHistoryRepository;
use codex_state::ThreadHistorySource;
use codex_state::ThreadHistorySuffix;
use codex_state::ThreadHistoryTurnStatus;
use codex_state::ThreadHistoryTurnTarget;
use codex_state::ThreadHistoryTurnUpsert;
use sha2::Digest;
use sha2::Sha256;
use tokio::sync::Semaphore;
use tracing::warn;

const PROJECTION_LOCK_SHARDS: usize = 16;
const MAX_REPLACEMENT_ATTEMPTS: usize = 3;
const MAX_PROJECTED_MUTATIONS: usize = 1_000_000;
const MAX_ROLLOUT_LINE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum ProjectionReconcileMode {
    VerifyPrefix,
    TrustLiveWriter,
}

pub(super) struct ThreadHistoryProjectionController {
    repository: ThreadHistoryRepository,
    projection_locks: [Semaphore; PROJECTION_LOCK_SHARDS],
}

impl ThreadHistoryProjectionController {
    pub(super) fn new(sqlite_home: PathBuf) -> Self {
        Self {
            repository: ThreadHistoryRepository::new(sqlite_home),
            projection_locks: std::array::from_fn(|_| Semaphore::new(1)),
        }
    }

    pub(super) async fn reconcile_best_effort(
        &self,
        thread_id: ThreadId,
        rollout_path: &Path,
        mode: ProjectionReconcileMode,
    ) {
        if let Err(error) = self.reconcile(thread_id, rollout_path, mode).await {
            let thread_id_text = thread_id.to_string();
            if let Err(dirty_error) = self.repository.mark_dirty(&thread_id_text).await {
                warn!(
                    "failed to mark thread history projection dirty for {thread_id}: {dirty_error}"
                );
            }
            warn!("failed to project JSONL history for thread {thread_id}: {error}");
        }
    }

    async fn reconcile(
        &self,
        thread_id: ThreadId,
        rollout_path: &Path,
        mode: ProjectionReconcileMode,
    ) -> anyhow::Result<()> {
        let _permit = self.projection_locks[projection_lock_index(&thread_id)]
            .acquire()
            .await
            .context("thread history projection lock closed")?;
        let thread_id_text = thread_id.to_string();
        self.repository.mark_dirty(&thread_id_text).await?;
        let checkpoint = self
            .repository
            .checkpoint(&thread_id_text)
            .await?
            .context("mark_dirty must create a projection checkpoint")?;
        let canonical_path = tokio::fs::canonicalize(rollout_path).await?;
        let source_id = source_id(canonical_path.as_path());
        let source_changed = checkpoint
            .rollout_source_id
            .as_deref()
            .is_some_and(|recorded| recorded != source_id)
            || (checkpoint.rollout_source_id.is_none() && checkpoint.next_rollout_byte_offset > 0);
        if source_changed
            || checkpoint.rollout_source_id.is_some() && checkpoint.rollout_fingerprint.is_none()
        {
            return self
                .replace_from_start(&thread_id_text, rollout_path, source_id)
                .await;
        }

        if let Some(suffix) = scan_rollout(
            rollout_path.to_path_buf(),
            checkpoint.next_rollout_byte_offset,
            checkpoint.next_rollout_ordinal,
            checkpoint.rollout_fingerprint,
            source_id.clone(),
            mode,
        )
        .await?
        {
            self.repository
                .apply_suffix(&thread_id_text, checkpoint.projection_generation, &suffix)
                .await
        } else {
            self.replace_from_start(&thread_id_text, rollout_path, source_id)
                .await
        }
    }

    async fn replace_from_start(
        &self,
        thread_id: &str,
        rollout_path: &Path,
        source_id: String,
    ) -> anyhow::Result<()> {
        for _ in 0..MAX_REPLACEMENT_ATTEMPTS {
            let suffix = scan_rollout(
                rollout_path.to_path_buf(),
                0,
                0,
                None,
                source_id.clone(),
                ProjectionReconcileMode::VerifyPrefix,
            )
            .await?
            .context("a scan from byte zero cannot have a prefix mismatch")?;
            if tokio::fs::metadata(rollout_path).await?.len()
                != suffix.checkpoint.next_rollout_byte_offset
            {
                continue;
            }
            let generation = self.repository.begin_replacement(thread_id).await?;
            match self
                .repository
                .replace_projection(thread_id, generation, &suffix)
                .await
            {
                Ok(()) => return Ok(()),
                Err(error)
                    if error
                        .to_string()
                        .contains("stale thread history projection") =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }

        let checkpoint = self.repository.checkpoint(thread_id).await?;
        let rollout_len = tokio::fs::metadata(rollout_path).await?.len();
        if checkpoint.is_some_and(|checkpoint| {
            checkpoint.projection_status == ThreadHistoryProjectionStatus::Clean
                && checkpoint.rollout_source_id.as_deref() == Some(source_id.as_str())
                && checkpoint.next_rollout_byte_offset == rollout_len
        }) {
            return Ok(());
        }
        bail!("thread history projection changed during replacement")
    }

    #[cfg(test)]
    pub(super) fn repository(&self) -> &ThreadHistoryRepository {
        &self.repository
    }
}

async fn scan_rollout(
    path: PathBuf,
    start_offset: u64,
    start_ordinal: u64,
    expected_prefix_fingerprint: Option<String>,
    source_id: String,
    mode: ProjectionReconcileMode,
) -> anyhow::Result<Option<ThreadHistorySuffix>> {
    tokio::task::spawn_blocking(move || {
        scan_rollout_blocking(
            path.as_path(),
            start_offset,
            start_ordinal,
            expected_prefix_fingerprint.as_deref(),
            source_id,
            mode,
        )
    })
    .await?
}

fn scan_rollout_blocking(
    path: &Path,
    start_offset: u64,
    start_ordinal: u64,
    expected_prefix_fingerprint: Option<&str>,
    source_id: String,
    mode: ProjectionReconcileMode,
) -> anyhow::Result<Option<ThreadHistorySuffix>> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut current_fingerprint = empty_fingerprint();
    if start_offset > 0 {
        let Some(expected_prefix_fingerprint) = expected_prefix_fingerprint else {
            return Ok(None);
        };
        match mode {
            ProjectionReconcileMode::VerifyPrefix => {
                let mut prefix_offset = 0_u64;
                while prefix_offset < start_offset {
                    let mut bytes = Vec::new();
                    let read = read_rollout_line(&mut reader, &mut bytes)?;
                    if read == 0 {
                        return Ok(None);
                    }
                    prefix_offset = prefix_offset
                        .checked_add(read as u64)
                        .context("paginated rollout byte offset overflow")?;
                    if prefix_offset > start_offset {
                        return Ok(None);
                    }
                    current_fingerprint = extend_fingerprint(&current_fingerprint, &bytes);
                }
                if current_fingerprint != expected_prefix_fingerprint {
                    return Ok(None);
                }
            }
            ProjectionReconcileMode::TrustLiveWriter => {
                reader.seek(SeekFrom::Start(start_offset))?;
                current_fingerprint = expected_prefix_fingerprint.to_string();
            }
        }
    }
    let mut mutations = Vec::new();
    let mut next_offset = start_offset;
    let mut next_ordinal = start_ordinal;
    let mut previous_ordinal = start_ordinal.checked_sub(1);
    loop {
        let mut bytes = Vec::new();
        let read = read_rollout_line(&mut reader, &mut bytes)?;
        if read == 0 {
            break;
        }
        if bytes.last() != Some(&b'\n') {
            bail!("paginated rollout has an incomplete trailing line");
        }
        current_fingerprint = extend_fingerprint(&current_fingerprint, &bytes);
        next_offset = next_offset
            .checked_add(read as u64)
            .context("paginated rollout byte offset overflow")?;
        let line: RolloutLine = serde_json::from_slice(&bytes)?;
        let changes = project_rollout_line(&line)?;
        let (ordinal, mutation) = changes.into_parts();
        let ordinal_u64 =
            u64::try_from(ordinal).context("projected ordinal must be non-negative")?;
        if ordinal_u64 < start_ordinal
            || previous_ordinal.is_some_and(|previous| ordinal_u64 <= previous)
        {
            bail!("paginated rollout ordinal {ordinal_u64} is not strictly increasing");
        }
        previous_ordinal = Some(ordinal_u64);
        next_ordinal = ordinal_u64 + 1;
        if let Some(mutation) = storage_mutation(&line, ordinal, mutation)? {
            if mutations.len() == MAX_PROJECTED_MUTATIONS {
                bail!("paginated rollout exceeds the projected mutation limit");
            }
            mutations.push(mutation);
        }
    }
    Ok(Some(ThreadHistorySuffix {
        mutations,
        checkpoint: ThreadHistoryCheckpointUpdate {
            next_rollout_byte_offset: next_offset,
            next_rollout_ordinal: next_ordinal,
            source: ThreadHistorySource {
                rollout_source_id: source_id,
                rollout_fingerprint: current_fingerprint,
            },
        },
    }))
}

fn storage_mutation(
    line: &RolloutLine,
    ordinal: i64,
    mutation: Option<ThreadHistoryProjectionMutation>,
) -> anyhow::Result<Option<ThreadHistoryMutation>> {
    mutation
        .map(|mutation| match mutation {
            ThreadHistoryProjectionMutation::UpsertTurn { target, update } => {
                Ok(ThreadHistoryMutation::UpsertTurn(ThreadHistoryTurnUpsert {
                    target: storage_target(target),
                    rollout_ordinal: ordinal,
                    status: storage_status(update.status),
                    error_json: update
                        .error
                        .map(|error| serde_json::to_string(&error))
                        .transpose()?,
                    started_at: update.started_at,
                    completed_at: update.completed_at,
                    duration_ms: update.duration_ms,
                }))
            }
            ThreadHistoryProjectionMutation::UpsertItem { target, item } => {
                let item_id = item.id().to_string();
                Ok(ThreadHistoryMutation::UpsertItem(ThreadHistoryItemUpsert {
                    target: storage_target(target),
                    item_id,
                    rollout_ordinal: ordinal,
                    item_created_at_ms: DateTime::parse_from_rfc3339(&line.timestamp)?
                        .timestamp_millis(),
                    item_json: serde_json::to_string(&item)?,
                }))
            }
            ThreadHistoryProjectionMutation::RemoveLatestTurns { count } => {
                Ok(ThreadHistoryMutation::RemoveLatestTurns {
                    rollout_ordinal: ordinal,
                    count,
                })
            }
        })
        .transpose()
}

fn storage_target(target: ProjectedTurnTarget) -> ThreadHistoryTurnTarget {
    match target {
        ProjectedTurnTarget::Id(turn_id) => ThreadHistoryTurnTarget::Id(turn_id),
        ProjectedTurnTarget::Active => ThreadHistoryTurnTarget::Active,
    }
}

fn storage_status(status: TurnStatus) -> ThreadHistoryTurnStatus {
    match status {
        TurnStatus::Completed => ThreadHistoryTurnStatus::Completed,
        TurnStatus::Interrupted => ThreadHistoryTurnStatus::Interrupted,
        TurnStatus::Failed => ThreadHistoryTurnStatus::Failed,
        TurnStatus::InProgress => ThreadHistoryTurnStatus::InProgress,
    }
}

fn read_rollout_line<R: BufRead>(reader: &mut R, bytes: &mut Vec<u8>) -> std::io::Result<usize> {
    let mut read = 0;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(read);
        }
        let available = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        if bytes.len().saturating_add(available) > MAX_ROLLOUT_LINE_BYTES {
            return Err(std::io::Error::other(
                "paginated rollout line exceeds the projection limit",
            ));
        }
        bytes.extend_from_slice(&buffer[..available]);
        reader.consume(available);
        read += available;
        if bytes.last() == Some(&b'\n') {
            return Ok(read);
        }
    }
}

fn empty_fingerprint() -> String {
    format!("{:x}", Sha256::digest([]))
}

fn extend_fingerprint(previous: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(previous.as_bytes());
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn source_id(path: &Path) -> String {
    format!(
        "path-sha256:{:x}",
        Sha256::digest(path.to_string_lossy().as_bytes())
    )
}

fn projection_lock_index(thread_id: &ThreadId) -> usize {
    let mut hasher = DefaultHasher::new();
    thread_id.hash(&mut hasher);
    (hasher.finish() as usize) % PROJECTION_LOCK_SHARDS
}

#[cfg(test)]
#[path = "thread_history_projection_tests.rs"]
mod tests;
