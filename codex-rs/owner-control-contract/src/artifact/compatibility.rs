use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

use super::ArtifactError;
use super::ContractArtifact;
use super::OWNER_CONTROL_CONTRACT_SCHEMA_VERSION;
use super::PREVIOUS_OWNER_CONTROL_CONTRACT_SCHEMA_VERSION;
use crate::DescriptorId;
use crate::canonical_json_sha256;

const PRESERVED_V2_SECTIONS: [&str; 5] = [
    "canonical_json",
    "canonicalization_vectors",
    "negative_confirmation_vectors",
    "negative_vectors",
    "signature_declaration",
];
const PRESERVED_V2_VECTOR_SECTIONS: [&str; 2] = ["confirmation_golden_vectors", "golden_vectors"];
const PRESERVED_V2_SCHEMAS: [&str; 6] = [
    "approval_request",
    "challenge_response",
    "channel_binding_record",
    "owner_control_confirmation_envelope",
    "owner_control_signature_payload",
    "server_review_payload",
];
const PRESERVED_V4_SECTIONS: [&str; 12] = [
    "canonical_json",
    "canonicalization_vectors",
    "challenge_lifecycle_vectors",
    "compatibility",
    "confirmation_golden_vectors",
    "golden_vectors",
    "negative_confirmation_vectors",
    "negative_vectors",
    "schema_version",
    "schemas",
    "signature_declaration",
    "verification_state_vectors",
];

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityDeclaration {
    pub change_kind: String,
    pub container_schema_version: u8,
    pub enrollment_provenance_schema_versions: Vec<u8>,
    pub preserved_v2_descriptor_ids: Vec<DescriptorId>,
    pub preserved_v2_descriptor_vector_section_sha256: BTreeMap<String, String>,
    pub preserved_v2_schema_sha256: BTreeMap<String, String>,
    pub preserved_v2_section_sha256: BTreeMap<String, String>,
    pub preserved_v4_section_sha256: BTreeMap<String, String>,
    pub previous_container_schema_version: u8,
    pub shadow_verifier_schema_versions: Vec<u8>,
    pub unknown_container_versions: String,
    pub wire_model_schema_versions: Vec<u8>,
}

