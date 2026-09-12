use super::*;
use crate::agent::bounded_worker::BoundedWorkerRequest;
use crate::agent::bounded_worker::WorkerContext;

#[tokio::test]
async fn cancellation_during_bounded_preflight_stops_the_probe_group() {
    let dir = TempDir::new().expect("tempdir");
    let script = dir.path().join("probe.sh");
    let pid_file = dir.path().join("probe-child");
    let task_file = dir.path().join("task");
    std::fs::write(&script, format!(
        "if [ \"$1\" = --version ]; then sleep 30 & echo $! > '{}'; wait; exit; fi\ntouch '{}'\n",
        pid_file.display(), task_file.display(),
    )).expect("script");
    let mut launch = test_launch(
        &dir,
        ExternalCommandAgentBackendConfig {
            command: format!("/bin/sh {}", script.display()),
            launch_family: Some("claude".to_string()),
            ..Default::default()
        },
        /*is_read_only*/ true,
    );
    launch.bounded_worker = Some(
        BoundedWorkerRequest {
            timeout_ms: 5000,
            max_input_bytes: 8192,
            max_result_bytes: 256,
            context: WorkerContext::Text,
        }
        .start(tokio::time::Instant::now())
        .expect("limits"),
    );
    let cancellation = launch.cancellation_token.clone();
    let runner = tokio::spawn(run_external_agent(launch, AgentControl::default()));
    let child_pid = tokio::time::timeout(Duration::from_secs(/*secs*/ 3), async {
        loop {
            if let Ok(pid) = tokio::fs::read_to_string(&pid_file).await
                && let Ok(pid) = pid.trim().parse::<i32>()
            {
                break nix::unistd::Pid::from_raw(pid);
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 10)).await;
        }
    })
    .await
    .expect("preflight started");
    cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(/*secs*/ 2), runner)
        .await
        .expect("cancelled promptly")
        .expect("runner");
    tokio::time::timeout(Duration::from_secs(/*secs*/ 2), async {
        while nix::sys::signal::kill(child_pid, /*signal*/ None).is_ok() {
            tokio::time::sleep(Duration::from_millis(/*millis*/ 10)).await;
        }
    })
    .await
    .expect("preflight descendant stopped");
    assert!(
        !task_file.exists(),
        "task must never launch after cancellation"
    );
}
