use std::cell::Cell;
use std::collections::BTreeSet;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codex_owner_control_contract::ChallengeResponse;
use codex_owner_control_contract::ChannelBindingRecord;
use codex_owner_control_contract::OwnerControlConfirmationEnvelope;
use codex_owner_control_contract::OwnerControlGestureSourceClaim;
use codex_owner_control_contract::OwnerControlKeyCustodyClaim;
use codex_owner_control_contract::OwnerControlPrincipalSeparationClaim;
use codex_owner_control_contract::OwnerControlProvenanceTier;
use codex_owner_control_contract::OwnerControlServerObservedCorroboration;
use codex_owner_control_contract::channel_binding_sha256;
use codex_owner_control_contract::load_embedded_artifact;
use codex_owner_control_contract::signature_payload_bytes;
use ed25519_dalek::Signer as _;
use ed25519_dalek::SigningKey;
use pretty_assertions::assert_eq;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::*;

#[test]
fn current_host_observation_is_sealed_self_asserted_and_inert() {
    let observed = ObservedOwnerControlHost::current(
        "current-owner-control-host",
        "current-owner-control-principal",
    )
    .unwrap();

    assert_eq!(
        observed.principal_claim().principal_separation,
        OwnerControlPrincipalSeparationClaim::NotClaimed
    );
    assert_eq!(
        observed.principal_claim().key_custody,
        OwnerControlKeyCustodyClaim::NotClaimed
    );
    assert_eq!(
        observed.principal_claim().gesture_source,
        OwnerControlGestureSourceClaim::NotClaimed
    );
    assert_eq!(
        observed.server_observed_corroboration(),
        OwnerControlServerObservedCorroboration::None
    );
    assert_eq!(
        observed.provenance_tier(),
        OwnerControlProvenanceTier::SelfAsserted
    );
    assert_eq!(
        observed.authority_state(),
        OwnerControlAuthorityState::Inert
    );
    assert!(!observed.authorizes_execution());
}

#[test]
fn enrollment_intent_binds_observation_to_exact_channel_without_authority() {
    let (_, mut binding, _) = synthetic_challenge();
    binding.owner_public_key =
        URL_SAFE_NO_PAD.encode(SigningKey::from_bytes(&[11; 32]).verifying_key().to_bytes());
    let observed = ObservedOwnerControlHost::current(
        "current-owner-control-host",
        "current-owner-control-principal",
    )
    .unwrap();

    let intent = observed.bind_channel(binding.clone()).unwrap();

    assert_eq!(intent.channel_binding(), &binding);
    assert_eq!(
        intent.channel_binding_sha256(),
        channel_binding_sha256(&binding).unwrap()
    );
    assert_eq!(intent.observed_host(), &observed);
    assert_eq!(
        intent.principal_claim_sha256(),
        codex_owner_control_contract::owner_control_host_principal_claim_sha256(
            observed.principal_claim()
        )
        .unwrap()
    );
    assert!(!intent.authorizes_execution());
}

#[test]
fn enrollment_intent_rejects_published_conformance_keys() {
    let binding = load_embedded_artifact()
        .unwrap()
        .confirmation_golden_vectors
        .into_iter()
        .next()
        .unwrap()
        .channel_binding
        .payload;
    let observed = ObservedOwnerControlHost::current(
        "current-owner-control-host",
        "current-owner-control-principal",
    )
    .unwrap();

    assert!(observed.bind_channel(binding).is_err());
}

#[test]
fn confirms_a_canonical_challenge_as_a_complete_golden_envelope() {
    let (challenge, binding, signing_key) = synthetic_challenge();
    let expected = expected_envelope(&challenge, &binding, &signing_key);
    let presented =
        PresentedOwnerConfirmation::new(challenge.approval_request.clone(), binding).unwrap();
    assert_eq!(
        presented.review(),
        &challenge.approval_request.server_review
    );
    let (flow, gesture) = presented.acknowledge_owner();
    let custody = SyntheticCustody::new(signing_key);

    let envelope = flow
        .confirm(
            gesture,
            &FixedClock::at(&challenge.confirmed_at),
            &custody,
            &mut InMemoryReplayStore::default(),
        )
        .unwrap()
        .into_envelope();

    assert_eq!(envelope, expected);
    assert_eq!(custody.calls.get(), 1);
}

