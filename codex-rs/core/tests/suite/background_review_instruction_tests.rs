use super::*;
use core_test_support::responses::ResponsesRequest;
use pretty_assertions::assert_eq;

fn review_instruction_parts(request: &ResponsesRequest) -> Vec<String> {
    let body = request.body_json();
    body["input"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|item| {
            if item["internal_chat_message_metadata_passthrough"]["content_item_kinds"]
                .as_array()
                .is_some_and(|kinds| {
                    kinds
                        .iter()
                        .any(|kind| kind == "background_review.agents_md_part")
                })
            {
                assert_eq!(item["content"].as_array().map(Vec::len), Some(1));
            }
            let content = item["content"].as_array().into_iter().flatten();
            let kinds = item["internal_chat_message_metadata_passthrough"]["content_item_kinds"]
                .as_array()
                .into_iter()
                .flatten();
            content.zip(kinds).filter_map(|(content, kind)| {
                (kind.as_str() == Some("background_review.agents_md_part"))
                    .then(|| content["text"].as_str().map(str::to_owned))
                    .flatten()
            })
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fitting_nested_instructions_reach_the_review_request() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let repo = create_git_repo()?;
    let nested = repo.path().join("nested");
    std::fs::create_dir(&nested)?;
    std::fs::write(repo.path().join("AGENTS.md"), "Follow the nested rules.\n")?;
    std::fs::write(
        nested.join("AGENTS.md"),
        "Reject unsafe arithmetic: NESTED_REVIEW_RULE.",
    )?;
    let cwd = AbsolutePathBuf::try_from(nested)?;
    let server = responses::start_mock_server().await;
    let mut bodies = code_changing_turn_responses("fitting");
    bodies.push(responses::sse(vec![
        responses::ev_response_created("review"),
        responses::ev_assistant_message("review-message", &review_output_json(/*findings*/ 0)),
        responses::ev_completed("review"),
    ]));
    let mock = responses::mount_sse_sequence(&server, bodies).await;
    let test = build_codex_in_repo(&server, cwd.clone(), /*budget*/ None).await?;
    submit_turn(&test.codex, &cwd, "add the feature").await?;
    background_review_statuses_until(&test.codex, BackgroundAutoReviewStatus::Completed).await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]
            .body_json()
            .to_string()
            .contains("NESTED_REVIEW_RULE")
    );
    shutdown_thread(&test.codex).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleted_instruction_file_does_not_disappear_from_review_coverage() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let repo = create_git_repo()?;
    std::fs::create_dir(repo.path().join("nested"))?;
    std::fs::write(
        repo.path().join("nested/AGENTS.md"),
        "Reject unsafe arithmetic.\n",
    )?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let patch = ADD_FEATURE_PATCH.replace(
        "*** End Patch",
        "*** Delete File: nested/AGENTS.md\n*** End Patch",
    );
    let mock = responses::mount_sse_sequence(
        &server,
        code_changing_turn_responses_with_patch("deleted", &patch),
    )
    .await;
    let test = build_codex_in_repo(&server, cwd.clone(), /*budget*/ None).await?;
    submit_turn(&test.codex, &cwd, "add the feature and remove its rules").await?;
    background_review_statuses_until(&test.codex, BackgroundAutoReviewStatus::Failed).await;
    let run = single_run(&AutoReviewStore::for_scope(
        test.codex_home_path(),
        repo.path(),
    ));
    assert_eq!(run.status, AutoReviewRunStatus::Failed);
    assert_eq!(
        run.error_summary.as_deref(),
        Some(
            "Background Review cannot verify complete AGENTS.md instructions: the turn changes instruction files whose baseline scope is not available. The review request was not sent."
        )
    );
    assert_eq!(mock.requests().len(), 2);
    shutdown_thread(&test.codex).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_nested_instructions_are_split_without_losing_utf8_rules() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let repo = create_git_repo()?;
    let nested = repo.path().join("nested");
    std::fs::create_dir(&nested)?;
    std::fs::write(repo.path().join("AGENTS.md"), "Follow the nested rules.\n")?;
    std::fs::write(
        nested.join("AGENTS.md"),
        format!("{}\nReject unsafe arithmetic.\n", "é".repeat(5_000)),
    )?;
    let cwd = AbsolutePathBuf::try_from(nested)?;
    let server = responses::start_mock_server().await;
    let mut bodies = code_changing_turn_responses("oversized");
    bodies.push(responses::sse(vec![
        responses::ev_response_created("review"),
        responses::ev_assistant_message("review-message", &review_output_json(/*findings*/ 0)),
        responses::ev_completed("review"),
    ]));
    let mock = responses::mount_sse_sequence(&server, bodies).await;
    let test = build_codex_in_repo(&server, cwd.clone(), /*budget*/ None).await?;
    submit_turn(&test.codex, &cwd, "add the feature").await?;
    background_review_statuses_until(&test.codex, BackgroundAutoReviewStatus::Completed).await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    let parts = review_instruction_parts(&requests[2]);
    assert!(parts.len() >= 2);
    assert!(parts.iter().all(|part| part.len() <= 9 * 1024));
    assert!(parts.iter().all(|part| part.contains("digest=")));
    assert!(
        parts
            .iter()
            .any(|part| part.contains("Reject unsafe arithmetic."))
    );
    assert_eq!(
        parts
            .iter()
            .flat_map(|part| part.chars())
            .filter(|character| *character == 'é')
            .count(),
        5_000
    );
    shutdown_thread(&test.codex).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_target_instructions_outside_cwd_reach_the_review_request() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let repo = create_git_repo()?;
    std::fs::create_dir(repo.path().join("nested"))?;
    std::fs::write(
        repo.path().join("nested/AGENTS.md"),
        "Reject unsafe arithmetic.\n",
    )?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let patch = ADD_FEATURE_PATCH.replace("feature.rs", "nested/feature.rs");
    let mut bodies = code_changing_turn_responses_with_patch("missing", &patch);
    bodies.push(responses::sse(vec![
        responses::ev_response_created("review"),
        responses::ev_assistant_message("review-message", &review_output_json(/*findings*/ 0)),
        responses::ev_completed("review"),
    ]));
    let mock = responses::mount_sse_sequence(&server, bodies).await;
    let test = build_codex_in_repo(&server, cwd.clone(), /*budget*/ None).await?;
    submit_turn(&test.codex, &cwd, "add the nested feature").await?;
    background_review_statuses_until(&test.codex, BackgroundAutoReviewStatus::Completed).await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]
            .body_json()
            .to_string()
            .contains("Reject unsafe arithmetic.")
    );
    shutdown_thread(&test.codex).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sibling_changed_targets_receive_only_their_scoped_rules_in_root_first_order() -> Result<()>
{
    skip_if_no_network!(Ok(()));
    let repo = create_git_repo()?;
    for directory in ["a", "b", "untouched"] {
        std::fs::create_dir(repo.path().join(directory))?;
    }
    std::fs::write(repo.path().join("AGENTS.md"), "ROOT_REVIEW_RULE")?;
    std::fs::write(repo.path().join("a/AGENTS.md"), "A_REVIEW_RULE")?;
    std::fs::write(repo.path().join("b/AGENTS.md"), "B_REVIEW_RULE")?;
    std::fs::write(
        repo.path().join("untouched/AGENTS.md"),
        "UNTOUCHED_REVIEW_RULE",
    )?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let patch = ADD_FEATURE_PATCH
        .replace("feature.rs", "a/feature.rs")
        .replace(
            "*** End Patch",
            "*** Add File: b/feature.rs\n+pub fn b() {}\n*** End Patch",
        );
    let mut bodies = code_changing_turn_responses_with_patch("siblings", &patch);
    bodies.push(responses::sse(vec![
        responses::ev_response_created("review"),
        responses::ev_assistant_message("review-message", &review_output_json(/*findings*/ 0)),
        responses::ev_completed("review"),
    ]));
    let mock = responses::mount_sse_sequence(&server, bodies).await;
    let test = build_codex_in_repo(&server, cwd.clone(), /*budget*/ None).await?;
    submit_turn(&test.codex, &cwd, "add both scoped features").await?;
    background_review_statuses_until(&test.codex, BackgroundAutoReviewStatus::Completed).await;
    let parts = review_instruction_parts(&mock.requests()[2]).join("\n");
    let root = parts.find("ROOT_REVIEW_RULE").unwrap();
    let a = parts.find("A_REVIEW_RULE").unwrap();
    let b = parts.find("B_REVIEW_RULE").unwrap();
    assert!(root < a && root < b);
    assert!(!parts.contains("UNTOUCHED_REVIEW_RULE"));
    shutdown_thread(&test.codex).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_review_completes_when_no_agents_files_apply() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let repo = create_git_repo()?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let mut bodies = code_changing_turn_responses("empty-instructions");
    bodies.push(responses::sse(vec![
        responses::ev_response_created("review"),
        responses::ev_assistant_message("review-message", &review_output_json(/*findings*/ 0)),
        responses::ev_completed("review"),
    ]));
    let mock = responses::mount_sse_sequence(&server, bodies).await;
    let test = build_codex_in_repo(&server, cwd.clone(), /*budget*/ None).await?;
    submit_turn(&test.codex, &cwd, "add the feature").await?;
    background_review_statuses_until(&test.codex, BackgroundAutoReviewStatus::Completed).await;
    assert!(review_instruction_parts(&mock.requests()[2]).is_empty());
    shutdown_thread(&test.codex).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loader_truncation_cannot_be_mistaken_for_a_complete_small_fragment() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let repo = create_git_repo()?;
    std::fs::write(
        repo.path().join("AGENTS.md"),
        "Retain all instructions, including this last rule.",
    )?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(&server, code_changing_turn_responses("loader")).await;
    let test_cwd = cwd.clone();
    let mut builder = test_codex().with_config(move |config| {
        config.cwd = test_cwd;
        config.project_doc_max_bytes = 8;
    });
    let test = builder.build(&server).await?;
    submit_turn(&test.codex, &cwd, "add the feature").await?;
    background_review_statuses_until(&test.codex, BackgroundAutoReviewStatus::Failed).await;
    let run = single_run(&AutoReviewStore::for_scope(
        test.codex_home_path(),
        repo.path(),
    ));
    assert_eq!(run.status, AutoReviewRunStatus::Failed);
    assert_eq!(
        run.error_summary.as_deref(),
        Some(
            "Background Review cannot verify complete AGENTS.md instructions: instruction loading was truncated or failed. The review request was not sent."
        )
    );
    assert_eq!(mock.requests().len(), 2);
    shutdown_thread(&test.codex).await
}
