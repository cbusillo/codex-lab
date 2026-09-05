//! Production-path proof for the durable environment baseline.
//!
//! `TurnContext::to_turn_context_item` persists the resolved environment
//! selections on every turn, and `rollout_reconstruction` seeds the world-state
//! environments baseline from them when a rollout predates world-state items.
//! Unit coverage for the reader hand-builds a `TurnContextItem`, which cannot
//! catch a writer that stops populating `environments` at all: the reader tests
//! would keep passing against a field nothing writes.
//!
//! This suite therefore drives a real turn, reads the field back out of the
//! rollout the session wrote, cross-checks it against the independently written
//! world-state snapshot, and then resumes the thread to prove the reconstructed
//! baseline reports the same environments.

use anyhow::Context;
use anyhow::Result;
use codex_history::RolloutItem;
use codex_history::RolloutLine;
use codex_protocol::protocol::TurnContextEnvironmentItem;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::sync::Arc;

/// Environment identity as the two independent writers record it: the turn
/// context item and the world-state environments section.
type EnvironmentBaseline = Vec<(String, String, Option<String>)>;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_turn_records_live_environments_and_resume_restores_the_same_baseline() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let turn = || {
        sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-1"),
        ])
    };
    mount_sse_sequence(&server, vec![turn(), turn()]).await;

    let mut builder = test_codex();
    let test = Box::pin(builder.build(&server)).await?;
    // `submit_turn` returns once the turn completes, so the rollout is written.
    Box::pin(test.submit_turn("record the live environments")).await?;

    let rollout_path = test
        .session_configured
        .rollout_path
        .clone()
        .context("initial thread should report a rollout path")?;
    let recorded = turn_context_environments(&rollout_path)?;
    assert!(
        !recorded.is_empty(),
        "a real turn must persist its resolved environments on the turn context item"
    );

    // The world-state section is written by a different code path from the same
    // live selections, so agreement between the two pins the turn context item to
    // the session's actual environments rather than to a restated literal.
    assert_eq!(recorded, world_state_environments(&rollout_path)?);

    // Resume on the same cwd so the reconstructed baseline is directly comparable:
    // a lost baseline shows up as a missing or divergent environments record.
    let resumed_cwd = test.config.cwd.clone();
    test.codex.shutdown_and_wait().await?;
    let mut resume_builder = test_codex()
        .with_home(Arc::clone(&test.home))
        .with_config(move |config| config.cwd = resumed_cwd);
    let resumed =
        Box::pin(resume_builder.resume(&server, Arc::clone(&test.home), rollout_path.clone()))
            .await?;
    Box::pin(resumed.submit_turn("continue the resumed thread")).await?;

    let resumed_rollout_path = resumed
        .session_configured
        .rollout_path
        .clone()
        .context("resumed thread should report a rollout path")?;
    assert_eq!(turn_context_environments(&resumed_rollout_path)?, recorded);
    assert_eq!(world_state_environments(&resumed_rollout_path)?, recorded);

    Ok(())
}

fn rollout_items(rollout_path: &Path) -> Result<Vec<RolloutItem>> {
    std::fs::read_to_string(rollout_path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            Ok(serde_json::from_str::<RolloutLine>(line)
                .with_context(|| format!("rollout line should deserialize: {line}"))?
                .item)
        })
        .collect()
}

/// Environments as the turn context writer recorded them, newest turn last.
fn turn_context_environments(rollout_path: &Path) -> Result<EnvironmentBaseline> {
    let environments = rollout_items(rollout_path)?
        .into_iter()
        .filter_map(|item| match item {
            RolloutItem::TurnContext(turn_context) => turn_context.environments,
            _ => None,
        })
        .next_back()
        .context("rollout should carry a turn context item with recorded environments")?;
    Ok(environments.iter().map(environment_identity).collect())
}

fn environment_identity(
    environment: &TurnContextEnvironmentItem,
) -> (String, String, Option<String>) {
    (
        environment.environment_id.clone(),
        environment.cwd.as_path().display().to_string(),
        environment.shell.clone(),
    )
}

/// Environments as the world-state writer recorded them, from the newest
/// snapshot that carries the section.
fn world_state_environments(rollout_path: &Path) -> Result<EnvironmentBaseline> {
    let section = rollout_items(rollout_path)?
        .into_iter()
        .filter_map(|item| match item {
            RolloutItem::WorldState(world_state) => world_state
                .state
                .get("environments")
                .and_then(|section| section.get("environments"))
                .and_then(serde_json::Value::as_object)
                .cloned(),
            _ => None,
        })
        .next_back()
        .context("rollout should carry a world-state environments section")?;
    Ok(section
        .into_iter()
        .map(|(environment_id, snapshot)| {
            (
                environment_id,
                snapshot
                    .get("cwd")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                snapshot
                    .get("shell")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            )
        })
        .collect())
}
