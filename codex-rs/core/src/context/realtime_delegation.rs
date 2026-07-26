use super::ContextualUserFragment;
use codex_utils_output_truncation::approx_bytes_for_tokens;

/// Hard cap on the rendered fragment, wrapper markers included.
///
/// Callers bound the pre-escape text, but that is not the size the model sees: XML escaping
/// expands a body by up to 5x (`&` becomes `&amp;`), so a body bounded to 2K approximate tokens
/// can still render as 10K. The cap is therefore enforced on the escaped bytes that are actually
/// rendered, which keeps the whole item strictly below the 10K per-item ceiling no matter what
/// characters the realtime transcript contained.
pub(crate) const REALTIME_DELEGATION_RENDERED_TOKEN_CAP: usize = 5_000;

/// Appended in place of the content the rendered-byte cap dropped. It contains no XML metacharacters,
/// so it costs the same whether it is measured before or after escaping.
const RENDERED_TRUNCATION_MARKER: &str = "…delegation truncated…";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RealtimeDelegationSource {
    Handoff,
    TranscriptTailFlush,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RealtimeDelegation<'a> {
    input: &'a str,
    transcript_delta: Option<&'a str>,
    source: RealtimeDelegationSource,
}

impl<'a> RealtimeDelegation<'a> {
    pub(crate) fn new(
        input: &'a str,
        transcript_delta: Option<&'a str>,
        source: RealtimeDelegationSource,
    ) -> Self {
        Self {
            input,
            transcript_delta,
            source,
        }
    }
}

impl ContextualUserFragment for RealtimeDelegation<'_> {
    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<realtime_delegation>", "</realtime_delegation>")
    }

    fn body(&self) -> String {
        let source = match self.source {
            RealtimeDelegationSource::Handoff => "",
            RealtimeDelegationSource::TranscriptTailFlush => {
                "  <source>transcript_tail_flush</source>\n"
            }
        };
        let transcript_delta = self.transcript_delta.filter(|text| !text.is_empty());

        // Everything the rendered cap has to pay for besides the escaped text itself: the wrapper
        // markers `render` adds around this body, the optional source line, and the element tags.
        let (open_marker, close_marker) = Self::type_markers();
        let mut reserved =
            open_marker.len() + close_marker.len() + source.len() + "\n  <input></input>\n".len();
        if transcript_delta.is_some() {
            reserved += "  <transcript_delta></transcript_delta>\n".len();
        }
        let escaped_budget = approx_bytes_for_tokens(REALTIME_DELEGATION_RENDERED_TOKEN_CAP)
            .saturating_sub(reserved);

        let Some(transcript_delta) = transcript_delta else {
            let input = escape_xml_text_within_bytes(self.input, escaped_budget);
            return format!("\n{source}  <input>{input}</input>\n");
        };

        // Both halves grow with session length, so neither may spend the other's share.
        let input_budget = escaped_budget / 2;
        let input = escape_xml_text_within_bytes(self.input, input_budget);
        let transcript_delta =
            escape_xml_text_within_bytes(transcript_delta, escaped_budget - input_budget);
        format!(
            "\n{source}  <input>{input}</input>\n  <transcript_delta>{transcript_delta}</transcript_delta>\n"
        )
    }
}

fn xml_entity(character: char) -> Option<&'static str> {
    match character {
        '&' => Some("&amp;"),
        '<' => Some("&lt;"),
        '>' => Some("&gt;"),
        _ => None,
    }
}

fn escape_xml_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match xml_entity(character) {
            Some(entity) => escaped.push_str(entity),
            None => escaped.push(character),
        }
    }
    escaped
}

/// Escape `text` for XML while keeping the escaped result within `max_bytes`.
///
/// The budget is measured per character *after* escaping, so an input made entirely of `&`, `<`,
/// or `>` cannot expand past it. Characters are copied whole, so truncation always lands on a
/// character boundary and never inside an entity.
fn escape_xml_text_within_bytes(text: &str, max_bytes: usize) -> String {
    let escaped = escape_xml_text(text);
    if escaped.len() <= max_bytes {
        return escaped;
    }
    if max_bytes < RENDERED_TRUNCATION_MARKER.len() {
        return String::new();
    }

    let content_budget = max_bytes - RENDERED_TRUNCATION_MARKER.len();
    let mut truncated = String::with_capacity(max_bytes);
    for character in text.chars() {
        let entity = xml_entity(character);
        let escaped_len = entity.map_or_else(|| character.len_utf8(), str::len);
        if truncated.len() + escaped_len > content_budget {
            break;
        }
        match entity {
            Some(entity) => truncated.push_str(entity),
            None => truncated.push(character),
        }
    }
    truncated.push_str(RENDERED_TRUNCATION_MARKER);
    truncated
}
