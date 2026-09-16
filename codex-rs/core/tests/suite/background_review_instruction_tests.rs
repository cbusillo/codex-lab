use super::*;
use pretty_assertions::assert_eq;

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
async fn oversized_nested_instructions_fail_without_a_review_request() -> Result<()> {
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
    let mock =
        responses::mount_sse_sequence(&server, code_changing_turn_responses("oversized")).await;
    let test = build_codex_in_repo(&server, cwd.clone(), /*budget*/ None).await?;
    submit_turn(&test.codex, &cwd, "add the feature").await?;
    let statuses =
        background_review_statuses_until(&test.codex, BackgroundAutoReviewStatus::Failed).await;
    let run = single_run(&AutoReviewStore::for_scope(
        test.codex_home_path(),
        repo.path(),
    ));
    assert_eq!(run.status, AutoReviewRunStatus::Failed);
    assert_eq!(run.finding_count, 0);
    assert_eq!(statuses.last().unwrap().error_summary, run.error_summary);
    assert_eq!(
        run.error_summary.as_deref(),
        Some(
            "Background Review cannot verify complete AGENTS.md instructions: the complete instruction fragment exceeds its context limit. The review request was not sent."
        )
    );
    assert_eq!(mock.requests().len(), 2);
    shutdown_thread(&test.codex).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_changed_target_instructions_fail_without_a_review_request() -> Result<()> {
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
    let mock = responses::mount_sse_sequence(
        &server,
        code_changing_turn_responses_with_patch("missing", &patch),
    )
    .await;
    let test = build_codex_in_repo(&server, cwd.clone(), /*budget*/ None).await?;
    submit_turn(&test.codex, &cwd, "add the nested feature").await?;
    background_review_statuses_until(&test.codex, BackgroundAutoReviewStatus::Failed).await;
    let run = single_run(&AutoReviewStore::for_scope(
        test.codex_home_path(),
        repo.path(),
    ));
    assert_eq!(run.status, AutoReviewRunStatus::Failed);
    assert_eq!(
        run.error_summary.as_deref(),
        Some(
            "Background Review cannot verify complete AGENTS.md instructions: applicable instruction files are missing from the review snapshot. The review request was not sent."
        )
    );
    assert_eq!(mock.requests().len(), 2);
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
