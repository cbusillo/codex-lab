//! Frozen, complete AGENTS.md delivery for Background Review.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;

use codex_exec_server::ReadFileOptions;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_utils_path_uri::PathUri;
use sha1::Digest;
use sha1::Sha1;

use crate::agents_md::agents_md_paths;
use crate::agents_md::candidate_filenames;
use crate::agents_md::project_root_for;
use crate::client_common::Prompt;
use crate::context::ContextualUserFragment;
use crate::context::world_state::REVIEW_AGENTS_MD_KIND;
use crate::context::world_state::REVIEW_AGENTS_MD_PARTS;
use crate::context::world_state::REVIEW_AGENTS_MD_TOTAL_BYTES;
use crate::context::world_state::ReviewAgentsMdFragment;
use crate::context::world_state::ReviewAgentsMdSnapshot;
use crate::session::step_context::StepContext;

const MAX_REVIEW_INSTRUCTION_SOURCES: usize = 64;
const MAX_REVIEW_TARGET_DIRECTORIES: usize = 1024;
const REVIEW_PART_BODY_BYTES: usize = 8 * 1024;

/// Shared with the parent review task so a request denial persists its exact reason.
#[derive(Clone, Debug, Default)]
pub(crate) struct BackgroundReviewInstructionsGate {
    paths: Vec<PathUri>,
    state: Arc<Mutex<GateState>>,
}

