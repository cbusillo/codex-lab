use std::collections::BTreeSet;

use pretty_assertions::assert_eq;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;

use super::*;

#[test]
fn canonicalization_vectors_match_exactly() {
    let artifact = load_embedded_artifact().expect("embedded artifact should load");
    for vector in artifact.canonicalization_vectors {
        let bytes = canonical_json_bytes(&vector.payload).expect("vector should serialize");
        assert_eq!(bytes, vector.canonical_json.as_bytes(), "{}", vector.name);
        assert_eq!(
            canonical_json_sha256(&vector.payload).unwrap(),
            vector.sha256,
            "{}",
            vector.name
        );
    }
}

#[test]
fn golden_vectors_match_exactly() {
    let artifact = load_embedded_artifact().expect("embedded artifact should load");
    for vector in artifact.golden_vectors {
        let request_value = serde_json::to_value(&vector.approval_request.payload).unwrap();
        let response_value = serde_json::to_value(&vector.challenge_response.payload).unwrap();
        assert_eq!(
            canonical_json_bytes(&request_value).unwrap(),
            vector.approval_request.canonical_json.as_bytes()
        );
        assert_eq!(
            canonical_json_sha256(&request_value).unwrap(),
            vector.approval_request.sha256
        );
        assert_eq!(
            canonical_json_bytes(&response_value).unwrap(),
            vector.challenge_response.canonical_json.as_bytes()
        );
        assert_eq!(
            canonical_json_sha256(&response_value).unwrap(),
            vector.challenge_response.sha256
        );
        assert_eq!(
            approval_request_digest(&vector.approval_request.payload).unwrap(),
            vector.approval_request.sha256
        );
        assert_eq!(
            challenge_response_digest(&vector.challenge_response.payload).unwrap(),
            vector.challenge_response.sha256
        );
    }
}

#[test]
fn every_published_negative_vector_is_rejected_at_the_expected_location() {
    let artifact = load_embedded_artifact().expect("embedded artifact should load");
    for vector in &artifact.negative_vectors {
        let error = parse_negative_payload(vector).expect_err(&vector.rule);
        let ArtifactError::Validation(error) = error else {
            panic!("{} returned a non-validation error: {error}", vector.rule);
        };
        assert_eq!(error.location(), vector.error_location, "{}", vector.rule);
    }
}

#[test]
fn unknown_fields_are_rejected_for_each_model() {
    let artifact = load_embedded_artifact().unwrap();
    let vector = &artifact.golden_vectors[0];
    let mut request = serde_json::to_value(&vector.approval_request.payload).unwrap();
    request["unknown"] = json!(true);
    assert_eq!(
        ApprovalRequest::from_value(request).unwrap_err().location(),
        vec![ErrorLocation::Field("unknown".to_owned())]
    );

    let mut response = serde_json::to_value(&vector.challenge_response.payload).unwrap();
    response["unknown"] = json!(true);
    assert_eq!(
        ChallengeResponse::from_value(response)
            .unwrap_err()
            .location(),
        vec![ErrorLocation::Field("unknown".to_owned())]
    );

    let mut review = serde_json::to_value(&vector.approval_request.payload.server_review).unwrap();
    review["unknown"] = json!(true);
    assert_eq!(
        ServerReviewPayload::from_value(review)
            .unwrap_err()
            .location(),
        vec![ErrorLocation::Field("unknown".to_owned())]
    );
}

#[test]
fn typed_validation_rejects_noncanonical_identifiers_and_timestamps() {
    let artifact = load_embedded_artifact().unwrap();
    let vector = &artifact.golden_vectors[0];
    let mut request = serde_json::to_value(&vector.approval_request.payload).unwrap();

    request["operation_id"] = json!("privileged-operation-not-hex");
    assert!(ApprovalRequest::from_value(request.clone()).is_err());
    request["operation_id"] = json!("privileged-operation-1d762a6ac3895594ed85ba7aa9c39330");

    request["policy_record_id"] = json!("Policy Record");
    assert!(ApprovalRequest::from_value(request.clone()).is_err());
    request["policy_record_id"] = json!("owner-policy-1d762a6ac3895594ed85");

    request["issued_at"] = json!("2030-02-30T03:00:05+00:00");
    assert!(ApprovalRequest::from_value(request).is_err());

    let mut review = serde_json::to_value(&vector.approval_request.payload.server_review).unwrap();
    review["review_id"] = json!("Review ID");
    assert!(ServerReviewPayload::from_value(review.clone()).is_err());
    review["review_id"] = json!("review-1d762a6ac3895594ed85ba7a");
    review["items"][0]["key"] = json!("bad-key");
    assert!(ServerReviewPayload::from_value(review).is_err());
}

