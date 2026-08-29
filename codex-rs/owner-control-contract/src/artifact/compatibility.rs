use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::Deserialize;
use serde::Serialize;

use super::ArtifactError;
use super::ContractArtifact;
use super::OWNER_CONTROL_CONTRACT_SCHEMA_VERSION;
use super::PREVIOUS_OWNER_CONTROL_CONTRACT_SCHEMA_VERSION;
use crate::canonical_json_sha256;

const PRESERVED_V2_SECTIONS: [&str; 8] = [
    "canonical_json",
    "canonicalization_vectors",
    "confirmation_golden_vectors",
    "golden_vectors",
    "negative_confirmation_vectors",
    "negative_vectors",
    "schemas",
    "signature_declaration",
];

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityDeclaration {
    pub change_kind: String,
    pub container_schema_version: u8,
    pub preserved_v2_section_sha256: BTreeMap<String, String>,
    pub previous_container_schema_version: u8,
    pub shadow_verifier_schema_versions: Vec<u8>,
    pub unknown_container_versions: String,
    pub wire_model_schema_versions: Vec<u8>,
}

impl CompatibilityDeclaration {
    pub(super) fn validate(&self, artifact: &ContractArtifact) -> Result<(), ArtifactError> {
        if self.change_kind != "additive-server-state-vectors"
            || self.container_schema_version != OWNER_CONTROL_CONTRACT_SCHEMA_VERSION
            || self.container_schema_version != artifact.schema_version
            || self.previous_container_schema_version
                != PREVIOUS_OWNER_CONTROL_CONTRACT_SCHEMA_VERSION
            || self.shadow_verifier_schema_versions != [1]
            || self.unknown_container_versions != "reject"
            || self.wire_model_schema_versions != [1]
        {
            return Err(ArtifactError::Invalid(
                "compatibility declaration does not match the published v3 contract".to_owned(),
            ));
        }

        let expected_sections = PRESERVED_V2_SECTIONS.into_iter().collect::<BTreeSet<_>>();
        let actual_sections = self
            .preserved_v2_section_sha256
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if actual_sections != expected_sections {
            return Err(ArtifactError::Invalid(
                "compatibility declaration must name exactly the eight preserved v2 sections"
                    .to_owned(),
            ));
        }

        let preserved_sections = [
            (
                "canonical_json",
                serde_json::to_value(&artifact.canonical_json)?,
            ),
            (
                "canonicalization_vectors",
                serde_json::to_value(&artifact.canonicalization_vectors)?,
            ),
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
            ("schemas", serde_json::to_value(&artifact.schemas)?),
            (
                "signature_declaration",
                serde_json::to_value(&artifact.signature_declaration)?,
            ),
        ];
        for (section, value) in preserved_sections {
            let actual_sha256 = canonical_json_sha256(&value)?;
            let expected_sha256 = &self.preserved_v2_section_sha256[section];
            if actual_sha256 != *expected_sha256 {
                return Err(ArtifactError::Invalid(format!(
                    "preserved v2 section {section} has SHA-256 {actual_sha256}, expected {expected_sha256}"
                )));
            }
        }
        Ok(())
    }
}
