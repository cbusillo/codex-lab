use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn remote_control_reconnect_response_maps_and_round_trips() {
    let response = RemoteControlReconnectResponse::from(RemoteControlStatusChangedNotification {
        status: RemoteControlConnectionStatus::Connected,
        server_name: "test-server".to_string(),
        installation_id: "installation-id".to_string(),
        environment_id: Some("environment-id".to_string()),
    });
    assert_eq!(
        response,
        RemoteControlReconnectResponse {
            status: RemoteControlConnectionStatus::Connected,
            server_name: "test-server".to_string(),
            installation_id: "installation-id".to_string(),
            environment_id: Some("environment-id".to_string()),
        }
    );

    let value = json!({
        "status": "connected",
        "serverName": "test-server",
        "installationId": "installation-id",
        "environmentId": "environment-id",
    });
    assert_eq!(
        serde_json::to_value(&response).expect("response should serialize"),
        value
    );
    assert_eq!(
        serde_json::from_value::<RemoteControlReconnectResponse>(value)
            .expect("response should deserialize"),
        response
    );
}

#[test]
fn remote_control_clients_list_params_serialize_nullable_optional_fields() {
    assert_eq!(
        serde_json::to_value(RemoteControlClientsListParams {
            environment_id: "env-123".to_string(),
            cursor: None,
            limit: None,
            order: None,
        })
        .expect("params should serialize"),
        json!({
            "environmentId": "env-123",
            "cursor": null,
            "limit": null,
            "order": null,
        })
    );
}

#[test]
fn remote_control_clients_list_params_deserialize_camel_case_fields() {
    assert_eq!(
        serde_json::from_value::<RemoteControlClientsListParams>(json!({
            "environmentId": "env-123",
            "cursor": "cursor-123",
            "limit": 10,
            "order": "asc",
        }))
        .expect("params should deserialize"),
        RemoteControlClientsListParams {
            environment_id: "env-123".to_string(),
            cursor: Some("cursor-123".to_string()),
            limit: Some(10),
            order: Some(RemoteControlClientsListOrder::Asc),
        }
    );
}

#[test]
fn remote_control_clients_revoke_response_serializes_as_empty_object() {
    assert_eq!(
        serde_json::to_value(RemoteControlClientsRevokeResponse {})
            .expect("response should serialize"),
        json!({})
    );
}