#[test]
fn canonical_json_rejects_non_integer_and_out_of_range_numbers() {
    let float = json!(1.5);
    assert!(canonical_json_bytes(&float).is_err());
    let too_large = serde_json::Value::Number(serde_json::Number::from(u64::MAX));
    assert!(canonical_json_bytes(&too_large).is_err());
}

#[test]
fn validation_matches_python_control_whitespace_and_nested_locations() {
    let artifact = load_embedded_artifact().unwrap();
    let vector = &artifact.golden_vectors[0];

    for whitespace in ['\u{1c}', '\u{1d}', '\u{1e}', '\u{1f}'] {
        let mut review =
            serde_json::to_value(&vector.approval_request.payload.server_review).unwrap();
        review["title"] = json!(format!("Owner approval required{whitespace}"));
        let error = ServerReviewPayload::from_value(review).unwrap_err();
        assert_eq!(error.location(), Vec::<ErrorLocation>::new());

        let mut review =
            serde_json::to_value(&vector.approval_request.payload.server_review).unwrap();
        review["items"][0]["label"] = json!(format!("Operation class{whitespace}"));
        let error = ServerReviewPayload::from_value(review).unwrap_err();
        assert_eq!(
            error.location(),
            vec![
                ErrorLocation::Field("items".to_owned()),
                ErrorLocation::Index(0),
            ]
        );
    }

    let mut request = serde_json::to_value(&vector.approval_request.payload).unwrap();
    request["server_review"]["review_id"] = json!("Review ID");
    let error = ApprovalRequest::from_value(request).unwrap_err();
    assert_eq!(
        error.location(),
        vec![
            ErrorLocation::Field("server_review".to_owned()),
            ErrorLocation::Field("review_id".to_owned()),
        ]
    );

    let mut request = serde_json::to_value(&vector.approval_request.payload).unwrap();
    request["server_review"]["items"][0]["key"] = json!("bad-key");
    let error = ApprovalRequest::from_value(request).unwrap_err();
    assert_eq!(
        error.location(),
        vec![
            ErrorLocation::Field("server_review".to_owned()),
            ErrorLocation::Field("items".to_owned()),
            ErrorLocation::Index(0),
            ErrorLocation::Field("key".to_owned()),
        ]
    );
}

#[test]
fn nested_validation_runs_before_later_approval_fields() {
    let artifact = load_embedded_artifact().unwrap();
    let vector = &artifact.golden_vectors[0];
    let mut request = serde_json::to_value(&vector.approval_request.payload).unwrap();
    request["server_review"]["review_id"] = json!("Review ID");
    request["nonce"] = json!("short");

    let error = ApprovalRequest::from_value(request).unwrap_err();
    assert_eq!(
        error.location(),
        vec![
            ErrorLocation::Field("server_review".to_owned()),
            ErrorLocation::Field("review_id".to_owned()),
        ]
    );
}

#[test]
fn signed_integer_overflow_errors_keep_their_field_locations() {
    let artifact = load_embedded_artifact().unwrap();
    let vector = &artifact.golden_vectors[0];
    let cases = [
        ("schema_version", json!(300)),
        ("descriptor_version", json!(256)),
        ("policy_revision", json!(u64::MAX)),
        ("owner_github_id", json!(u64::MAX)),
    ];

    for (field, invalid_value) in cases {
        let mut request = serde_json::to_value(&vector.approval_request.payload).unwrap();
        request[field] = invalid_value;
        let error = ApprovalRequest::from_value(request).unwrap_err();
        assert_eq!(
            error.location(),
            vec![ErrorLocation::Field(field.to_owned())],
            "{field}"
        );
    }
}