#[derive(Debug, Default)]
struct GateState {
    failure: Option<String>,
    prepared: Option<Arc<PreparedReviewInstructions>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreparedReviewInstructions {
    parts: Vec<ReviewAgentsMdSnapshot>,
    sources: Vec<PreparedSource>,
    fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreparedSource {
    path: String,
    digest: String,
}

#[derive(Clone, Debug)]
struct ReviewSourceBlock {
    header: String,
    contents: String,
}

impl BackgroundReviewInstructionsGate {
    pub(crate) fn new(paths: Vec<PathUri>) -> Self {
        Self {
            paths,
            ..Self::default()
        }
    }

    pub(crate) fn failure(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failure
            .clone()
    }

    /// Prepares the immutable instruction set before world-state rendering.
    pub(crate) async fn prepared_parts(
        &self,
        step: &StepContext,
    ) -> CodexResult<Vec<ReviewAgentsMdSnapshot>> {
        self.prepare(step)
            .await
            .map(|prepared| prepared.parts.clone())
            .map_err(|reason| self.deny(reason))
    }

    pub(crate) async fn authorize_request(
        &self,
        step: &StepContext,
        prompt: &Prompt,
    ) -> CodexResult<()> {
        let prepared = self
            .prepare(step)
            .await
            .map_err(|reason| self.deny(reason))?;
        let observed = self
            .collect(step)
            .await
            .map_err(|reason| self.deny(reason))?;
        if observed.fingerprint != prepared.fingerprint || observed.sources != prepared.sources {
            return Err(
                self.deny("applicable instruction sources changed after review preparation")
            );
        }
        validate_review_parts(&prepared.parts, prompt).map_err(|reason| self.deny(reason))
    }

    async fn prepare(&self, step: &StepContext) -> Result<Arc<PreparedReviewInstructions>, String> {
        if let Some(prepared) = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .prepared
            .clone()
        {
            return Ok(prepared);
        }
        let prepared = Arc::new(self.collect(step).await?);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(state
            .prepared
            .get_or_insert_with(|| Arc::clone(&prepared))
            .clone())
    }

    fn deny(&self, reason: impl Into<String>) -> CodexErr {
        let message = format!(
            "Background Review cannot verify complete AGENTS.md instructions: {}. The review request was not sent.",
            reason.into()
        );
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failure = Some(message.clone());
        CodexErr::InvalidRequest(message)
    }

    async fn collect(&self, step: &StepContext) -> Result<PreparedReviewInstructions, String> {
        let loaded = step.loaded_agents_md.as_deref();
        if loaded.is_some_and(|loaded| !loaded.is_complete()) {
            return Err("instruction loading was truncated or failed".to_string());
        }
        if self.paths.is_empty() {
            return Err("the exact changed paths are unavailable".to_string());
        }
        let environment = step
            .environments
            .primary()
            .ok_or_else(|| "the review environment is unavailable".to_string())?;
        if step.environments.turn_environments().count() != 1 {
            return Err("multiple review environments are unsupported".to_string());
        }
        let filesystem = environment.environment.get_filesystem();
        let sandbox = (!environment
            .permission_profile()
            .file_system_sandbox_policy()
            .has_full_disk_read_access())
        .then(|| environment.sandbox_context(/*additional_permissions*/ None));
        let names = candidate_filenames(&step.turn.config, environment.cwd());
        if self.paths.iter().any(|path| {
            path.to_path_buf()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| names.contains(&name))
        }) {
            return Err(
                "the turn changes instruction files whose baseline scope is not available"
                    .to_string(),
            );
        }

        let mut directories = self
            .paths
            .iter()
            .map(|path| {
                path.parent().ok_or_else(|| {
                    "a changed path is outside the local review environment".to_string()
                })
            })
            .collect::<Result<HashSet<_>, _>>()?
            .into_iter()
            .collect::<Vec<_>>();
        directories.sort_by_key(PathUri::to_string);
        if directories.len() > MAX_REVIEW_TARGET_DIRECTORIES {
            return Err(omission_reason(
                "too many changed target directories",
                directories.first(),
                directories.len() - MAX_REVIEW_TARGET_DIRECTORIES,
            ));
        }
        if directories
            .iter()
            .any(|directory| directory.to_abs_path().is_err())
        {
            return Err("a changed path is outside the local review environment".to_string());
        }

        let expected_root = project_root_for(
            &step.turn.config,
            environment.cwd(),
            filesystem.as_ref(),
            sandbox.as_ref(),
            codex_file_system::FindUpErrorPolicy::Propagate,
        )
        .await
        .map_err(|_| "applicable instruction files could not be discovered".to_string())?;
        let mut paths = HashSet::new();
        for directory in &directories {
            let root = project_root_for(
                &step.turn.config,
                directory,
                filesystem.as_ref(),
                sandbox.as_ref(),
                codex_file_system::FindUpErrorPolicy::Propagate,
            )
            .await
            .map_err(|_| "applicable instruction files could not be discovered".to_string())?;
            if root != expected_root {
                return Err("a changed target belongs to an unsupported project root".to_string());
            }
            for path in agents_md_paths(
                &step.turn.config,
                directory,
                filesystem.as_ref(),
                sandbox.as_ref(),
                codex_file_system::FindUpErrorPolicy::Propagate,
            )
            .await
            .map_err(|_| "applicable instruction files could not be discovered".to_string())?
            {
                paths.insert(path);
            }
        }
        let mut paths = paths.into_iter().collect::<Vec<_>>();
        paths.sort_by(|left, right| {
            path_depth(left)
                .cmp(&path_depth(right))
                .then_with(|| left.to_string().cmp(&right.to_string()))
        });
        if paths.len() > MAX_REVIEW_INSTRUCTION_SOURCES {
            return Err(omission_reason(
                "too many applicable instruction sources",
                paths.get(MAX_REVIEW_INSTRUCTION_SOURCES),
                paths.len() - MAX_REVIEW_INSTRUCTION_SOURCES,
            ));
        }

        let mut source_blocks = Vec::new();
        let mut sources = Vec::new();
        if let Some(loaded) = loaded {
            let global = loaded.non_project_text();
            if !global.trim().is_empty() {
                source_blocks.push(source_block(0, "global", "host", &global));
                sources.push(PreparedSource {
                    path: "<global>".to_string(),
                    digest: digest(&global),
                });
            }
        }
        let mut raw_bytes = 0usize;
        for (index, path) in paths.iter().enumerate() {
            let bytes = filesystem
                .read_file(path, ReadFileOptions::default(), sandbox.as_ref())
                .await
                .map_err(|_| {
                    omission_reason(
                        "applicable instruction source could not be read",
                        Some(path),
                        1,
                    )
                })?;
            raw_bytes = raw_bytes.saturating_add(bytes.len());
            if raw_bytes > step.turn.config.project_doc_max_bytes {
                return Err(omission_reason(
                    "instruction loading was truncated or failed",
                    Some(path),
                    1,
                ));
            }
            let contents = std::str::from_utf8(&bytes).map_err(|_| {
                omission_reason("instruction loading was truncated or failed", Some(path), 1)
            })?;
            let scope = path
                .parent()
                .map(|scope| scope.inferred_native_path_string())
                .unwrap_or_else(|| path.to_string());
            source_blocks.push(source_block(index + 1, &scope, &path.to_string(), contents));
            sources.push(PreparedSource {
                path: path.to_string(),
                digest: digest(contents),
            });
        }
        let parts = pack_parts(source_blocks, &sources)?;
        let fingerprint = source_fingerprint(&sources);
        Ok(PreparedReviewInstructions {
            parts,
            sources,
            fingerprint,
        })
    }
}

fn pack_parts(
    source_blocks: Vec<ReviewSourceBlock>,
    sources: &[PreparedSource],
) -> Result<Vec<ReviewAgentsMdSnapshot>, String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for block in source_blocks {
        let maximum_contents = REVIEW_PART_BODY_BYTES
            .saturating_sub(block.header.len())
            .saturating_sub(64)
            .max(1);
        let pieces = split_utf8(&block.contents, maximum_contents);
        let part_count = pieces.len();
        for (continuation, piece) in pieces.into_iter().enumerate() {
            let header = if part_count == 1 {
                block.header.clone()
            } else {
                format!(
                    "{}; continuation {}/{}",
                    block.header,
                    continuation + 1,
                    part_count
                )
            };
            let piece = format!("{header}\n<INSTRUCTIONS>\n{piece}\n</INSTRUCTIONS>\n");
            if !current.is_empty()
                && current.len().saturating_add(piece.len()) > REVIEW_PART_BODY_BYTES
            {
                chunks.push(std::mem::take(&mut current));
            }
            current.push_str(&piece);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.len() > REVIEW_AGENTS_MD_PARTS {
        return Err(omission_reason(
            "complete instructions exceed the Background Review part capacity",
            None,
            chunks.len() - REVIEW_AGENTS_MD_PARTS,
        ));
    }
    let digest = source_fingerprint(sources);
    let total = chunks.len();
    let parts = chunks
        .into_iter()
        .enumerate()
        .map(|(ordinal, text)| ReviewAgentsMdSnapshot {
            ordinal,
            total,
            digest: digest.clone(),
            text,
        })
        .collect::<Vec<_>>();
    let rendered_bytes = parts
        .iter()
        .map(|part| ReviewAgentsMdFragment(part.clone()).render().len())
        .sum::<usize>();
    if rendered_bytes > REVIEW_AGENTS_MD_TOTAL_BYTES
        || parts
            .iter()
            .any(|part| ReviewAgentsMdFragment(part.clone()).render().len() > 9 * 1024)
    {
        return Err(
            "complete instructions exceed the Background Review context budget".to_string(),
        );
    }
    Ok(parts)
}

fn split_utf8(text: &str, max_bytes: usize) -> Vec<String> {
    if text.len() <= max_bytes {
        return vec![text.to_string()];
    }
    let mut pieces = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let mut end = remaining.len().min(max_bytes);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        if let Some(newline) = remaining[..end].rfind('\n').filter(|newline| *newline > 0) {
            end = newline + 1;
        }
        pieces.push(remaining[..end].to_string());
        remaining = &remaining[end..];
    }
    pieces
}

fn source_block(rank: usize, scope: &str, source: &str, contents: &str) -> ReviewSourceBlock {
    ReviewSourceBlock {
        header: format!("source rank {rank}; applies to files under `{scope}`; source `{source}`"),
        contents: contents.to_string(),
    }
}

fn path_depth(path: &PathUri) -> usize {
    path.to_string()
        .bytes()
        .filter(|byte| *byte == b'/')
        .count()
}

fn digest(text: &str) -> String {
    format!("{:x}", Sha1::digest(text.as_bytes()))
}

fn source_fingerprint(sources: &[PreparedSource]) -> String {
    digest(
        &sources
            .iter()
            .map(|source| format!("{}:{}", source.path, source.digest))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn omission_reason(category: &str, path: Option<&PathUri>, count: usize) -> String {
    let scope = path
        .map(PathUri::to_string)
        .unwrap_or_else(|| "<review scope>".to_string());
    let mut reason = format!("{category}: `{scope}` ({count} omitted)");
    if reason.len() > 512 {
        let mut boundary = 512;
        while !reason.is_char_boundary(boundary) {
            boundary -= 1;
        }
        reason.truncate(boundary);
    }
    reason
}

fn validate_review_parts(parts: &[ReviewAgentsMdSnapshot], prompt: &Prompt) -> Result<(), String> {
    if parts.is_empty() {
        return Ok(());
    }
    let expected = parts
        .iter()
        .map(|part| (part.ordinal, ReviewAgentsMdFragment(part.clone()).render()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut found = std::collections::BTreeMap::new();
    for item in &prompt.input {
        let ResponseItem::Message {
            role,
            content,
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } = item
        else {
            continue;
        };
        if role != "user" {
            continue;
        }
        for (index, item) in content.iter().enumerate() {
            if metadata
                .content_item_kinds
                .as_ref()
                .and_then(|kinds| kinds.get(index))
                .is_none_or(|kind| kind.0 != REVIEW_AGENTS_MD_KIND)
            {
                continue;
            }
            let ContentItem::InputText { text } = item else {
                return Err("a review instruction part has invalid trusted metadata".to_string());
            };
            let Some((ordinal, _)) = expected.iter().find(|(_, expected)| *expected == text) else {
                return Err("a review instruction part is missing, truncated, or conflicts with the prepared set".to_string());
            };
            if let Some(previous) = found.insert(*ordinal, text)
                && previous != text
            {
                return Err("a review instruction part conflicts with the prepared set".to_string());
            }
        }
    }
    if found.len() != expected.len() || !expected.keys().all(|ordinal| found.contains_key(ordinal))
    {
        return Err(
            "the complete instruction fragment is absent from the final request".to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "background_review_instructions_tests.rs"]
mod tests;
