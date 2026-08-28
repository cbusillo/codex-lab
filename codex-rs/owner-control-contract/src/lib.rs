mod artifact;
mod canonical;
mod decode;
mod model;

pub use artifact::ArtifactError;
pub use artifact::CanonicalJsonSpec;
pub use artifact::CanonicalizationVector;
pub use artifact::ContractArtifact;
pub use artifact::EMBEDDED_CONTRACT_JSON;
pub use artifact::EMBEDDED_CONTRACT_SHA256;
pub use artifact::GoldenPayload;
pub use artifact::GoldenVector;
pub use artifact::NegativeModel;
pub use artifact::NegativeVector;
pub use artifact::load_embedded_artifact;
pub use artifact::parse_negative_payload;
pub use canonical::CanonicalJsonError;
pub use canonical::canonical_json_bytes;
pub use canonical::canonical_json_sha256;
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

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
