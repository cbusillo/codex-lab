use super::common::SessionFileCandidate;
use super::common::detect_recent_sessions;
use crate::model::ExternalAgentSessionImportLimits;
use crate::sessions::ExternalAgentSessionMigration;
use crate::sessions::SessionRecordFormat;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

const CUR_PROJECT_PATH_LIMITS: CurProjectPathLimits = CurProjectPathLimits {
    max_path_probes: 128,
    max_scanned_entries: 4_096,
};

#[derive(Clone, Copy)]
struct CurProjectPathLimits {
    max_path_probes: usize,
    max_scanned_entries: usize,
}

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
        let transcripts = cur_transcript_files(&project_storage.join("agent-transcripts"));
        if transcripts.is_empty() {
            continue;
        }
        let fallback_cwd = cur_project_cwd(&project_storage, external_agent_home);
        for path in transcripts {
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
    #[cfg(not(windows))]
    let path = PathBuf::from("/");

    #[cfg(windows)]
    let (encoded, path) = {
        let (drive, encoded) = decode_cur_windows_project_drive(encoded)?;
        (encoded, PathBuf::from(format!("{drive}:\\")))
    };

    let encoded = encoded.strip_prefix('-').unwrap_or(encoded);
    if encoded.split('-').any(|component| {
        component.is_empty()
            || matches!(component, "." | "..")
            || component.contains(['/', '\\', ':'])
    }) {
        return None;
    }
    let mut probes = 0;
    let mut scanned_entries = 0;
    decode_cur_project_path_from(
        &path,
        encoded,
        &mut probes,
        &mut scanned_entries,
        CUR_PROJECT_PATH_LIMITS,
    )
    .ok()
    .flatten()
}

fn decode_cur_project_path_from(
    parent: &Path,
    encoded: &str,
    probes: &mut usize,
    scanned_entries: &mut usize,
    limits: CurProjectPathLimits,
) -> Result<Option<PathBuf>, ()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(parent).map_err(|_| ())? {
        if *scanned_entries >= limits.max_scanned_entries {
            return Err(());
        }
        *scanned_entries += 1;
        entries.push(entry.map_err(|_| ())?);
    }
    entries.sort_by_key(fs::DirEntry::file_name);
    let mut matched_path = None;
    for entry in entries {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let mut normalized = String::with_capacity(name.len());
        let mut previous_was_separator = false;
        for character in name.chars() {
            let is_separator = matches!(character, '-' | '_' | '.' | ' ' | '+' | '@' | '&');
            if is_separator {
                if !previous_was_separator {
                    normalized.push('-');
                }
            } else {
                normalized.push(character);
            }
            previous_was_separator = is_separator;
        }

        for component in [
            Some(name.as_str()),
            (normalized != name).then_some(normalized.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            let remaining = strip_cur_project_component(encoded, component);
            let Some(remaining) = remaining else {
                continue;
            };
            if *probes >= limits.max_path_probes {
                return Err(());
            }
            *probes += 1;
            let candidate = if remaining.is_empty() {
                Some(path.clone())
            } else {
                decode_cur_project_path_from(&path, remaining, probes, scanned_entries, limits)?
            };
            if let Some(candidate) = candidate {
                if matched_path
                    .as_ref()
                    .is_some_and(|matched_path| matched_path != &candidate)
                {
                    return Err(());
                }
                matched_path = Some(candidate);
            }
        }
    }
    Ok(matched_path)
}

fn strip_cur_project_component<'a>(encoded: &'a str, component: &str) -> Option<&'a str> {
    #[cfg(windows)]
    let remaining = {
        let prefix = encoded.get(..component.len())?;
        if !prefix.eq_ignore_ascii_case(component) {
            return None;
        }
        encoded.get(component.len()..)?
    };

    #[cfg(not(windows))]
    let remaining = encoded.strip_prefix(component)?;

    if remaining.is_empty() {
        Some(remaining)
    } else {
        remaining.strip_prefix('-')
    }
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
