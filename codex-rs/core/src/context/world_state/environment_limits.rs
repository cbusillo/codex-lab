//! Hard caps for the model-visible `<environment_context>` fragment.
//!
//! The fragment is rebuilt from live host state (selected environments, workspace roots, network
//! rules, subagent roster), all of which are attacker- or misconfiguration-reachable. Every input
//! is bounded individually so the fragment stays readable, and the whole rendered body is bounded
//! again so no combination of inputs can push a single model-context item past the 10K-token limit.

/// Maximum number of environments rendered in one fragment, including `unavailable` entries for
/// environments that disappeared since the previous render.
pub(crate) const MAX_RENDERED_ENVIRONMENTS: usize =
    codex_protocol::protocol::MAX_TURN_ENVIRONMENT_SELECTIONS * 2;

/// Maximum number of `<root>` entries rendered for the filesystem workspace roots.
pub(crate) const MAX_RENDERED_WORKSPACE_ROOTS: usize = 32;

/// Maximum number of domains rendered in each of `<allowed>` and `<denied>`.
pub(crate) const MAX_RENDERED_NETWORK_DOMAINS: usize = 64;

/// Maximum number of `<subagents>` lines rendered.
pub(crate) const MAX_RENDERED_SUBAGENT_LINES: usize = 64;

/// Hard byte cap for the rendered `<environment_context>` body.
///
/// At the repo's ~4-bytes-per-token approximation this is well under 10K tokens even for
/// pathological single-byte-token content.
pub(crate) const MAX_ENVIRONMENT_CONTEXT_BODY_BYTES: usize = 16_384;

const TRUNCATION_NOTICE: &str =
    "\n  <truncated>environment context exceeded its size limit</truncated>\n";

/// Truncate `body` to [`MAX_ENVIRONMENT_CONTEXT_BODY_BYTES`] on a UTF-8 boundary, appending a
/// notice so the model is told the fragment is incomplete rather than silently reading a
/// half-written element.
pub(crate) fn bound_environment_context_body(body: String) -> String {
    if body.len() <= MAX_ENVIRONMENT_CONTEXT_BODY_BYTES {
        return body;
    }

    let keep = MAX_ENVIRONMENT_CONTEXT_BODY_BYTES - TRUNCATION_NOTICE.len();
    let mut boundary = keep;
    while boundary > 0 && !body.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let mut bounded = body[..boundary].to_string();
    bounded.push_str(TRUNCATION_NOTICE);
    bounded
}

/// Truncate `values` to `max` entries, keeping the deterministic leading prefix.
pub(crate) fn bound_entries<T>(mut values: Vec<T>, max: usize) -> Vec<T> {
    values.truncate(max);
    values
}

#[cfg(test)]
#[path = "environment_limits_tests.rs"]
mod tests;
