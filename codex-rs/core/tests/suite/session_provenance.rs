//! Regression coverage for the `SessionProvenance` / `SessionSource` split.
//!
//! The TUI derives a `SessionProvenance` from the `AGENT_SESSION_*` environment
//! contract (see `tui/src/agent_session_env.rs`) while always reporting
//! `SessionSource::Cli`, because provenance answers "who asked for this work"
//! and `SessionSource` answers "which surface is running it". Collapsing the two
//! would mislabel every agent-launched CLI session, so this test pins the
//! durable rollout metadata for exactly that combination.

use anyhow::Context;
use anyhow::Result;
use codex_core::StartThreadOptions;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::SessionProvenance;
use codex_protocol::protocol::SessionSource;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;

/// Mirrors what `session_provenance_from_agent_env` produces for a launcher that
/// exports `AGENT_SESSION_ORIGIN` plus the rest of the generic contract.
fn agent_session_provenance() -> SessionProvenance {
    SessionProvenance {
        request_id: Some("agent-session-123".to_string()),
        repository: Some("cbusillo/codex-lab".to_string()),
        issue_number: Some(48),
        issue_url: Some("https://github.com/cbusillo/codex-lab/issues/48".to_string()),
        source: Some("agent-session".to_string()),
        origin: Some("launchplane".to_string()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_launched_cli_session_records_provenance_without_changing_session_source()
-> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;

    let mut builder = test_codex();
    let test = Box::pin(builder.build(&server)).await?;
    let provenance = agent_session_provenance();
    let started = test
        .thread_manager
        .start_thread(StartThreadOptions {
            session_source: Some(SessionSource::Cli),
            session_provenance: Some(provenance.clone()),
            environments: None,
            ..StartThreadOptions::new(test.config.clone())
        })
        .await?
        .thread;

    started.ensure_rollout_materialized().await;
    started.flush_rollout().await?;

    let rollout_path = started.rollout_path().context("rollout path")?;
    let first_line = std::fs::read_to_string(&rollout_path)?
        .lines()
        .next()
        .context("rollout must start with session_meta")?
        .to_string();
    let payload = serde_json::from_str::<serde_json::Value>(&first_line)?
        .get("payload")
        .cloned()
        .context("session_meta line must carry a payload")?;
    let session_meta: SessionMetaLine = serde_json::from_value(payload)?;

    // Provenance travels in its own field: it must never be folded into the
    // `source` discriminant that decides CLI vs sub-agent behavior.
    assert_eq!(session_meta.meta.source, SessionSource::Cli);
    assert_eq!(session_meta.meta.session_provenance, Some(provenance));

    Ok(())
}
