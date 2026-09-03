mod artifact;
mod canonical;
mod confirmation;
mod decode;
mod model;
mod provenance;

pub use artifact::ArtifactError;
pub use artifact::CanonicalJsonSpec;
pub use artifact::CanonicalizationVector;
pub use artifact::ChallengeLifecycleEvent;
pub use artifact::ChallengeLifecycleVector;
pub use artifact::CompatibilityDeclaration;
pub use artifact::ConfirmationGoldenVector;
pub use artifact::ContractArtifact;
pub use artifact::EMBEDDED_CONTRACT_JSON;
pub use artifact::EMBEDDED_CONTRACT_SHA256;
pub use artifact::GoldenPayload;
pub use artifact::GoldenVector;
pub use artifact::NegativeConfirmationModel;
pub use artifact::NegativeConfirmationVector;
pub use artifact::NegativeModel;
pub use artifact::NegativeProvenanceModel;
pub use artifact::NegativeProvenanceOperation;
pub use artifact::NegativeProvenanceResult;
pub use artifact::NegativeProvenanceVector;
pub use artifact::NegativeVector;
pub use artifact::OWNER_CONTROL_CONTRACT_SCHEMA_VERSION;
pub use artifact::OwnerControlAuthorityState;
pub use artifact::OwnerControlChallengeRecord;
pub use artifact::OwnerControlChallengeState;
pub use artifact::OwnerControlChannelSessionRecord;
pub use artifact::OwnerControlChannelSessionStatus;
pub use artifact::OwnerControlRejectionReason;
pub use artifact::OwnerControlTransitionReason;
pub use artifact::OwnerControlVerificationStatus;
pub use artifact::OwnerControlVerifierMode;
pub use artifact::ProvenanceDeclaration;
pub use artifact::ProvenanceVector;
pub use artifact::SignatureDeclaration;
pub use artifact::VerificationOutcome;
pub use artifact::VerificationStateExpectation;
pub use artifact::VerificationStateVector;
pub use artifact::load_embedded_artifact;
pub use artifact::parse_negative_confirmation_payload;
pub use artifact::parse_negative_payload;
pub use canonical::CanonicalJsonError;
pub use canonical::canonical_json_bytes;
pub use canonical::canonical_json_sha256;
pub use confirmation::ChannelBindingRecord;
pub use confirmation::OWNER_CONTROL_SIGNATURE_ALGORITHM;
pub use confirmation::OWNER_CONTROL_SIGNATURE_DOMAIN;
pub use confirmation::OwnerControlConfirmationEnvelope;
pub use confirmation::OwnerControlSignaturePayload;
pub use confirmation::channel_binding_sha256;
pub use confirmation::signature_payload;
pub use confirmation::signature_payload_bytes;
pub use confirmation::verify_confirmation_signature_proof;
pub use model::ApprovalRequest;
pub use model::ChallengeResponse;
pub use model::Decision;
pub use model::DescriptorId;
pub use model::ErrorLocation;
pub use model::OWNER_CONTROL_SCHEMA_VERSION;
pub use model::ReviewItem;
pub use model::ServerReviewPayload;
pub use model::ValidationError;
pub use model::approval_request_digest;
pub use model::challenge_response_digest;
pub use provenance::OWNER_CONTROL_ENROLLMENT_PROVENANCE_SCHEMA_VERSION;
pub use provenance::OwnerControlEnrollmentContext;
pub use provenance::OwnerControlEnrollmentProvenance;
pub use provenance::OwnerControlGestureSourceClaim;
pub use provenance::OwnerControlHostPrincipalClaim;
pub use provenance::OwnerControlKeyCustodyClaim;
pub use provenance::OwnerControlPrincipalSeparationClaim;
pub use provenance::OwnerControlProvenanceResult;
pub use provenance::OwnerControlProvenanceTier;
pub use provenance::OwnerControlServerObservedCorroboration;
pub use provenance::derive_owner_control_provenance_tier;
pub use provenance::is_published_owner_control_synthetic_public_key;
pub use provenance::owner_control_host_principal_claim_sha256;
pub use provenance::owner_control_public_key_sha256;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "confirmation_tests.rs"]
mod confirmation_tests;

#[cfg(test)]
#[path = "provenance_tests.rs"]
mod provenance_tests;
