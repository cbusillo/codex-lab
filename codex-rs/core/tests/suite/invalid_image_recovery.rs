//! Recovery from a Responses API `invalid image` rejection.
//!
//! The rejection is fatal for the whole request, so the offending image has to leave history or
//! every later turn of the thread fails. These tests pin the two properties that make that
//! rewrite safe: the sanitized history is checkpointed without destroying the thread's context
//! window identity or its world-state baseline, and every candidate image is cleared in one pass.

use anyhow::Result;
use codex_history::CompactedItem;
use codex_history::RolloutItem;
use codex_history::RolloutLine;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::TOOLS_OPEN_TAG;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tempfile::TempDir;
use wiremock::ResponseTemplate;

const TINY_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
const INVALID_IMAGE_BODY: &str =
    r#"{"error":{"message":"The image data you provided does not represent a valid image"}}"#;
const INVALID_IMAGE_SANITIZED_MESSAGE: &str = concat!(
    "The model could not read one of the images in this conversation, so it was removed from ",
    "history. Later turns will work; retry with a different image if you still need it."
);

fn user_image_turn(text: &str) -> TurnInputRequest {
    TurnInputRequest::user_input(vec![
        UserInput::Image {
            image_url: TINY_PNG.to_string(),
            detail: None,
        },
        UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        },
    ])
}

fn input_image_count(request: &ResponsesRequest) -> usize {
    request
        .body_json()
        .to_string()
        .matches("input_image")
        .count()
}

/// Counts the model-visible `<tools>` world-state fragments in a request. The tools section is
/// identical across both sessions here, so a second occurrence means the resumed thread lost its
/// world-state baseline and re-rendered the whole section.
fn tools_fragment_count(request: &ResponsesRequest) -> usize {
    request
        .message_input_texts("developer")
        .into_iter()
        .filter(|text| text.starts_with(TOOLS_OPEN_TAG))
        .count()
}

fn rollout_items(rollout_text: &str) -> Vec<RolloutItem> {
    rollout_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<RolloutLine>(line.trim()).ok())
        .map(|line| line.item)
        .collect()
}

/// A user-attached image the API refuses to read is cleared from history, the user is told
/// explicitly, and the resumed thread keeps the same context window and world-state baseline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_user_image_sanitization_survives_resume_coherently() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let mock = responses::mount_response_sequence(
        &server,
        vec![
            ResponseTemplate::new(400).set_body_string(INVALID_IMAGE_BODY),
            responses::sse_response(sse(vec![
                ev_response_created("resp-resumed"),
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-resumed"),
            ])),
        ],
    )
    .await;

    let mut builder = test_codex().with_home(Arc::clone(&home));
    let test = Box::pin(builder.build(&server)).await?;
    test.codex
        .start_or_steer_turn(user_image_turn("look at this"))
        .await?;

    let error = wait_for_event(&test.codex, |event| matches!(event, EventMsg::Error(_))).await;
    let EventMsg::Error(error) = error else {
        unreachable!("waited for an error event")
    };
    assert_eq!(
        error.message, INVALID_IMAGE_SANITIZED_MESSAGE,
        "the user must be told their image was removed rather than silently retried"
    );

    let rollout_path = test.codex.rollout_path().expect("rollout path");
    test.codex.submit(Op::Shutdown).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ShutdownComplete)
    })
    .await;

    // The sanitization checkpoint must carry the *current* window identity. A window-less
    // checkpoint makes resume treat the thread as a legacy compaction and discard its window ids.
    let rollout_text = tokio::fs::read_to_string(&rollout_path).await?;
    let items = rollout_items(&rollout_text);
    let session_window_id = items
        .iter()
        .find_map(|item| match item {
            RolloutItem::SessionMeta(meta) => meta.meta.context_window.as_ref(),
            _ => None,
        })
        .map(|context_window| context_window.window_id.clone());
    let checkpoint = items
        .iter()
        .find_map(|item| match item {
            RolloutItem::Compacted(compacted) => Some(compacted),
            _ => None,
        })
        .cloned()
        .expect("sanitization should persist a history checkpoint");
    let CompactedItem {
        window_number,
        first_window_id,
        previous_window_id,
        window_id,
        ..
    } = &checkpoint;
    assert_eq!(*window_number, Some(0), "sanitization opens no new window");
    assert_eq!(*previous_window_id, None);
    assert_eq!(first_window_id, window_id);
    assert_eq!(window_id, &session_window_id);

    // The baseline is re-established after the checkpoint, since sanitization only rewrote image
    // content and left every world-state section intact.
    let checkpoint_index = items
        .iter()
        .position(|item| matches!(item, RolloutItem::Compacted(_)))
        .expect("checkpoint index");
    assert!(
        items[checkpoint_index + 1..]
            .iter()
            .any(|item| matches!(item, RolloutItem::WorldState(state) if state.full)),
        "a full world-state snapshot must follow the checkpoint"
    );

    let mut resume_builder = test_codex().with_home(Arc::clone(&home));
    let resumed = Box::pin(resume_builder.resume(&server, Arc::clone(&home), rollout_path)).await?;
    resumed.submit_turn("continue after sanitization").await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        input_image_count(&requests[0]),
        1,
        "the rejected request should still carry the user image"
    );
    assert_eq!(
        input_image_count(&requests[1]),
        0,
        "the resumed thread must not replay the poisoned image"
    );
    assert_eq!(
        requests[1].header("x-codex-window-id"),
        requests[0].header("x-codex-window-id"),
        "resume must keep the same context window number"
    );
    assert_eq!(
        tools_fragment_count(&requests[1]),
        tools_fragment_count(&requests[0]),
        "the world-state baseline must survive, so world state is not re-rendered on resume"
    );

    Ok(())
}

/// One rejection clears every candidate image, not just the newest image-bearing item. Clearing
/// one item per rejection would burn a failed turn per image and destroy the newest (most likely
/// good) image first, while still ending up removing the same set.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_image_recovery_clears_all_candidate_images_in_one_pass() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = responses::mount_response_sequence(
        &server,
        vec![
            responses::sse_response(sse(vec![
                ev_response_created("resp-1"),
                ev_assistant_message("msg-1", "saw it"),
                ev_completed("resp-1"),
            ])),
            ResponseTemplate::new(400).set_body_string(INVALID_IMAGE_BODY),
            responses::sse_response(sse(vec![
                ev_response_created("resp-3"),
                ev_assistant_message("msg-2", "done"),
                ev_completed("resp-3"),
            ])),
        ],
    )
    .await;

    let mut builder = test_codex();
    let test = Box::pin(builder.build(&server)).await?;

    test.codex
        .start_or_steer_turn(user_image_turn("first image"))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    test.codex
        .start_or_steer_turn(user_image_turn("second image"))
        .await?;
    wait_for_event(&test.codex, |event| matches!(event, EventMsg::Error(_))).await;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    test.submit_turn("continue after sanitization").await?;

    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(input_image_count(&requests[0]), 1);
    assert_eq!(
        input_image_count(&requests[1]),
        2,
        "the rejected request should carry both candidate images"
    );
    assert_eq!(
        input_image_count(&requests[2]),
        0,
        "a single recovery pass must clear every candidate image"
    );

    Ok(())
}
