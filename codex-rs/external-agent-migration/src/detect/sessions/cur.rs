use super::common::SessionFileCandidate;
use super::common::detect_recent_sessions;
use crate::model::ExternalAgentSessionImportLimits;
use crate::sessions::ExternalAgentSessionMigration;
use crate::sessions::SessionRecordFormat;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

// This limits directory enumerations (`fs::read_dir` calls), not metadata probes.
const MAX_CUR_PROJECT_PATH_PROBES: usize = 128;
const MAX_CUR_PROJECT_PATH_ENTRIES: usize = 8_192;
const MAX_CUR_PROJECT_PATH_FRONTIER: usize = 1_024;
const MAX_CUR_PROJECT_PATH_INPUT_BYTES: usize = 4_096;

#[derive(Clone, Copy)]
struct CurProjectPathSearchLimits {
    max_directory_probes: usize,
    max_entries_scanned: usize,
    max_frontier_states: usize,
    max_input_bytes: usize,
}

const CUR_PROJECT_PATH_SEARCH_LIMITS: CurProjectPathSearchLimits = CurProjectPathSearchLimits {
    max_directory_probes: MAX_CUR_PROJECT_PATH_PROBES,
    max_entries_scanned: MAX_CUR_PROJECT_PATH_ENTRIES,
    max_frontier_states: MAX_CUR_PROJECT_PATH_FRONTIER,
    max_input_bytes: MAX_CUR_PROJECT_PATH_INPUT_BYTES,
};

pub fn detect_recent_cur_sessions(
    external_agent_home: &Path,
    codex_home: &Path,
) -> io::Result<Vec<ExternalAgentSessionMigration>> {
    detect_recent_cur_sessions_with_limits(
        external_agent_home,
        codex_home,
        ExternalAgentSessionImportLimits::default(),
    )
}

pub(crate) fn detect_recent_cur_sessions_with_limits(
    external_agent_home: &Path,
    codex_home: &Path,
    limits: ExternalAgentSessionImportLimits,
) -> io::Result<Vec<ExternalAgentSessionMigration>> {
    let projects_root = external_agent_home.join("projects");
    if !projects_root.is_dir() {
        return Ok(Vec::new());
    }

    let mut candidates = Vec::new();
    for project_entry in fs::read_dir(projects_root)? {
        let Ok(project_entry) = project_entry else {
            continue;
        };
        let project_storage = project_entry.path();
        if !project_storage.is_dir() {
            continue;
        }
        let fallback_cwd = cur_project_cwd(&project_storage, external_agent_home);
        for path in cur_transcript_files(&project_storage.join("agent-transcripts")) {
            candidates.push(SessionFileCandidate {
                path,
                fallback_cwd: fallback_cwd.clone(),
                record_format: SessionRecordFormat::Cur,
            });
        }
    }
    detect_recent_sessions(
        codex_home, candidates, /*require_existing_cwd*/ false, limits,
    )
}

fn cur_transcript_files(transcripts_root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![transcripts_root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if entry.file_name() != "subagents" {
                    pending.push(path);
                }
            } else if file_type.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn cur_project_cwd(project_storage: &Path, external_agent_home: &Path) -> Option<PathBuf> {
    let encoded = project_storage.file_name()?.to_str()?;
    // Cursor stores projectless chats under this reserved project name.
    if encoded == "empty-window" {
        let external_agent_home = if external_agent_home.is_absolute() {
            external_agent_home.to_path_buf()
        } else {
            std::env::current_dir().ok()?.join(external_agent_home)
        };
        return external_agent_home.parent().map(Path::to_path_buf);
    }
    decode_cur_project_path(encoded)
}

fn decode_cur_project_path(encoded: &str) -> Option<PathBuf> {
    decode_cur_project_path_with_limits(encoded, CUR_PROJECT_PATH_SEARCH_LIMITS)
}

fn decode_cur_project_path_with_limits(
    encoded: &str,
    limits: CurProjectPathSearchLimits,
) -> Option<PathBuf> {
    if encoded.len() > limits.max_input_bytes {
        return None;
    }

    #[cfg(not(windows))]
    let root = PathBuf::from("/");

    #[cfg(windows)]
    let (encoded, root) = {
        let (drive, encoded) = decode_cur_windows_project_drive(encoded)?;
        (encoded, PathBuf::from(format!("{drive}:\\")))
    };

    let encoded = encoded.strip_prefix('-').unwrap_or(encoded);
    for component in encoded.split('-') {
        if component.is_empty()
            || matches!(component, "." | "..")
            || component.contains(['/', '\\', ':'])
        {
            return None;
        }
    }

    resolve_cur_project_path(root, encoded, limits)
}

fn resolve_cur_project_path(
    root: PathBuf,
    encoded: &str,
    limits: CurProjectPathSearchLimits,
) -> Option<PathBuf> {
    if limits.max_frontier_states == 0 {
        return None;
    }

    let mut matched_path = None;
    let mut directory_probes = 0;
    let mut entries_scanned = 0;
    let mut pending = vec![(root, 0)];

    while let Some((directory, offset)) = pending.pop() {
        if directory_probes >= limits.max_directory_probes {
            return None;
        }
        directory_probes += 1;
        let entries = fs::read_dir(&directory).ok()?;
        let remaining = encoded.get(offset..)?;

        for entry in entries {
            if entries_scanned >= limits.max_entries_scanned {
                return None;
            }
            entries_scanned += 1;
            let entry = entry.ok()?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };

            let mut match_ends = Vec::with_capacity(2);
            if let Some(match_end) =
                native_cur_component_match_end(&directory, &entry.path(), remaining, file_name)
            {
                match_ends.push(match_end);
            }
            let normalized = normalize_cur_project_component(file_name);
            if normalized != file_name
                && let Some(match_end) = native_normalized_cur_component_match_end(
                    &directory,
                    &entry.path(),
                    remaining,
                    file_name,
                    &normalized,
                )
                && !match_ends.contains(&match_end)
            {
                match_ends.push(match_end);
            }
            if match_ends.is_empty() {
                continue;
            }

            let path = entry.path();
            let file_type = entry.file_type().ok()?;
            let is_directory = if file_type.is_symlink() {
                fs::metadata(&path).ok()?.is_dir()
            } else {
                file_type.is_dir()
            };
            if !is_directory {
                continue;
            }

            for match_end in match_ends {
                let next_offset = offset + match_end;
                if next_offset == encoded.len() {
                    if matched_path
                        .as_ref()
                        .is_some_and(|matched_path| matched_path != &path)
                    {
                        return None;
                    }
                    matched_path = Some(path.clone());
                } else {
                    if pending.len() >= limits.max_frontier_states {
                        return None;
                    }
                    pending.push((path.clone(), next_offset));
                }
            }
        }
    }

    matched_path
}

