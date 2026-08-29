use std::io::Cursor;
use std::io::Read;
use std::io::Write;

use codex_owner_control_contract::ContractArtifact;
use codex_owner_control_contract::approval_request_digest;
use codex_owner_control_contract::channel_binding_sha256;
use codex_owner_control_contract::load_embedded_artifact;
use pretty_assertions::assert_eq;

use super::*;
use crate::framing::read_frame;
use crate::framing::write_frame;

#[cfg(unix)]
use crate::endpoint::UnixDeadlineStream;
#[cfg(unix)]
use crate::session::serve_stream;

#[test]
fn inspect_returns_only_validated_server_review_and_digests() {
    let (request, artifact) = inspect_request();
    let response = handle_request(request, &DenyAllGestureSource);
    let vector = first_vector(&artifact);
    assert_eq!(
        response.protocol_version,
        OWNER_CONTROL_IPC_PROTOCOL_VERSION
    );
    assert_eq!(
        response.outcome,
        OwnerControlIpcOutcome::ReviewAvailable {
            review: vector
                .challenge_response
                .payload
                .approval_request
                .server_review
                .clone(),
            approval_request_digest: approval_request_digest(
                &vector.challenge_response.payload.approval_request
            )
            .unwrap(),
            channel_binding_digest: channel_binding_sha256(&vector.channel_binding.payload)
                .unwrap(),
        }
    );
}

#[test]
fn confirmation_is_always_gesture_unavailable() {
    let (mut request, _) = inspect_request();
    let OwnerControlIpcOperation::InspectChallenge(material) = request.operation else {
        unreachable!();
    };
    request.operation = OwnerControlIpcOperation::ConfirmChallenge(material);
    assert_eq!(
        handle_request(request, &DenyAllGestureSource),
        OwnerControlIpcResponse::rejected(IpcFailureCode::GestureUnavailable)
    );
}

#[test]
fn invalid_contract_input_is_rejected_before_gesture() {
    let (mut request, _) = inspect_request();
    let OwnerControlIpcOperation::InspectChallenge(ref mut material) = request.operation else {
        unreachable!();
    };
    material.approval_request.owner_github_id = 0;
    request.operation = OwnerControlIpcOperation::ConfirmChallenge(material.clone());
    assert_eq!(
        handle_request(request, &DenyAllGestureSource),
        OwnerControlIpcResponse::rejected(IpcFailureCode::InvalidContractInput)
    );
}

#[test]
fn unsupported_protocol_version_is_rejected() {
    let (mut request, _) = inspect_request();
    request.protocol_version += 1;
    assert_eq!(
        handle_request(request, &DenyAllGestureSource),
        OwnerControlIpcResponse::rejected(IpcFailureCode::UnsupportedVersion)
    );
}

#[test]
fn protocol_rejects_unknown_fields() {
    let value = serde_json::json!({
        "protocol_version": OWNER_CONTROL_IPC_PROTOCOL_VERSION,
        "operation": {
            "type": "inspect_challenge",
            "payload": {
                "approval_request": {},
                "channel_binding": {},
                "gesture": "caller-controlled",
            },
        },
    });
    assert!(serde_json::from_value::<OwnerControlIpcRequest>(value).is_err());
}

#[test]
fn protocol_rejects_duplicate_nested_contract_fields() {
    let (request, _) = inspect_request();
    let mut encoded = serde_json::to_string(&request).unwrap();
    encoded = encoded.replacen(
        "\"owner_github_id\":",
        "\"owner_github_id\":1,\"owner_github_id\":",
        1,
    );
    assert!(serde_json::from_str::<OwnerControlIpcRequest>(&encoded).is_err());
}

#[test]
fn frame_round_trip_is_length_prefixed_and_bounded() {
    let (request, _) = inspect_request();
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &request).unwrap();
    assert_eq!(
        u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize,
        bytes.len() - 4
    );
    assert_eq!(
        read_frame::<OwnerControlIpcRequest>(&mut Cursor::new(bytes)).unwrap(),
        request
    );

    let mut oversized = Vec::new();
    oversized.extend_from_slice(&u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes());
    assert_eq!(
        read_frame::<OwnerControlIpcRequest>(&mut Cursor::new(oversized)),
        Err(FrameError::FrameTooLarge)
    );
}

#[test]
#[cfg(unix)]
fn malformed_stream_gets_bounded_rejection() {
    let payload = b"not-json";
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(payload);
    let mut stream = DuplexCursor::new(bytes);
    serve_stream(&mut stream, &DenyAllGestureSource).unwrap();
    let response = read_frame::<OwnerControlIpcResponse>(&mut Cursor::new(stream.written)).unwrap();
    assert_eq!(
        response,
        OwnerControlIpcResponse::rejected(IpcFailureCode::MalformedRequest)
    );
}

