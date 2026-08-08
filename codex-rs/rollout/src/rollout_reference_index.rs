//! Indexes direct fork references found in local rollout files.

use std::collections::HashMap;
use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use codex_protocol::ThreadId;
use codex_protocol::protocol::HistoryPosition;

use crate::ARCHIVED_SESSIONS_SUBDIR;
use crate::SESSIONS_SUBDIR;
use crate::compression::RolloutFile;

/// Direct history-base edges discovered from local rollout metadata.
///
/// This is a physical-rollout index, not a logical lineage resolver. Callers use it to answer
/// cheap inverse-reference questions without each reimplementing rollout discovery.
#[derive(Debug, Default)]
pub struct RolloutReferenceIndex {
    history_base_by_thread: HashMap<ThreadId, HistoryPosition>,
    reference_counts_by_thread: HashMap<ThreadId, usize>,
    scanned_rollouts: usize,
    unreadable_rollouts: usize,
    duplicate_thread_ids: usize,
    scan_truncated: bool,
}

impl RolloutReferenceIndex {
    /// Scans active and archived local rollout metadata without a deadline.
    pub async fn scan(codex_home: &Path) -> io::Result<Self> {
        let Some(index) = Self::scan_with_deadline(
            codex_home,
            ScanDeadline::Unlimited,
            /*max_rollouts*/ None,
        )
        .await?
        else {
            return Err(io::Error::other(
                "unlimited rollout reference scan exceeded a deadline",
            ));
        };
        Ok(index)
    }

    /// Scans at most `max_rollouts` active and archived rollout files.
    ///
    /// A truncated or otherwise incomplete result remains useful for diagnostics, but callers
    /// must use [`RolloutReferenceIndex::is_complete`] before making eligibility decisions.
    pub async fn scan_bounded(codex_home: &Path, max_rollouts: usize) -> io::Result<Self> {
        let Some(index) =
            Self::scan_with_deadline(codex_home, ScanDeadline::Unlimited, Some(max_rollouts))
                .await?
        else {
            return Err(io::Error::other(
                "bounded rollout reference scan exceeded a deadline",
            ));
        };
        Ok(index)
    }

    /// Scans active and archived local rollout metadata until the worker deadline expires.
    ///
    /// Returns None instead of a partial index when the deadline expires.
    pub(crate) async fn scan_until(
        codex_home: &Path,
        started_at: Instant,
        max_runtime: Duration,
    ) -> io::Result<Option<Self>> {
        Self::scan_with_deadline(
            codex_home,
            ScanDeadline::Until {
                started_at,
                max_runtime,
            },
            /*max_rollouts*/ None,
        )
        .await
    }

    /// Returns how many other discovered rollouts directly reference thread_id.
    pub fn reference_count(&self, thread_id: ThreadId) -> usize {
        self.reference_counts_by_thread
            .get(&thread_id)
            .copied()
            .unwrap_or_default()
    }

    /// Returns the direct history-base edge for thread_id, if one was discovered.
    pub fn history_base(&self, thread_id: ThreadId) -> Option<&HistoryPosition> {
        self.history_base_by_thread.get(&thread_id)
    }

    /// Whether every discovered rollout contributed trustworthy reference metadata.
    pub fn is_complete(&self) -> bool {
        self.unreadable_rollouts == 0 && self.duplicate_thread_ids == 0 && !self.scan_truncated
    }

    /// Returns the number of rollout files considered by the scan.
    pub fn scanned_rollouts(&self) -> usize {
        self.scanned_rollouts
    }

    /// Returns the number of rollout files whose session metadata could not be read.
    pub fn unreadable_rollouts(&self) -> usize {
        self.unreadable_rollouts
    }

    /// Returns the number of rollout files that repeated an already-seen thread ID.
    pub fn duplicate_thread_ids(&self) -> usize {
        self.duplicate_thread_ids
    }

    /// Returns whether the configured rollout limit stopped the scan early.
    pub fn scan_truncated(&self) -> bool {
        self.scan_truncated
    }

    async fn scan_with_deadline(
        codex_home: &Path,
        deadline: ScanDeadline,
        max_rollouts: Option<usize>,
    ) -> io::Result<Option<Self>> {
        let mut history_base_by_thread = HashMap::new();
        let mut seen_thread_ids = HashSet::new();
        let mut scanned_rollouts = 0usize;
        let mut unreadable_rollouts = 0usize;
        let mut duplicate_thread_ids = 0usize;
        let mut scan_truncated = false;
        let mut stack = vec![
            codex_home.join(ARCHIVED_SESSIONS_SUBDIR),
            codex_home.join(SESSIONS_SUBDIR),
        ];
        while let Some(directory) = stack.pop() {
            if deadline.expired() {
                return Ok(None);
            }
            let mut entries = match tokio::fs::read_dir(directory.as_path()).await {
                Ok(entries) => entries,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            loop {
                if deadline.expired() {
                    return Ok(None);
                }
                let Some(entry) = entries.next_entry().await? else {
                    break;
                };
                let path = entry.path();
                let file_type = entry.file_type().await?;
                if file_type.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let Some(rollout_file) = RolloutFile::from_path(path) else {
                    continue;
                };
                if max_rollouts.is_some_and(|limit| scanned_rollouts >= limit) {
                    scan_truncated = true;
                    break;
                }
                scanned_rollouts = scanned_rollouts.saturating_add(1);
                let Ok(meta) = crate::read_session_meta_line(rollout_file.path()).await else {
                    unreadable_rollouts = unreadable_rollouts.saturating_add(1);
                    continue;
                };
                let thread_id = meta.meta.id;
                if !seen_thread_ids.insert(thread_id) {
                    duplicate_thread_ids = duplicate_thread_ids.saturating_add(1);
                    continue;
                }
                if let Some(history_base) = meta.meta.history_base {
                    history_base_by_thread.insert(thread_id, history_base);
                }
            }
            if scan_truncated {
                break;
            }
        }

        let mut reference_counts_by_thread = HashMap::new();
        for (thread_id, history_base) in &history_base_by_thread {
            if history_base.thread_id == *thread_id {
                continue;
            }
            *reference_counts_by_thread
                .entry(history_base.thread_id)
                .or_default() += 1;
        }
        Ok(Some(Self {
            history_base_by_thread,
            reference_counts_by_thread,
            scanned_rollouts,
            unreadable_rollouts,
            duplicate_thread_ids,
            scan_truncated,
        }))
    }
}

#[derive(Clone, Copy)]
enum ScanDeadline {
    Unlimited,
    Until {
        started_at: Instant,
        max_runtime: Duration,
    },
}

impl ScanDeadline {
    fn expired(self) -> bool {
        match self {
            Self::Unlimited => false,
            Self::Until {
                started_at,
                max_runtime,
            } => started_at.elapsed() >= max_runtime,
        }
    }
}

#[cfg(test)]
#[path = "rollout_reference_index_tests.rs"]
mod tests;
