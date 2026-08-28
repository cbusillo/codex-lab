use std::collections::BTreeSet;

use pretty_assertions::assert_eq;

use super::*;

#[test]
fn confirmation_golden_vectors_match_exact_bytes_and_verify() {
    let artifact = load_embedded_artifact().expect("embedded artifact should load");
    for vector in artifact.confirmation_golden_vectors {
        assert_golden_payload(&vector.channel_binding);
        assert_golden_payload(&vector.challenge_response);
        assert_golden_payload(&vector.signature_payload);
        assert_golden_payload(&vector.confirmation_envelope);
        assert_eq!(
            channel_binding_sha256(&vector.channel_binding.payload).unwrap(),
            vector.channel_binding.sha256
        );
        assert_eq!(
            signature_payload(&vector.challenge_response.payload),
            vector.signature_payload.payload
        );
        assert_eq!(
            signature_payload_bytes(&vector.challenge_response.payload).unwrap(),
            vector.signature_payload.canonical_json.as_bytes()
        );
        assert_eq!(vector.verification, VerificationOutcome::Valid);
        assert!(verify_confirmation_signature_proof(
            &vector.confirmation_envelope.payload
        ));
    }
}

#[test]
fn structural_confirmation_negatives_fail_at_published_locations_and_messages() {
    let artifact = load_embedded_artifact().expect("embedded artifact should load");
    for vector in artifact
        .negative_confirmation_vectors
        .iter()
        .filter(|vector| vector.verification.is_none())
    {
        let error = parse_negative_confirmation_payload(vector).expect_err(&vector.rule);
        let ArtifactError::Validation(error) = error else {
            panic!("{} returned a non-validation error: {error}", vector.rule);
        };
        assert_eq!(error.location(), vector.error_location, "{}", vector.rule);
        if let Some(message) = &vector.error_message_contains {
            assert!(
                error.to_string().contains(message),
                "{} returned unexpected message: {error}",
                vector.rule
            );
        }
    }
}

#[test]
fn proof_confirmation_negatives_parse_but_fail_signature_verification() {
    let artifact = load_embedded_artifact().expect("embedded artifact should load");
    let mut proof_rules = BTreeSet::new();
    for vector in artifact
        .negative_confirmation_vectors
        .iter()
        .filter(|vector| vector.verification == Some(VerificationOutcome::Invalid))
    {
        let envelope = parse_negative_confirmation_payload(vector)
            .unwrap_or_else(|error| panic!("{} did not parse: {error}", vector.rule));
        assert!(
            !verify_confirmation_signature_proof(&envelope),
            "{}",
            vector.rule
        );
        proof_rules.insert(vector.rule.as_str());
    }
    assert_eq!(
        proof_rules,
        BTreeSet::from([
            "cross-session-substitution-is-rejected",
            "signature-from-wrong-private-key-is-rejected",
            "tampered-signed-payload-is-rejected",
        ])
    );
}

#[test]
fn legacy_golden_binding_digests_remain_explicit_placeholders() {
    let artifact = load_embedded_artifact().expect("embedded artifact should load");
    assert_eq!(
        artifact.signature_declaration.legacy_golden_channel_binding,
        "synthetic-placeholder-not-channel-binding-record"
    );
    let confirmation_digests = artifact
        .confirmation_golden_vectors
        .iter()
        .map(|vector| vector.channel_binding.sha256.as_str())
        .collect::<BTreeSet<_>>();
    for vector in artifact.golden_vectors {
        assert!(
            !confirmation_digests.contains(
                vector
                    .challenge_response
                    .payload
                    .channel_binding_sha256
                    .as_str()
            )
        );
    }
}

#[test]
fn confirmation_models_reject_unknown_fields() {
    let artifact = load_embedded_artifact().expect("embedded artifact should load");
    let vector = &artifact.confirmation_golden_vectors[0];

    let mut binding = serde_json::to_value(&vector.channel_binding.payload).unwrap();
    binding["unknown"] = serde_json::json!(true);
    assert_eq!(
        ChannelBindingRecord::from_value(binding)
            .unwrap_err()
            .location(),
        vec![ErrorLocation::Field("unknown".to_owned())]
    );

    let mut payload = serde_json::to_value(&vector.signature_payload.payload).unwrap();
    payload["unknown"] = serde_json::json!(true);
    assert_eq!(
        OwnerControlSignaturePayload::from_value(payload)
            .unwrap_err()
            .location(),
        vec![ErrorLocation::Field("unknown".to_owned())]
    );

    let mut envelope = serde_json::to_value(&vector.confirmation_envelope.payload).unwrap();
    envelope["unknown"] = serde_json::json!(true);
    assert_eq!(
        OwnerControlConfirmationEnvelope::from_value(envelope)
            .unwrap_err()
            .location(),
        vec![ErrorLocation::Field("unknown".to_owned())]
    );
}

fn assert_golden_payload<T: serde::Serialize>(payload: &GoldenPayload<T>) {
    let value = serde_json::to_value(&payload.payload).unwrap();
    assert_eq!(
        canonical_json_bytes(&value).unwrap(),
        payload.canonical_json.as_bytes()
    );
    assert_eq!(canonical_json_sha256(&value).unwrap(), payload.sha256);
}