#[test]
fn published_structural_confirmation_negatives_remain_rejected() {
    let artifact = load_embedded_artifact().unwrap();
    for vector in artifact
        .negative_confirmation_vectors
        .iter()
        .filter(|vector| vector.verification.is_none())
    {
        assert!(
            OwnerControlConfirmationEnvelope::from_value(vector.payload.clone()).is_err(),
            "{}",
            vector.rule
        );
    }
}

#[test]
fn gesture_from_one_challenge_cannot_confirm_another() {
    let (first_challenge, first_binding, _) = synthetic_challenge();
    let (second_challenge, second_binding, signing_key) = synthetic_challenge_with_seed([8; 32]);
    let (_, first_gesture) =
        PresentedOwnerConfirmation::new(first_challenge.approval_request, first_binding)
            .unwrap()
            .acknowledge_owner();
    let (second_flow, _) =
        PresentedOwnerConfirmation::new(second_challenge.approval_request.clone(), second_binding)
            .unwrap()
            .acknowledge_owner();
    let custody = SyntheticCustody::new(signing_key);

    let error = second_flow
        .confirm(
            first_gesture,
            &FixedClock::at(&second_challenge.confirmed_at),
            &custody,
            &mut InMemoryReplayStore::default(),
        )
        .unwrap_err();

    assert_eq!(error, ConfirmationError::GestureMismatch);
    assert_eq!(custody.calls.get(), 0);
}

#[test]
fn expiry_between_presentation_and_confirmation_never_calls_custody() {
    let (challenge, binding, signing_key) = synthetic_challenge();
    let (flow, gesture) = PresentedOwnerConfirmation::new(challenge.approval_request, binding)
        .unwrap()
        .acknowledge_owner();
    let custody = SyntheticCustody::new(signing_key);

    let error = flow
        .confirm(
            gesture,
            &FixedClock::at("2030-01-02T03:09:06+00:00"),
            &custody,
            &mut InMemoryReplayStore::default(),
        )
        .unwrap_err();

    assert_eq!(error, ConfirmationError::ChallengeExpired);
    assert_eq!(custody.calls.get(), 0);
}

#[test]
fn replay_rejection_precedes_custody() {
    let (challenge, binding, signing_key) = synthetic_challenge();
    let (first_flow, first_gesture) =
        PresentedOwnerConfirmation::new(challenge.approval_request.clone(), binding.clone())
            .unwrap()
            .acknowledge_owner();
    let (second_flow, second_gesture) =
        PresentedOwnerConfirmation::new(challenge.approval_request.clone(), binding)
            .unwrap()
            .acknowledge_owner();
    let custody = SyntheticCustody::new(signing_key);
    let mut replay_store = InMemoryReplayStore::default();

    first_flow
        .confirm(
            first_gesture,
            &FixedClock::at(&challenge.confirmed_at),
            &custody,
            &mut replay_store,
        )
        .unwrap();
    let error = second_flow
        .confirm(
            second_gesture,
            &FixedClock::at(&challenge.confirmed_at),
            &custody,
            &mut replay_store,
        )
        .unwrap_err();

    assert_eq!(error, ConfirmationError::ReplayRejected);
    assert_eq!(custody.calls.get(), 1);
}

#[test]
fn custody_failure_is_redacted_and_never_returns_an_envelope() {
    let (challenge, binding, _) = synthetic_challenge();
    let (flow, gesture) =
        PresentedOwnerConfirmation::new(challenge.approval_request.clone(), binding)
            .unwrap()
            .acknowledge_owner();
    let custody = FailingCustody::default();

    let error = flow
        .confirm(
            gesture,
            &FixedClock::at(&challenge.confirmed_at),
            &custody,
            &mut InMemoryReplayStore::default(),
        )
        .unwrap_err();

    assert_eq!(error, ConfirmationError::CustodyUnavailable);
    assert_eq!(format!("{error:?}"), "CustodyUnavailable");
    assert_eq!(
        format!("{:?}", OwnerSigningFailure::unavailable()),
        "OwnerSigningFailure"
    );
    assert_eq!(custody.calls.get(), 1);
}