impl CompatibilityDeclaration {
    pub(super) fn validate(&self, artifact: &ContractArtifact) -> Result<(), ArtifactError> {
        if self.change_kind != "additive-enrollment-provenance"
            || self.container_schema_version != OWNER_CONTROL_CONTRACT_SCHEMA_VERSION
            || self.container_schema_version != artifact.schema_version
            || self.enrollment_provenance_schema_versions != [1]
            || self.preserved_v2_descriptor_ids
                != [
                    DescriptorId::ManagedAuthzPolicySet,
                    DescriptorId::ManagedSecretReencryption,
                ]
            || self.previous_container_schema_version
                != PREVIOUS_OWNER_CONTROL_CONTRACT_SCHEMA_VERSION
            || self.shadow_verifier_schema_versions != [1]
            || self.unknown_container_versions != "reject"
            || self.wire_model_schema_versions != [1]
        {
            return Err(ArtifactError::Invalid(
                "compatibility declaration does not match the published v5 contract".to_owned(),
            ));
        }

        validate_published_hashes(
            &self.preserved_v2_section_sha256,
            [
                (
                    "canonical_json",
                    "0c6b6454d737943d01d4621c217ff8412552a0bf0c69a0f50a761d38ac0e7d1f",
                ),
                (
                    "canonicalization_vectors",
                    "ca481ff769bba537310c8568b56850f5d12ebc0c90ace9ea2dc39ff714daa6a8",
                ),
                (
                    "negative_confirmation_vectors",
                    "9d529ca0f5153c8c3eb3eb4862311efc6dc3c1dc7e5df55824ebde13194eb46d",
                ),
                (
                    "negative_vectors",
                    "232d29bc542df455c9f54a3196a2a4d41cb0912155f92d060c850df99c835b29",
                ),
                (
                    "signature_declaration",
                    "7d9c62d55792931383d4a02ed99d31e21c67b5ce714c01d9144dc2a3bed34f72",
                ),
            ],
            "preserved v2 section",
        )?;
        validate_published_hashes(
            &self.preserved_v2_descriptor_vector_section_sha256,
            [
                (
                    "confirmation_golden_vectors",
                    "58391f364a79ab321596d30200b87c9d29be366eaa1386a9ff4c242c8b38d50a",
                ),
                (
                    "golden_vectors",
                    "6955c5c8bb228c21bc6a68a4ddb7cf22456cd51615ffe6e421b0fa04f15d9584",
                ),
            ],
            "preserved v2 descriptor-scoped section",
        )?;
        validate_published_hashes(
            &self.preserved_v2_schema_sha256,
            [
                (
                    "approval_request",
                    "6cc0379a8323715191aea8a605b594d6500a2937c6d46c45caa9cea313b3b8b4",
                ),
                (
                    "challenge_response",
                    "1fc009f4497b88caacf1c7273c0e7e932936cd23a24f5d3e1846f235da1082d0",
                ),
                (
                    "channel_binding_record",
                    "399e8f1ee814e60f6973290d5da0a542add174bb2926ab2cc2dff583c18c1a94",
                ),
                (
                    "owner_control_confirmation_envelope",
                    "b3e15f59f63efdeff5d95e621af981959c6d5ec2c71e1ae15351967722ee589f",
                ),
                (
                    "owner_control_signature_payload",
                    "002b5e22c57ac17b6f7915486e29f2d3d11aee7e0b44505050b1366c23ee8c00",
                ),
                (
                    "server_review_payload",
                    "b4b0d9e55212190615e9d4247ff459c15309ab1a10a715df92432585b9f34855",
                ),
            ],
            "preserved v2 schema",
        )?;
        validate_published_hashes(
            &self.preserved_v4_section_sha256,
            [
                (
                    "canonical_json",
                    "0c6b6454d737943d01d4621c217ff8412552a0bf0c69a0f50a761d38ac0e7d1f",
                ),
                (
                    "canonicalization_vectors",
                    "ca481ff769bba537310c8568b56850f5d12ebc0c90ace9ea2dc39ff714daa6a8",
                ),
                (
                    "challenge_lifecycle_vectors",
                    "81a1d62ee2c9268366e052da78221ab13d8946b276c0a06c1009296357a89807",
                ),
                (
                    "compatibility",
                    "62115e1e9d322dad345d0a48e4523445285d05281a52a4d94a18e1fcb3d27937",
                ),
                (
                    "confirmation_golden_vectors",
                    "763f4795b9b25d725f394d4df13f8a713e253429aa31457a84cc88c7f9f71b7a",
                ),
                (
                    "golden_vectors",
                    "b8e51053225c281d68d6dff7d8a1e5963ac7a2396dc48a691a4da3a12fe8b303",
                ),
                (
                    "negative_confirmation_vectors",
                    "9d529ca0f5153c8c3eb3eb4862311efc6dc3c1dc7e5df55824ebde13194eb46d",
                ),
                (
                    "negative_vectors",
                    "232d29bc542df455c9f54a3196a2a4d41cb0912155f92d060c850df99c835b29",
                ),
                (
                    "schema_version",
                    "4b227777d4dd1fc61c6f884f48641d02b4d121d3fd328cb08b5531fcacdabf8a",
                ),
                (
                    "schemas",
                    "1fdb3187f24ee64f95a8f7753ec64b93d72d1f3b92ea118db00d43a798263f0c",
                ),
                (
                    "signature_declaration",
                    "7d9c62d55792931383d4a02ed99d31e21c67b5ce714c01d9144dc2a3bed34f72",
                ),
                (
                    "verification_state_vectors",
                    "4c199bf64618845f098ac12ef992a21111ab87a6e91e5325f962db3e8174c8df",
                ),
            ],
            "preserved v4 section",
        )?;

        validate_keys(
            &self.preserved_v2_section_sha256,
            PRESERVED_V2_SECTIONS,
            "five preserved v2 sections",
        )?;
        validate_keys(
            &self.preserved_v2_descriptor_vector_section_sha256,
            PRESERVED_V2_VECTOR_SECTIONS,
            "two descriptor-scoped v2 vector sections",
        )?;
        validate_keys(
            &self.preserved_v2_schema_sha256,
            PRESERVED_V2_SCHEMAS,
            "six preserved v2 schemas",
        )?;
        validate_keys(
            &self.preserved_v4_section_sha256,
            PRESERVED_V4_SECTIONS,
            "twelve preserved v4 sections",
        )?;

        validate_hashes(
            &self.preserved_v2_section_sha256,
            [
                (
                    "canonical_json",
                    serde_json::to_value(&artifact.canonical_json)?,
                ),
                (
                    "canonicalization_vectors",
                    serde_json::to_value(&artifact.canonicalization_vectors)?,
                ),
                (
                    "negative_confirmation_vectors",
                    serde_json::to_value(&artifact.negative_confirmation_vectors)?,
                ),
                (
                    "negative_vectors",
                    serde_json::to_value(&artifact.negative_vectors)?,
                ),
                (
                    "signature_declaration",
                    serde_json::to_value(&artifact.signature_declaration)?,
                ),
            ],
            "preserved v2 section",
        )?;

        let preserved_descriptors = self
            .preserved_v2_descriptor_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let golden_vectors = artifact
            .golden_vectors
            .iter()
            .filter(|vector| preserved_descriptors.contains(&vector.descriptor_id))
            .collect::<Vec<_>>();
        let confirmation_vectors = artifact
            .confirmation_golden_vectors
            .iter()
            .filter(|vector| preserved_descriptors.contains(&vector.descriptor_id))
            .collect::<Vec<_>>();
        validate_hashes(
            &self.preserved_v2_descriptor_vector_section_sha256,
            [
                (
                    "confirmation_golden_vectors",
                    serde_json::to_value(confirmation_vectors)?,
                ),
                ("golden_vectors", serde_json::to_value(golden_vectors)?),
            ],
            "preserved v2 descriptor-scoped section",
        )?;

        for schema_name in PRESERVED_V2_SCHEMAS {
            let schema = artifact.schemas.get(schema_name).ok_or_else(|| {
                ArtifactError::Invalid(format!("missing published schema {schema_name}"))
            })?;
            let actual_sha256 = canonical_json_sha256(&preserve_v2_descriptor_enums(schema))?;
            let expected_sha256 = &self.preserved_v2_schema_sha256[schema_name];
            if actual_sha256 != *expected_sha256 {
                return Err(ArtifactError::Invalid(format!(
                    "preserved v2 schema {schema_name} has SHA-256 {actual_sha256}, expected {expected_sha256}"
                )));
            }
        }

        validate_hashes(
            &self.preserved_v4_section_sha256,
            [
                (
                    "canonical_json",
                    serde_json::to_value(&artifact.canonical_json)?,
                ),
                (
                    "canonicalization_vectors",
                    serde_json::to_value(&artifact.canonicalization_vectors)?,
                ),
                (
                    "challenge_lifecycle_vectors",
                    serde_json::to_value(&artifact.challenge_lifecycle_vectors)?,
                ),
                ("compatibility", v4_compatibility_declaration()),
                (
                    "confirmation_golden_vectors",
                    serde_json::to_value(&artifact.confirmation_golden_vectors)?,
                ),
                (
                    "golden_vectors",
                    serde_json::to_value(&artifact.golden_vectors)?,
                ),
                (
                    "negative_confirmation_vectors",
                    serde_json::to_value(&artifact.negative_confirmation_vectors)?,
                ),
                (
                    "negative_vectors",
                    serde_json::to_value(&artifact.negative_vectors)?,
                ),
                ("schema_version", Value::from(4)),
                ("schemas", serde_json::to_value(&artifact.schemas)?),
                (
                    "signature_declaration",
                    serde_json::to_value(&artifact.signature_declaration)?,
                ),
                (
                    "verification_state_vectors",
                    serde_json::to_value(&artifact.verification_state_vectors)?,
                ),
            ],
            "preserved v4 section",
        )?;
        Ok(())
    }
}