fn native_cur_component_match_end(
    directory: &Path,
    entry_path: &Path,
    encoded: &str,
    spelling: &str,
) -> Option<usize> {
    if let Some(match_end) = cur_component_match_end(encoded, spelling) {
        return Some(match_end);
    }

    let candidate = encoded.get(..spelling.len())?;
    let match_end = cur_component_match_end(encoded, candidate)?;
    if !candidate.eq_ignore_ascii_case(spelling)
        || fs::canonicalize(directory.join(candidate)).ok()? != fs::canonicalize(entry_path).ok()?
    {
        return None;
    }
    Some(match_end)
}

fn native_normalized_cur_component_match_end(
    directory: &Path,
    entry_path: &Path,
    encoded: &str,
    file_name: &str,
    normalized: &str,
) -> Option<usize> {
    if let Some(match_end) = cur_component_match_end(encoded, normalized) {
        return Some(match_end);
    }

    let candidate = encoded.get(..normalized.len())?;
    let match_end = cur_component_match_end(encoded, candidate)?;
    if !candidate.eq_ignore_ascii_case(normalized) {
        return None;
    }
    let native_candidate = cur_component_with_encoded_ascii_case(file_name, candidate)?;
    if fs::canonicalize(directory.join(native_candidate)).ok()?
        != fs::canonicalize(entry_path).ok()?
    {
        return None;
    }
    Some(match_end)
}

fn cur_component_with_encoded_ascii_case(component: &str, encoded: &str) -> Option<String> {
    let mut encoded = encoded.chars();
    let mut native = String::with_capacity(component.len());
    let mut in_punctuation_run = false;
    for character in component.chars() {
        if matches!(character, '-' | '_' | '.' | ' ' | '+' | '@' | '&') {
            if !in_punctuation_run && encoded.next()? != '-' {
                return None;
            }
            native.push(character);
            in_punctuation_run = true;
        } else {
            let encoded_character = encoded.next()?;
            if character == encoded_character {
                native.push(character);
            } else if character.is_ascii_alphabetic()
                && encoded_character.is_ascii_alphabetic()
                && character.eq_ignore_ascii_case(&encoded_character)
            {
                native.push(encoded_character);
            } else {
                return None;
            }
            in_punctuation_run = false;
        }
    }
    if encoded.next().is_some() {
        return None;
    }
    Some(native)
}

fn cur_component_match_end(encoded: &str, spelling: &str) -> Option<usize> {
    let trailing = encoded.strip_prefix(spelling)?;
    if trailing.is_empty() {
        Some(spelling.len())
    } else if trailing.starts_with('-') {
        Some(spelling.len() + 1)
    } else {
        None
    }
}

fn normalize_cur_project_component(component: &str) -> String {
    let mut normalized = String::with_capacity(component.len());
    let mut in_punctuation_run = false;
    for character in component.chars() {
        if matches!(character, '-' | '_' | '.' | ' ' | '+' | '@' | '&') {
            if !in_punctuation_run {
                normalized.push('-');
            }
            in_punctuation_run = true;
        } else {
            normalized.push(character);
            in_punctuation_run = false;
        }
    }
    normalized
}

#[cfg(any(windows, test))]
fn decode_cur_windows_project_drive(encoded: &str) -> Option<(char, &str)> {
    let drive = encoded.as_bytes().first().copied()?;
    if !drive.is_ascii_alphabetic() || encoded.as_bytes().get(1) != Some(&b'-') {
        return None;
    }

    Some((char::from(drive), encoded.get(2..)?))
}

#[cfg(test)]
#[path = "cur_tests.rs"]
mod tests;
