use super::*;
use crate::agent::bounded_worker::BoundedWorkerRequest;
use crate::agent::bounded_worker::WorkerContext;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn cancellation_during_bounded_preflight_stops_the_probe_group() {
    let dir = TempDir::new().expect("tempdir");
    let script = dir.path().join("probe.sh");
    let pid_file = dir.path().join("probe-child");
    let task_file = dir.path().join("task");
    std::fs::write(&script, format!(
        "if [ \"$1\" = --version ]; then sleep 30 & printf '%s %s\\n' \"$$\" \"$!\" > '{}'; wait; exit; fi\ntouch '{}'\n",
        pid_file.display(), task_file.display(),
    )).expect("script");
    let mut launch = test_launch(
        &dir,
        ExternalCommandAgentBackendConfig {
            command: format!("/bin/sh {}", script.display()),
            protocol: ExternalCommandProtocol::RawCli,
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
    let probe_pids = tokio::time::timeout(Duration::from_secs(/*secs*/ 3), async {
        loop {
            if let Ok(contents) = tokio::fs::read_to_string(&pid_file).await
                && let Ok(pids) = contents
                    .split_whitespace()
                    .map(str::parse)
                    .collect::<Result<Vec<i32>, _>>()
                && let Ok(pids) = <[i32; 2]>::try_from(pids)
            {
                break pids;
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 10)).await;
        }
    })
    .await
    .expect("preflight started");
    assert!(probe_pids.iter().all(|pid| *pid > 0));
    assert_eq!(unsafe { libc::getpgid(probe_pids[1]) }, probe_pids[0]);
    cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(/*secs*/ 2), runner)
        .await
        .expect("cancelled promptly")
        .expect("runner");
    for pid in probe_pids {
        let observation = tokio::time::timeout(Duration::from_secs(/*secs*/ 2), async {
            // Observe both fixture members directly; a group probe can be denied on macOS.
            while unsafe {
                libc::kill(pid, /*sig*/ 0)
            } == 0
            {
                tokio::time::sleep(Duration::from_millis(/*millis*/ 10)).await;
            }
            std::io::Error::last_os_error().raw_os_error()
        })
        .await
        .expect("preflight member stopped");
        assert_eq!(observation, Some(libc::ESRCH));
    }
    assert!(
        !task_file.exists(),
        "task must never launch after cancellation"
    );
}