fn validate_keys<const N: usize>(
    declarations: &BTreeMap<String, String>,
    expected: [&str; N],
    description: &str,
) -> Result<(), ArtifactError> {
    let expected = expected.into_iter().collect::<BTreeSet<_>>();
    let actual = declarations
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(ArtifactError::Invalid(format!(
            "compatibility declaration must name exactly the {description}"
        )));
    }
    Ok(())
}

fn validate_hashes<const N: usize>(
    declarations: &BTreeMap<String, String>,
    sections: [(&str, Value); N],
    description: &str,
) -> Result<(), ArtifactError> {
    for (section, value) in sections {
        let actual_sha256 = canonical_json_sha256(&value)?;
        let expected_sha256 = &declarations[section];
        if actual_sha256 != *expected_sha256 {
            return Err(ArtifactError::Invalid(format!(
                "{description} {section} has SHA-256 {actual_sha256}, expected {expected_sha256}"
            )));
        }
    }
    Ok(())
}

fn validate_published_hashes<const N: usize>(
    declarations: &BTreeMap<String, String>,
    expected: [(&str, &str); N],
    description: &str,
) -> Result<(), ArtifactError> {
    for (section, expected_sha256) in expected {
        if declarations.get(section).map(String::as_str) != Some(expected_sha256) {
            return Err(ArtifactError::Invalid(format!(
                "{description} {section} does not match the published v5 digest"
            )));
        }
    }
    Ok(())
}

