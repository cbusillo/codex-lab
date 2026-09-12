use super::*;
use pretty_assertions::assert_eq;

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
