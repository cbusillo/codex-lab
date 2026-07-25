use std::sync::Mutex;

use pretty_assertions::assert_eq;

use super::INSTALL_URL;
use super::InstallerHttp;
use super::InstallerResponse;
use super::fetch_installer_script;
use super::installer_shell_command;
use super::run_installer_script;
use super::update_modes_for_identities;
use crate::RestartMode;
use crate::UpdaterRefreshMode;
use crate::managed_install::executable_identity_from_bytes;

#[test]
fn unchanged_updater_uses_version_based_restart() {
    assert_eq!(
        update_modes_for_identities(
            &executable_identity_from_bytes(b"same"),
            &executable_identity_from_bytes(b"same"),
        ),
        (RestartMode::IfVersionChanged, UpdaterRefreshMode::None)
    );
}

#[test]
fn changed_updater_forces_refresh_even_when_version_may_match() {
    assert_eq!(
        update_modes_for_identities(
            &executable_identity_from_bytes(b"old"),
            &executable_identity_from_bytes(b"new"),
        ),
        (
            RestartMode::Always,
            UpdaterRefreshMode::ReexecIfManagedBinaryChanged,
        )
    );
}

#[tokio::test]
async fn installer_fetch_uses_exact_url_and_preserves_bytes() {
    let script = b"#!/bin/sh\nprintf 'update bytes'\n".to_vec();
    let http = FakeInstallerHttp::new(InstallerResponse::Success(script.clone()));

    assert_eq!(
        fetch_installer_script(&http)
            .await
            .expect("installer fetch should succeed"),
        script
    );
    assert_eq!(http.requested_urls(), vec![INSTALL_URL.to_string()]);
}

#[tokio::test]
async fn installer_fetch_rejects_non_success_status() {
    let http = FakeInstallerHttp::new(InstallerResponse::Unsuccessful { status: 503 });

    let error = fetch_installer_script(&http)
        .await
        .expect_err("non-success response should fail");

    assert!(error.to_string().contains("503"));
    assert_eq!(http.requested_urls(), vec![INSTALL_URL.to_string()]);
}

#[test]
fn installer_shell_command_translates_codex_lab_home_for_child() {
    let codex_lab_home = tempfile::tempdir().expect("temp codex lab home");
    let command = installer_shell_command(codex_lab_home.path());

    let envs = command
        .as_std()
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(envs.len(), 2);
    assert!(envs.contains(&(
        "CODEX_HOME".to_string(),
        Some(codex_lab_home.path().to_string_lossy().into_owned()),
    )));
    assert!(envs.contains(&("CODE_HOME".to_string(), None)));
}

#[tokio::test]
async fn installer_child_scopes_codex_home_and_drops_code_home() {
    let codex_lab_home = tempfile::tempdir().expect("temp codex lab home");

    let script = br#"
set -eu
printf '%s' "${CODEX_HOME-<unset>}" > "$CODEX_HOME/observed-codex-home"
printf '%s' "${CODE_HOME-<unset>}" > "$CODEX_HOME/observed-code-home"
"#;

    run_installer_script(installer_shell_command(codex_lab_home.path()), script)
        .await
        .expect("installer script should succeed");

    let observed = |name: &str| {
        std::fs::read_to_string(codex_lab_home.path().join(name)).expect("observed env file")
    };
    let expected_home = codex_lab_home.path().to_string_lossy().into_owned();
    assert_eq!(observed("observed-codex-home"), expected_home);
    assert_eq!(observed("observed-code-home"), "<unset>");
}

#[tokio::test]
async fn installer_script_failure_is_reported() {
    let codex_lab_home = tempfile::tempdir().expect("temp codex lab home");

    let error = run_installer_script(installer_shell_command(codex_lab_home.path()), b"exit 3\n")
        .await
        .expect_err("failing installer script should error");

    assert!(
        error
            .to_string()
            .contains("standalone Codex updater exited")
    );
}

struct FakeInstallerHttp {
    response: InstallerResponse,
    requested_urls: Mutex<Vec<String>>,
}

impl FakeInstallerHttp {
    fn new(response: InstallerResponse) -> Self {
        Self {
            response,
            requested_urls: Mutex::new(Vec::new()),
        }
    }

    fn requested_urls(&self) -> Vec<String> {
        self.requested_urls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl InstallerHttp for FakeInstallerHttp {
    async fn get(&self, url: &str) -> anyhow::Result<InstallerResponse> {
        self.requested_urls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(url.to_string());
        Ok(self.response.clone())
    }
}