#[test]
fn custody_must_sign_with_the_enrolled_session_key() {
    let (challenge, binding, _) = synthetic_challenge();
    let (flow, gesture) =
        PresentedOwnerConfirmation::new(challenge.approval_request.clone(), binding)
            .unwrap()
            .acknowledge_owner();
    let custody = SyntheticCustody::new(SigningKey::from_bytes(&[9; 32]));

    let error = flow
        .confirm(
            gesture,
            &FixedClock::at(&challenge.confirmed_at),
            &custody,
            &mut InMemoryReplayStore::default(),
        )
        .unwrap_err();

    assert_eq!(error, ConfirmationError::InvalidCustodySignature);
    assert_eq!(custody.calls.get(), 1);
}

#[test]
fn acknowledgement_never_auto_signs() {
    let (challenge, binding, signing_key) = synthetic_challenge();
    let custody = SyntheticCustody::new(signing_key);
    let (flow, gesture) =
        PresentedOwnerConfirmation::new(challenge.approval_request.clone(), binding)
            .unwrap()
            .acknowledge_owner();

    assert_eq!(custody.calls.get(), 0);
    flow.confirm(
        gesture,
        &FixedClock::at(&challenge.confirmed_at),
        &custody,
        &mut InMemoryReplayStore::default(),
    )
    .unwrap();
    assert_eq!(custody.calls.get(), 1);
}

fn synthetic_challenge() -> (ChallengeResponse, ChannelBindingRecord, SigningKey) {
    synthetic_challenge_with_seed([7; 32])
}

fn synthetic_challenge_with_seed(
    seed: [u8; 32],
) -> (ChallengeResponse, ChannelBindingRecord, SigningKey) {
    let artifact = load_embedded_artifact().unwrap();
    let vector = artifact
        .confirmation_golden_vectors
        .into_iter()
        .next()
        .unwrap();
    let signing_key = SigningKey::from_bytes(&seed);
    let mut binding = vector.channel_binding.payload;
    binding.owner_public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let mut challenge = vector.challenge_response.payload;
    challenge.channel_binding_sha256 = channel_binding_sha256(&binding).unwrap();
    challenge.approval_request_digest =
        codex_owner_control_contract::approval_request_digest(&challenge.approval_request).unwrap();
    (challenge, binding, signing_key)
}

fn expected_envelope(
    challenge: &ChallengeResponse,
    binding: &ChannelBindingRecord,
    signing_key: &SigningKey,
) -> OwnerControlConfirmationEnvelope {
    let signature = signing_key.sign(&signature_payload_bytes(challenge).unwrap());
    OwnerControlConfirmationEnvelope {
        schema_version: codex_owner_control_contract::OWNER_CONTROL_SCHEMA_VERSION,
        channel_binding: binding.clone(),
        challenge_response: challenge.clone(),
        signature_algorithm: codex_owner_control_contract::OWNER_CONTROL_SIGNATURE_ALGORITHM
            .to_owned(),
        signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
    }
}

struct FixedClock(OffsetDateTime);

impl FixedClock {
    fn at(value: &str) -> Self {
        Self(OffsetDateTime::parse(value, &Rfc3339).unwrap())
    }
}

impl OwnerClock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

struct SyntheticCustody {
    signing_key: SigningKey,
    calls: Cell<usize>,
}

impl SyntheticCustody {
    fn new(signing_key: SigningKey) -> Self {
        Self {
            signing_key,
            calls: Cell::new(0),
        }
    }
}

impl OwnerSigningCustody for SyntheticCustody {
    fn sign_owner_confirmation(&self, payload: &[u8]) -> Result<String, OwnerSigningFailure> {
        self.calls.set(self.calls.get() + 1);
        Ok(URL_SAFE_NO_PAD.encode(self.signing_key.sign(payload).to_bytes()))
    }
}

#[derive(Default)]
struct FailingCustody {
    calls: Cell<usize>,
}

impl OwnerSigningCustody for FailingCustody {
    fn sign_owner_confirmation(&self, _: &[u8]) -> Result<String, OwnerSigningFailure> {
        self.calls.set(self.calls.get() + 1);
        Err(OwnerSigningFailure::unavailable())
    }
}

#[derive(Default)]
struct InMemoryReplayStore(BTreeSet<String>);

impl OwnerControlReplayStore for InMemoryReplayStore {
    fn check_and_insert(&mut self, challenge_digest: &str) -> Result<(), ReplayStoreFailure> {
        if !self.0.insert(challenge_digest.to_owned()) {
            return Err(ReplayStoreFailure::rejected());
        }
        Ok(())
    }
}