#[test]
fn deserialization_shape_errors_keep_their_locations() {
    let artifact = load_embedded_artifact().unwrap();
    let vector = &artifact.golden_vectors[0];

    let mut review = serde_json::to_value(&vector.approval_request.payload.server_review).unwrap();
    review.as_object_mut().unwrap().remove("title");
    assert_eq!(
        ServerReviewPayload::from_value(review)
            .unwrap_err()
            .location(),
        vec![ErrorLocation::Field("title".to_owned())]
    );

    let mut review = serde_json::to_value(&vector.approval_request.payload.server_review).unwrap();
    review["items"][0]["label"] = json!(1);
    assert_eq!(
        ServerReviewPayload::from_value(review)
            .unwrap_err()
            .location(),
        vec![
            ErrorLocation::Field("items".to_owned()),
            ErrorLocation::Index(0),
            ErrorLocation::Field("label".to_owned()),
        ]
    );

    let mut request = serde_json::to_value(&vector.approval_request.payload).unwrap();
    request["descriptor_id"] = json!("nope");
    assert_eq!(
        ApprovalRequest::from_value(request).unwrap_err().location(),
        vec![ErrorLocation::Field("descriptor_id".to_owned())]
    );

    let mut response = serde_json::to_value(&vector.challenge_response.payload).unwrap();
    response["decision"] = json!("denied");
    assert_eq!(
        ChallengeResponse::from_value(response)
            .unwrap_err()
            .location(),
        vec![ErrorLocation::Field("decision".to_owned())]
    );
}

#[test]
fn field_validation_order_matches_launchplane_models() {
    let artifact = load_embedded_artifact().unwrap();
    let vector = &artifact.golden_vectors[0];

    let mut request = serde_json::to_value(&vector.approval_request.payload).unwrap();
    request["policy_record_id"] = json!("Policy Record");
    request["policy_sha256"] = json!("bad");
    assert_eq!(
        ApprovalRequest::from_value(request).unwrap_err().location(),
        vec![ErrorLocation::Field("policy_record_id".to_owned())]
    );

    let mut response = serde_json::to_value(&vector.challenge_response.payload).unwrap();
    response["approval_request_digest"] = json!("0".repeat(64));
    response["channel_binding_sha256"] = json!("bad");
    assert_eq!(
        ChallengeResponse::from_value(response)
            .unwrap_err()
            .location(),
        vec![ErrorLocation::Field("channel_binding_sha256".to_owned())]
    );
}

#[test]
fn embedded_v3_artifact_is_exactly_pinned() {
    let artifact = load_embedded_artifact().expect("embedded artifact should load");

    assert_eq!(artifact.schema_version, 3);
    assert_eq!(artifact.compatibility.container_schema_version, 3);
    assert_eq!(artifact.compatibility.previous_container_schema_version, 2);
    assert_eq!(artifact.signature_declaration.contract_schema_version, 2);
    assert_eq!(
        format!("{:x}", Sha256::digest(EMBEDDED_CONTRACT_JSON.as_bytes())),
        EMBEDDED_CONTRACT_SHA256
    );
}

#[test]
fn compatibility_digests_recompute_from_the_published_v2_sections() {
    let artifact = load_embedded_artifact().expect("embedded artifact should load");
    let raw: serde_json::Value = serde_json::from_str(EMBEDDED_CONTRACT_JSON).unwrap();
    let expected_sections = [
        "canonical_json",
        "canonicalization_vectors",
        "confirmation_golden_vectors",
        "golden_vectors",
        "negative_confirmation_vectors",
        "negative_vectors",
        "schemas",
        "signature_declaration",
    ];

    assert_eq!(
        artifact
            .compatibility
            .preserved_v2_section_sha256
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected_sections.into_iter().collect::<BTreeSet<_>>()
    );
    for section in expected_sections {
        assert_eq!(
            canonical_json_sha256(&raw[section]).unwrap(),
            artifact.compatibility.preserved_v2_section_sha256[section],
            "{section}"
        );
    }
}

