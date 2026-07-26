//! End-to-end coverage for the shared CLI options that change how a run is
//! configured before any model traffic happens: `--workspace-root`, which
//! rewrites the sandbox profile, and `--auth-profile`, which redirects the
//! credential home. Both only matter as observed by a real process, so these
//! drive the `codex-exec` binary rather than the option parser.

use codex_login::CODEX_API_KEY_ENV_VAR;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex_exec::test_codex_exec;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use wiremock::matchers::header;

fn turn_response() -> String {
    sse(vec![
        ev_response_created("resp-1"),
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-1"),
    ])
}

/// `--workspace-root` swaps the run onto the exact workspace permission profile,
/// which is only correct under `workspace-write`; every other sandbox mode must
/// be rejected before the run starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_root_requires_workspace_write_sandbox() -> anyhow::Result<()> {
    let test = test_codex_exec();
    std::fs::create_dir_all(test.cwd_path().join("tenant"))?;

    test.cmd()
        .arg("--skip-git-repo-check")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--workspace-root")
        .arg("tenant")
        .arg("hello")
        .assert()
        .failure()
        .stderr(contains(
            "--workspace-root requires --sandbox workspace-write",
        ));

    Ok(())
}

/// `--add-dir` widens the default profile while `--workspace-root` replaces it,
/// so combining them would silently drop one of the two intents.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_root_conflicts_with_add_dir() -> anyhow::Result<()> {
    let test = test_codex_exec();
    std::fs::create_dir_all(test.cwd_path().join("tenant"))?;
    std::fs::create_dir_all(test.cwd_path().join("extra"))?;

    test.cmd()
        .arg("--skip-git-repo-check")
        .arg("--sandbox")
        .arg("workspace-write")
        .arg("--workspace-root")
        .arg("tenant")
        .arg("--add-dir")
        .arg("extra")
        .arg("hello")
        .assert()
        .failure()
        .stderr(contains(
            "the argument '--workspace-root <DIR>' cannot be used with '--add-dir <DIR>'",
        ));

    Ok(())
}

/// A plain `--sandbox workspace-write` run stays on the legacy sandbox mode, so
/// its summary still enumerates the writable roots.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_write_without_workspace_root_keeps_the_legacy_sandbox_summary()
-> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = start_mock_server().await;
    mount_sse_once_match(
        &server,
        header("Authorization", "Bearer dummy"),
        turn_response(),
    )
    .await;

    test.cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("--sandbox")
        .arg("workspace-write")
        .arg("hello")
        .assert()
        .success()
        .stderr(contains("sandbox: workspace-write [workdir"));

    Ok(())
}

/// `--workspace-root` under `workspace-write` drops the legacy sandbox mode and
/// selects the built-in workspace *permission profile* instead. That profile has
/// no legacy equivalent, so the effective-configuration summary no longer prints
/// a `workspace-write` policy — the differential is what proves the flag took
/// the profile path rather than merely widening the legacy mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_root_switches_the_run_onto_the_exact_workspace_profile() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = start_mock_server().await;
    mount_sse_once_match(
        &server,
        header("Authorization", "Bearer dummy"),
        turn_response(),
    )
    .await;
    std::fs::create_dir_all(test.cwd_path().join("tenant-root"))?;

    test.cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("--sandbox")
        .arg("workspace-write")
        .arg("--workspace-root")
        .arg("tenant-root")
        .arg("hello")
        .assert()
        .success()
        .stderr(contains("sandbox: workspace-write [workdir").not());

    Ok(())
}

/// `--auth-profile` must read credentials from
/// `$CODEX_LAB_HOME/auth-profiles/<name>/auth.json` rather than the default
/// credential home, so the request has to carry the profile's key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_profile_resolves_credentials_from_the_profile_home() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = start_mock_server().await;
    mount_sse_once_match(
        &server,
        header("Authorization", "Bearer profile-api-key"),
        turn_response(),
    )
    .await;

    let profile_home = test.home_path().join("auth-profiles").join("work");
    std::fs::create_dir_all(&profile_home)?;
    std::fs::write(
        profile_home.join("auth.json"),
        serde_json::json!({ "OPENAI_API_KEY": "profile-api-key" }).to_string(),
    )?;
    // The default credential home holds a different key, so a run that ignored
    // `--auth-profile` would not match the mounted mock.
    std::fs::write(
        test.home_path().join("auth.json"),
        serde_json::json!({ "OPENAI_API_KEY": "default-api-key" }).to_string(),
    )?;

    test.cmd_with_server(&server)
        .env_remove(CODEX_API_KEY_ENV_VAR)
        .arg("--skip-git-repo-check")
        .arg("--auth-profile")
        .arg("work")
        .arg("hello")
        .assert()
        .success();

    Ok(())
}

/// A profile name that escapes the profile directory must be rejected instead of
/// resolving to an arbitrary credential home.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_profile_rejects_a_traversing_profile_name() -> anyhow::Result<()> {
    let test = test_codex_exec();

    test.cmd()
        .arg("--skip-git-repo-check")
        .arg("--auth-profile")
        .arg("..")
        .arg("hello")
        .assert()
        .failure()
        .stderr(contains("invalid --auth-profile"));

    Ok(())
}
