use super::ActiveRequestKey;
use super::cancel_active_request;
use super::register_active_request;
use crate::outgoing_message::ConnectionId;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[test]
fn duplicate_registration_preserves_the_original_request() {
    let active_requests = Arc::new(Mutex::new(HashMap::new()));
    let first = CancellationToken::new();
    let second = CancellationToken::new();
    let request_key = ActiveRequestKey {
        connection_id: ConnectionId(1),
        request_id: "request-1".to_string(),
    };
    let registration =
        register_active_request(&active_requests, request_key.clone(), first.clone())
            .expect("first registration");

    assert!(
        register_active_request(&active_requests, request_key.clone(), second.clone()).is_err()
    );
    assert!(cancel_active_request(&active_requests, &request_key));
    assert!(first.is_cancelled());
    assert!(!second.is_cancelled());
    drop(registration);
}

#[test]
fn registration_cleanup_makes_late_cancellation_a_noop() {
    let active_requests = Arc::new(Mutex::new(HashMap::new()));
    let request_key = ActiveRequestKey {
        connection_id: ConnectionId(1),
        request_id: "request-1".to_string(),
    };
    let registration = register_active_request(
        &active_requests,
        request_key.clone(),
        CancellationToken::new(),
    )
    .expect("registration");

    drop(registration);

    assert!(!cancel_active_request(&active_requests, &request_key));
    assert!(!cancel_active_request(
        &active_requests,
        &ActiveRequestKey {
            connection_id: ConnectionId(1),
            request_id: "unknown".to_string(),
        }
    ));
}

#[test]
fn identical_request_ids_are_isolated_by_connection() {
    let active_requests = Arc::new(Mutex::new(HashMap::new()));
    let first_key = ActiveRequestKey {
        connection_id: ConnectionId(1),
        request_id: "request-1".to_string(),
    };
    let second_key = ActiveRequestKey {
        connection_id: ConnectionId(2),
        request_id: "request-1".to_string(),
    };
    let first = CancellationToken::new();
    let second = CancellationToken::new();
    let first_registration =
        register_active_request(&active_requests, first_key.clone(), first.clone())
            .expect("first registration");
    let second_registration = register_active_request(&active_requests, second_key, second.clone())
        .expect("second registration");

    assert!(cancel_active_request(&active_requests, &first_key));
    assert!(first.is_cancelled());
    assert!(!second.is_cancelled());
    drop(first_registration);
    drop(second_registration);
}

#[test]
fn request_ids_preserve_caller_whitespace() {
    let active_requests = Arc::new(Mutex::new(HashMap::new()));
    let request_key = ActiveRequestKey {
        connection_id: ConnectionId(1),
        request_id: " request-1 ".to_string(),
    };
    let cancellation = CancellationToken::new();
    let registration =
        register_active_request(&active_requests, request_key.clone(), cancellation.clone())
            .expect("registration");

    assert!(cancel_active_request(&active_requests, &request_key));
    assert!(cancellation.is_cancelled());
    drop(registration);
}
