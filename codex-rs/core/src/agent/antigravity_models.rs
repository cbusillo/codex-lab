use super::external_diagnostics::ExternalAgentFailureDetail;
use super::external_diagnostics::ExternalAgentFailureKind;
use std::collections::HashSet;

pub(super) const MAX_DISCOVERED_MODELS: usize = 32;
const MAX_MODEL_NAME_BYTES: usize = 128;

pub(crate) fn is_valid_antigravity_model_name(model: &str) -> bool {
    !model.is_empty()
        && !model.starts_with('-')
        && !model.ends_with(['.', ':'])
        && model.len() <= MAX_MODEL_NAME_BYTES
        && model
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.:/+".contains(character))
}

pub(super) fn parse_antigravity_models(
    output: &[u8],
) -> Result<Vec<String>, ExternalAgentFailureDetail> {
    let output = String::from_utf8_lossy(output);
    let mut models = Vec::new();
    let mut first_rejected = None;
    let mut rejected = 0;
    let mut seen = HashSet::new();

    let lines = output.lines().map(strip_ansi).collect::<Vec<_>>();
    let table_header_index = lines
        .iter()
        .position(|line| line.contains('|') || line.contains('│'))
        .filter(|index| {
            lines
                .get(index + 1)
                .is_some_and(|line| is_table_header_separator(line.trim()))
        });
    for (index, line) in lines.iter().enumerate() {
        let Some(candidate) = normalize_model_line(line) else {
            continue;
        };
        if table_header_index == Some(index) {
            continue;
        }
        if !is_valid_antigravity_model_name(&candidate) {
            rejected += 1;
            first_rejected.get_or_insert(candidate);
            continue;
        }
        if seen.insert(candidate.to_ascii_lowercase()) {
            models.push(candidate);
        }
        if models.len() > MAX_DISCOVERED_MODELS {
            return Err(ExternalAgentFailureDetail::new(
                ExternalAgentFailureKind::MalformedOutput,
                format!(
                    "Antigravity model discovery exceeded the {MAX_DISCOVERED_MODELS}-model limit; rule: `agy models` must return at most {MAX_DISCOVERED_MODELS} canonical model identifiers. Remediation: update or reconfigure Antigravity, then refresh capabilities."
                ),
            ));
        }
    }

    if models.is_empty() {
        let (kind, message) = if rejected == 0 {
            (
                ExternalAgentFailureKind::EmptyOutput,
                "Antigravity model discovery returned no model identifiers. Remediation: sign in to Antigravity, confirm `agy models` returns canonical identifiers, then refresh capabilities.".to_string(),
            )
        } else {
            let selector = first_rejected
                .as_deref()
                .map(|candidate| format!("`antigravity-{}`", bounded_selector_candidate(candidate)))
                .unwrap_or_else(|| "an unavailable selector".to_string());
            (
                ExternalAgentFailureKind::MalformedOutput,
                format!(
                    "Antigravity selector {selector} was rejected during model discovery ({rejected} malformed identifier(s)); rule: each `agy models` entry must be a canonical identifier using only letters, digits, and `-_.:/+`. Remediation: update or reconfigure Antigravity, then refresh capabilities."
                ),
            )
        };
        return Err(ExternalAgentFailureDetail::new(kind, message));
    }

    Ok(models)
}

fn bounded_selector_candidate(candidate: &str) -> String {
    candidate.chars().take(MAX_MODEL_NAME_BYTES).collect()
}

fn normalize_model_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || is_table_decoration(line) {
        return None;
    }
    let line = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("• "))
        .or_else(|| line.strip_prefix("│ "))
        .unwrap_or(line)
        .trim();
    let line = line.trim_matches(['|', '│']).trim();
    let line = line.split(['|', '│', '\t']).next().unwrap_or(line).trim();
    let line = line
        .split_once("  ")
        .map_or(line, |(candidate, _)| candidate);
    let line = line
        .strip_suffix("(default)")
        .or_else(|| line.strip_suffix("[default]"))
        .unwrap_or(line)
        .trim();
    (!line.is_empty() && !is_model_heading(line)).then(|| line.to_string())
}

fn is_model_heading(value: &str) -> bool {
    let value = value.trim_end_matches(':');
    matches!(
        value.to_ascii_lowercase().as_str(),
        "alias"
            | "available"
            | "available models"
            | "current model"
            | "default"
            | "description"
            | "done"
            | "family"
            | "id"
            | "model"
            | "model id"
            | "model name"
            | "models"
            | "models available"
            | "name"
            | "none"
            | "provider"
            | "slug"
            | "supported models"
            | "loading"
            | "unauthenticated"
    )
}

fn is_table_decoration(line: &str) -> bool {
    line.chars().all(|character| {
        character.is_whitespace()
            || matches!(
                character,
                '-' | '+'
                    | '|'
                    | '│'
                    | '─'
                    | '┌'
                    | '┬'
                    | '┐'
                    | '├'
                    | '┼'
                    | '┤'
                    | '└'
                    | '┴'
                    | '┘'
            )
    })
}

fn is_table_header_separator(line: &str) -> bool {
    is_table_decoration(line)
        && (line.contains('-')
            || line
                .chars()
                .any(|character| matches!(character, '├' | '┼' | '┤')))
}

fn strip_ansi(value: &str) -> String {
    let mut stripped = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            match characters.peek() {
                Some('[') => {
                    characters.next();
                    for character in characters.by_ref() {
                        if character.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some('(' | ')') => {
                    characters.next();
                    characters.next();
                }
                Some(']' | 'P') => {
                    characters.next();
                    while let Some(character) = characters.next() {
                        if character == '\u{7}' {
                            break;
                        }
                        if character == '\u{1b}' && characters.peek() == Some(&'\\') {
                            characters.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else {
            stripped.push(character);
        }
    }
    stripped
}
