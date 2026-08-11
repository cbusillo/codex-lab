use std::collections::HashMap;
use std::io;
use std::time::Duration;

use codex_protocol::models::SandboxPermissions;
use codex_utils_absolute_path::AbsolutePathBuf;
use tokio_util::sync::CancellationToken;

use super::cargo_validation_cache_key::cargo_toolchain_identity;
use super::turn_context::TurnContext;
use crate::exec::ExecCapturePolicy;
use crate::exec::ExecExpiration;
use crate::exec::ExecParams;
use crate::exec::process_exec_tool_call;

const CARGO_TOOLCHAIN_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

pub(crate) async fn resolve_cargo_toolchain_identity(
    turn_context: &TurnContext,
    toolchain: Option<&str>,
    command: &[String],
    execution_cwd: &AbsolutePathBuf,
    sandbox_cwd: &AbsolutePathBuf,
    environment: &HashMap<String, String>,
    cancellation: &CancellationToken,
) -> io::Result<Option<String>> {
    let program = command
        .first()
        .ok_or_else(|| io::Error::other("cargo validation command has no executable"))?;
    let params = ExecParams {
        command: vec![program.clone(), "-Vv".to_string()],
        cwd: execution_cwd.clone(),
        expiration: ExecExpiration::TimeoutOrCancellation {
            timeout: CARGO_TOOLCHAIN_PROBE_TIMEOUT,
            cancellation: cancellation.clone(),
        },
        capture_policy: ExecCapturePolicy::ShellTool,
        env: environment.clone(),
        network: turn_context.network.clone(),
        network_environment_id: turn_context
            .environments
            .single_local_environment()
            .map(|environment| environment.environment_id.clone()),
        sandbox_permissions: SandboxPermissions::UseDefault,
        windows_sandbox_level: turn_context.windows_sandbox_level,
        windows_sandbox_private_desktop: turn_context
            .config
            .permissions
            .windows_sandbox_private_desktop,
        justification: None,
        arg0: None,
    };
    let output = process_exec_tool_call(
        params,
        &turn_context.permission_profile,
        sandbox_cwd,
        &turn_context.config.effective_workspace_roots(),
        &turn_context.config.codex_linux_sandbox_exe,
        turn_context.config.features.use_legacy_landlock(),
        /*stdout_stream*/ None,
    )
    .await
    .map_err(io::Error::other)?;
    if output.exit_code != 0 {
        return Err(io::Error::other(format!(
            "cargo toolchain probe exited with status {}",
            output.exit_code
        )));
    }
    Ok(cargo_toolchain_identity(toolchain, &output.stdout.text))
}
