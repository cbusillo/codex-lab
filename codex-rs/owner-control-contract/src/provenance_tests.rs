use std::collections::BTreeSet;

use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;

#[test]
fn published_provenance_vectors_are_exhaustive_canonical_and_inert() {
    let artifact = load_embedded_artifact().unwrap();
    let combinations = artifact
        .provenance_vectors
        .iter()
        .map(|vector| {
            assert_eq!(
                vector
                    .enrollment_provenance
                    .payload
                    .host_principal_claim()
                    .unwrap(),
                vector.claim.payload
            );
            assert_eq!(
                vector.result,
                OwnerControlProvenanceResult {
                    authority_state: OwnerControlAuthorityState::Inert,
                    authorizes_execution: false,
                    provenance_tier: OwnerControlProvenanceTier::SelfAsserted,
                    server_observed_corroboration: OwnerControlServerObservedCorroboration::None,
                }
            );
            (
                vector.claim.payload.principal_separation,
                vector.claim.payload.key_custody,
                vector.claim.payload.gesture_source,
            )
        })
        .collect::<BTreeSet<_>>();

    assert_eq!(artifact.provenance_vectors.len(), 18);
    assert_eq!(combinations.len(), 18);
}

#[test]
fn published_negative_provenance_vectors_fail_closed() {
    let artifact = load_embedded_artifact().unwrap();
    let mut structural_count = 0;
    let mut runtime_count = 0;
    for vector in &artifact.negative_provenance_vectors {
        match (vector.model, &vector.payload, &vector.error_location) {
            (
                Some(NegativeProvenanceModel::OwnerControlHostPrincipalClaim),
                Some(payload),
                Some(location),
            ) => {
                structural_count += 1;
                assert_eq!(
                    OwnerControlHostPrincipalClaim::from_value(payload.clone())
                        .unwrap_err()
                        .location(),
                    *location
                );
            }
            (
                Some(NegativeProvenanceModel::OwnerControlEnrollmentProvenance),
                Some(payload),
                Some(location),
            ) => {
                structural_count += 1;
                assert_eq!(
                    OwnerControlEnrollmentProvenance::from_value(payload.clone())
                        .unwrap_err()
                        .location(),
                    *location
                );
            }
            (None, None, None) => runtime_count += 1,
            _ => panic!("unsupported negative provenance vector: {}", vector.rule),
        }
    }
    assert_eq!((structural_count, runtime_count), (4, 4));
}

#[test]
fn provenance_models_reject_unknown_fields_and_versions() {
    let artifact = load_embedded_artifact().unwrap();
    let vector = &artifact.provenance_vectors[0];

    let mut unknown_claim = serde_json::to_value(&vector.claim.payload).unwrap();
    unknown_claim["unexpected"] = json!(true);
    assert!(OwnerControlHostPrincipalClaim::from_value(unknown_claim).is_err());

    let mut unknown_provenance =
        serde_json::to_value(&vector.enrollment_provenance.payload).unwrap();
    unknown_provenance["unexpected"] = json!(true);
    assert!(OwnerControlEnrollmentProvenance::from_value(unknown_provenance).is_err());

    let mut future_claim = serde_json::to_value(&vector.claim.payload).unwrap();
    future_claim["schema_version"] = json!(2);
    assert_eq!(
        OwnerControlHostPrincipalClaim::from_value(future_claim)
            .unwrap_err()
            .location(),
        vec![ErrorLocation::Field("schema_version".to_owned())]
    );
}