#[test]
fn verification_state_vectors_are_complete_inert_and_proof_checked() {
    let artifact = load_embedded_artifact().expect("embedded artifact should load");
    let expected_reasons = [
        OwnerControlRejectionReason::UnknownChannelSession,
        OwnerControlRejectionReason::UnknownChallenge,
        OwnerControlRejectionReason::ChannelSessionRevoked,
        OwnerControlRejectionReason::ChannelSessionExpired,
        OwnerControlRejectionReason::ChallengeChannelSessionMismatch,
        OwnerControlRejectionReason::ChallengeExpired,
        OwnerControlRejectionReason::ChallengeReplayed,
        OwnerControlRejectionReason::StoredBindingMismatch,
        OwnerControlRejectionReason::StoredApprovalRequestMismatch,
        OwnerControlRejectionReason::SignatureInvalid,
        OwnerControlRejectionReason::AttemptBudgetExhausted,
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let actual_reasons = artifact
        .verification_state_vectors
        .iter()
        .filter_map(|vector| vector.expected.rejection_reason)
        .collect::<BTreeSet<_>>();

    assert_eq!(actual_reasons, expected_reasons);
    assert_eq!(
        artifact
            .verification_state_vectors
            .iter()
            .filter(|vector| {
                vector.expected.verification_status == OwnerControlVerificationStatus::Verified
            })
            .count(),
        1
    );
    for vector in &artifact.verification_state_vectors {
        assert_eq!(
            vector.expected.authority_state,
            OwnerControlAuthorityState::Inert,
            "{}",
            vector.name
        );
        assert_eq!(
            vector.expected.verifier_mode,
            OwnerControlVerifierMode::Shadow,
            "{}",
            vector.name
        );
        assert!(!vector.expected.authorizes_execution, "{}", vector.name);
        let envelope =
            OwnerControlConfirmationEnvelope::from_value(vector.confirmation_envelope.clone())
                .unwrap();
        assert_eq!(
            verify_confirmation_signature_proof(&envelope),
            vector.expected.rejection_reason != Some(OwnerControlRejectionReason::SignatureInvalid),
            "{}",
            vector.name
        );
    }
}

#[test]
fn lifecycle_vector_pins_only_the_published_boundary_transition() {
    let artifact = load_embedded_artifact().expect("embedded artifact should load");
    let [vector] = artifact.challenge_lifecycle_vectors.as_slice() else {
        panic!("expected one lifecycle vector");
    };
    let mut expected_terminalized = vector.issued_challenge.clone();
    expected_terminalized.state = OwnerControlChallengeState::Expired;
    expected_terminalized.terminal_event_id =
        Some(vector.expected_lifecycle_event.event_id.clone());

    assert_eq!(vector.name, "issued-to-expired-at-boundary");
    assert_eq!(
        vector.expected_terminalized_challenge,
        expected_terminalized
    );
    assert_eq!(vector.observed_at, vector.issued_challenge.expires_at);
    assert_eq!(
        vector.expected_lifecycle_event.occurred_at,
        vector.observed_at
    );
    assert_eq!(
        vector.expected_lifecycle_event.challenge_expires_at,
        vector.observed_at
    );
    assert_eq!(
        vector.expected_lifecycle_event.authority_state,
        OwnerControlAuthorityState::Inert
    );
    assert!(!vector.expected_lifecycle_event.authorizes_execution);
}

#[test]
fn v3_artifact_drift_and_unknown_fields_fail_closed() {
    let raw: serde_json::Value = serde_json::from_str(EMBEDDED_CONTRACT_JSON).unwrap();

    let mut unknown_root = raw.clone();
    unknown_root["unknown"] = json!(true);
    assert!(serde_json::from_value::<ContractArtifact>(unknown_root).is_err());

    let mut unknown_lifecycle_field = raw.clone();
    unknown_lifecycle_field["challenge_lifecycle_vectors"][0]["expected_lifecycle_event"]["envelope_sha256"] =
        json!("0".repeat(64));
    assert!(serde_json::from_value::<ContractArtifact>(unknown_lifecycle_field).is_err());

    let mut normalized_optional_field = raw.clone();
    normalized_optional_field["negative_confirmation_vectors"][0]["error_message_contains"] =
        serde_json::Value::Null;
    assert!(serde_json::from_value::<ContractArtifact>(normalized_optional_field).is_err());

    let mut wrong_version: ContractArtifact = serde_json::from_value(raw.clone()).unwrap();
    wrong_version.schema_version = 4;
    assert!(wrong_version.validate().is_err());

    let mut preserved_section_drift: ContractArtifact =
        serde_json::from_value(raw.clone()).unwrap();
    preserved_section_drift
        .schemas
        .get_mut("approval_request")
        .unwrap()["title"] = json!("drifted");
    assert!(preserved_section_drift.validate().is_err());

    let mut authorizing_vector: ContractArtifact = serde_json::from_value(raw).unwrap();
    authorizing_vector.verification_state_vectors[0]
        .expected
        .authorizes_execution = true;
    assert!(authorizing_vector.validate().is_err());

    let mut cross_record_drift: ContractArtifact =
        serde_json::from_str(EMBEDDED_CONTRACT_JSON).unwrap();
    let mismatched_session = cross_record_drift.verification_state_vectors[5]
        .channel_session
        .clone();
    cross_record_drift.verification_state_vectors[0].channel_session = mismatched_session;
    assert!(cross_record_drift.validate().is_err());
}