#[cfg(unix)]
#[test]
fn unix_endpoint_requires_private_parent_and_creates_private_socket() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket_path = temp.path().join("owner-control.sock");
    let endpoint = OwnerControlEndpoint::bind(&socket_path).unwrap();
    assert_eq!(
        std::fs::symlink_metadata(&socket_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        OwnerControlEndpoint::bind(&socket_path).err(),
        Some(EndpointError::EndpointAlreadyExists)
    );
    drop(endpoint);
}

#[cfg(unix)]
#[test]
fn unix_endpoint_serves_one_same_uid_peer() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;

    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket_path = temp.path().join("owner-control.sock");
    let endpoint = OwnerControlEndpoint::bind(&socket_path).unwrap();
    let server = std::thread::spawn(move || endpoint.serve_once());
    let (request, _) = inspect_request();
    let mut client = UnixStream::connect(&socket_path).unwrap();
    write_frame(&mut client, &request).unwrap();
    let response = read_frame::<OwnerControlIpcResponse>(&mut client).unwrap();
    assert!(matches!(
        response.outcome,
        OwnerControlIpcOutcome::ReviewAvailable { .. }
    ));
    server.join().unwrap().unwrap();
}

#[cfg(unix)]
#[test]
fn unix_endpoint_rejects_relative_and_insecure_paths() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;

    assert_eq!(
        OwnerControlEndpoint::bind("owner-control.sock").err(),
        Some(EndpointError::InvalidPath)
    );
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        OwnerControlEndpoint::bind(temp.path().join("owner-control.sock")).err(),
        Some(EndpointError::InvalidPermissions)
    );

    let symlink_root = tempfile::tempdir().unwrap();
    let private_parent = symlink_root.path().join("private");
    std::fs::create_dir(&private_parent).unwrap();
    std::fs::set_permissions(&private_parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let linked_parent = symlink_root.path().join("linked");
    symlink(private_parent, &linked_parent).unwrap();
    assert_eq!(
        OwnerControlEndpoint::bind(linked_parent.join("owner-control.sock")).err(),
        Some(EndpointError::InvalidPermissions)
    );
}

#[cfg(unix)]
#[test]
fn unix_endpoint_rejects_parent_permission_changes_before_accepting() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket_path = temp.path().join("owner-control.sock");
    let endpoint = OwnerControlEndpoint::bind(&socket_path).unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        endpoint.serve_once(),
        Err(EndpointServeError::Endpoint(
            EndpointError::EndpointCompromised
        ))
    );
}

#[cfg(unix)]
#[test]
fn unix_endpoint_rejects_removed_socket_before_accepting() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket_path = temp.path().join("owner-control.sock");
    let endpoint = OwnerControlEndpoint::bind(&socket_path).unwrap();
    std::fs::remove_file(socket_path).unwrap();
    assert_eq!(
        endpoint.serve_once(),
        Err(EndpointServeError::Endpoint(
            EndpointError::EndpointCompromised
        ))
    );
}

#[cfg(unix)]
#[test]
fn absolute_deadline_bounds_slow_trickle_reads() {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    use std::time::Instant;

    let (reader, mut writer) = UnixStream::pair().unwrap();
    let mut reader = UnixDeadlineStream::new(reader, Duration::from_millis(40)).unwrap();
    let writer_thread = std::thread::spawn(move || {
        writer.write_all(&2_u32.to_be_bytes()).unwrap();
        writer.write_all(b"{").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let _ = writer.write_all(b"}");
    });
    let started = Instant::now();
    assert_eq!(
        read_frame::<serde_json::Value>(&mut reader),
        Err(FrameError::Io)
    );
    assert!(started.elapsed() < Duration::from_millis(180));
    writer_thread.join().unwrap();
}

fn inspect_request() -> (OwnerControlIpcRequest, ContractArtifact) {
    let artifact = load_embedded_artifact().unwrap();
    let vector = first_vector(&artifact);
    let request = OwnerControlIpcRequest {
        protocol_version: OWNER_CONTROL_IPC_PROTOCOL_VERSION,
        operation: OwnerControlIpcOperation::InspectChallenge(ChallengeMaterial {
            approval_request: vector.challenge_response.payload.approval_request.clone(),
            channel_binding: vector.channel_binding.payload.clone(),
        }),
    };
    (request, artifact)
}

fn first_vector(
    artifact: &ContractArtifact,
) -> &codex_owner_control_contract::ConfirmationGoldenVector {
    artifact.confirmation_golden_vectors.first().unwrap()
}

struct DuplexCursor {
    readable: Cursor<Vec<u8>>,
    written: Vec<u8>,
}

impl DuplexCursor {
    fn new(readable: Vec<u8>) -> Self {
        Self {
            readable: Cursor::new(readable),
            written: Vec::new(),
        }
    }
}

impl Read for DuplexCursor {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.readable.read(buffer)
    }
}

impl Write for DuplexCursor {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.written.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
