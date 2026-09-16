use super::*;
use pretty_assertions::assert_eq;

#[tokio::test(start_paused = true)]
async fn preparation_cannot_outlive_or_restart_an_expired_deadline() {
    let started = Instant::now();
    let worker = BoundedWorkerRequest {
        timeout_ms: 100,
        max_input_bytes: 8192,
        max_result_bytes: 256,
        context: WorkerContext::Text,
    }
    .start(started)
    .expect("limits");
    assert!(
        prepare(Some(&worker), std::future::pending::<()>())
            .await
            .is_err()
    );
    assert_eq!(Instant::now(), worker.deadline);
    let mut polled = false;
    assert!(
        prepare(Some(&worker), async {
            polled = true;
        })
        .await
        .is_err()
    );
    assert!(!polled, "expired preparation must not be polled");
}

#[test]
fn bounded_result_keeps_utf8_tail_and_honors_backend_deadline() {
    let start = Instant::now();
    let request = BoundedWorkerRequest {
        timeout_ms: 5000,
        max_input_bytes: 8192,
        max_result_bytes: 256,
        context: WorkerContext::Text,
    };
    let mut worker = request.start(start).expect("valid limits");
    worker.restrict_timeout(start, /*configured_ms*/ 1000);
    worker.restrict_timeout(start, u64::MAX);
    assert_eq!(worker.deadline, start + Duration::from_secs(/*secs*/ 1));
    assert_eq!(worker.limits.timeout_ms, 1000);
    let message = "🙂".repeat(/*n*/ 100);
    let bounded = worker.bound_result(&message);
    assert!(bounded.len() <= 256);
    assert!(bounded.ends_with("🙂🙂"));
    assert_eq!(worker.bound_result("short result"), "short result");
}

#[test]
fn worker_rejects_unbounded_limits_and_ambiguous_paths() {
    let mut request = BoundedWorkerRequest {
        timeout_ms: 0,
        max_input_bytes: 8192,
        max_result_bytes: 256,
        context: WorkerContext::Text,
    };
    assert!(request.start(Instant::now()).is_err());
    request.timeout_ms = 1000;
    request.context = WorkerContext::Code {
        paths: vec!["../outside.rs".to_string()],
    };
    assert!(request.start(Instant::now()).is_err());
    request.context = WorkerContext::Text;
    request.max_input_bytes = usize::MAX;
    assert!(request.start(Instant::now()).is_err());
}

#[test]
fn code_input_budget_is_explicit_and_does_not_raise_other_limits() {
    let mut request = BoundedWorkerRequest {
        timeout_ms: 5000,
        max_input_bytes: 8192,
        max_result_bytes: 256,
        context: WorkerContext::Code {
            paths: vec!["widget.rs".to_string()],
        },
    };
    for max_input_bytes in [1, 8192, 20_000, 32_768] {
        request.max_input_bytes = max_input_bytes;
        assert_eq!(
            request
                .start(Instant::now())
                .expect("explicit budget")
                .limits,
            BoundedWorkerLimits {
                timeout_ms: 5000,
                max_input_bytes,
                max_result_bytes: 256,
            }
        );
    }
    request.max_input_bytes = 32_769;
    assert!(request.start(Instant::now()).is_err());
    request.max_input_bytes = 8192;
    request.max_result_bytes = 8193;
    assert!(request.start(Instant::now()).is_err());
    request.max_result_bytes = 8192;
    request.context = WorkerContext::Text;
    assert!(request.start(Instant::now()).is_ok());
    request.max_input_bytes = 8193;
    assert!(request.start(Instant::now()).is_err());
}

#[tokio::test]
async fn context_token_limit_is_independent_of_bytes_and_inclusive() {
    let text = " a".repeat(/*n*/ 10_000);
    assert_eq!(validate_context_tokens(&text).await, Ok(()));
    assert_eq!(
        validate_context_tokens(&format!("{text} a")).await,
        Err(
            "complete bounded worker context exceeds 10000 o200k_base tokens; nothing was launched"
        )
    );
    assert!(
        validate_context_tokens(&"🙂".repeat(/*n*/ 2500))
            .await
            .is_ok()
    );
}