fn preserve_v2_descriptor_enums(value: &Value) -> Value {
    match value {
        Value::Array(values) => {
            Value::Array(values.iter().map(preserve_v2_descriptor_enums).collect())
        }
        Value::Object(values) => {
            let normalized = values
                .iter()
                .map(|(key, value)| {
                    let mut value = preserve_v2_descriptor_enums(value);
                    if key == "descriptor_id" {
                        preserve_v2_descriptor_enum(&mut value);
                    }
                    (key.clone(), value)
                })
                .collect::<serde_json::Map<_, _>>();
            Value::Object(normalized)
        }
        _ => value.clone(),
    }
}

fn preserve_v2_descriptor_enum(value: &mut Value) {
    let Value::Object(schema) = value else {
        return;
    };
    let Some(Value::Array(enum_values)) = schema.get_mut("enum") else {
        return;
    };
    let all_descriptors = BTreeSet::from([
        "managed-authz-policy-set",
        "managed-merge-train-policy-import",
        "managed-secret-reencryption",
    ]);
    let actual = enum_values
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    if enum_values.len() == all_descriptors.len() && actual == all_descriptors {
        enum_values.retain(|value| {
            matches!(
                value.as_str(),
                Some("managed-authz-policy-set" | "managed-secret-reencryption")
            )
        });
    }
}

fn v4_compatibility_declaration() -> Value {
    json!({
        "change_kind": "additive-descriptor-wire-schema-and-vectors",
        "container_schema_version": 4,
        "preserved_v2_descriptor_ids": [
            "managed-authz-policy-set",
            "managed-secret-reencryption"
        ],
        "preserved_v2_descriptor_vector_section_sha256": {
            "confirmation_golden_vectors": "58391f364a79ab321596d30200b87c9d29be366eaa1386a9ff4c242c8b38d50a",
            "golden_vectors": "6955c5c8bb228c21bc6a68a4ddb7cf22456cd51615ffe6e421b0fa04f15d9584"
        },
        "preserved_v2_schema_sha256": {
            "approval_request": "6cc0379a8323715191aea8a605b594d6500a2937c6d46c45caa9cea313b3b8b4",
            "challenge_response": "1fc009f4497b88caacf1c7273c0e7e932936cd23a24f5d3e1846f235da1082d0",
            "channel_binding_record": "399e8f1ee814e60f6973290d5da0a542add174bb2926ab2cc2dff583c18c1a94",
            "owner_control_confirmation_envelope": "b3e15f59f63efdeff5d95e621af981959c6d5ec2c71e1ae15351967722ee589f",
            "owner_control_signature_payload": "002b5e22c57ac17b6f7915486e29f2d3d11aee7e0b44505050b1366c23ee8c00",
            "server_review_payload": "b4b0d9e55212190615e9d4247ff459c15309ab1a10a715df92432585b9f34855"
        },
        "preserved_v2_section_sha256": {
            "canonical_json": "0c6b6454d737943d01d4621c217ff8412552a0bf0c69a0f50a761d38ac0e7d1f",
            "canonicalization_vectors": "ca481ff769bba537310c8568b56850f5d12ebc0c90ace9ea2dc39ff714daa6a8",
            "negative_confirmation_vectors": "9d529ca0f5153c8c3eb3eb4862311efc6dc3c1dc7e5df55824ebde13194eb46d",
            "negative_vectors": "232d29bc542df455c9f54a3196a2a4d41cb0912155f92d060c850df99c835b29",
            "signature_declaration": "7d9c62d55792931383d4a02ed99d31e21c67b5ce714c01d9144dc2a3bed34f72"
        },
        "previous_container_schema_version": 3,
        "schema_change": "descriptor literal expanded for managed-merge-train-policy-import",
        "shadow_verifier_schema_versions": [1],
        "unknown_container_versions": "reject",
        "wire_model_schema_versions": [1]
    })
}
