use super::*;
use crate::OperationId;
use crate::authority::{
    ProposalApproval, ProposalStep, Signer, StepDigest,
    trusted_key::tests::{Answer, Authenticator, HONEST},
};

const ISSUED: u64 = 1_000_000;
const WINDOW: u64 = 10 * 60 * 1000;

fn at(millis: u64) -> Timestamp {
    Timestamp::from_millis(millis).unwrap()
}

fn cluster() -> SealingPublicKey {
    SealingPublicKey::from_bytes([5; 32])
}

fn device() -> Signer {
    Signer::from_seed(&[1; 32])
}

fn here() -> KeyMember {
    KeyMember {
        id: MachineId::parse("a".repeat(32)).unwrap(),
        public_key: WireGuardPublicKey([7; 32]),
    }
}

fn operation() -> OperationId {
    OperationId::parse("0f8fad5bd9cb469fa16570867728950e").unwrap()
}

fn trust(members: Vec<KeyMember>, signer: SignerKey) -> KeyStep {
    KeyStep {
        cluster: cluster(),
        change: KeyChange::Trust {
            key: NewKey::Device {
                key: DeviceKey::from_bytes([3; 32]),
            },
            members,
        },
        signer,
        proposal: None,
    }
}

fn removal(signer: SignerKey) -> KeyStep {
    KeyStep {
        cluster: cluster(),
        change: KeyChange::Remove {
            key: KeyRef::Device {
                key: DeviceKey::from_bytes([3; 32]),
            },
        },
        signer,
        proposal: None,
    }
}

fn by_device() -> SignerKey {
    SignerKey::Device(device().public().into())
}

/// `step` as step 0 of a bundle signed by `sign`, which names `signer`.
fn bundled(
    mut step: KeyStep,
    signer: SignerKey,
    sign: impl FnOnce(&ProposalApproval) -> Signed<ProposalApproval>,
) -> Signed<KeyStep> {
    step.proposal = Some(ProposalRef {
        operation: operation(),
        index: 0,
    });
    let unsigned = Signed::unsigned(&step).unwrap();
    let plan = ProposalApproval {
        cluster: cluster(),
        environment: Namespace::parse("app").unwrap(),
        operation: operation(),
        steps: vec![ProposalStep {
            index: 0,
            kind: StepKind::Key,
            digest: StepDigest::of(&unsigned.body),
        }],
        issued_at: at(ISSUED),
        expires_at: at(ISSUED + WINDOW),
        signer,
    };
    Signed::proposed(&step, sign(&plan)).unwrap()
}

fn device_bundle(step: KeyStep) -> Signed<KeyStep> {
    bundled(step, by_device(), |plan| {
        Signed::sign(plan, &device()).unwrap()
    })
}

fn trusted() -> TrustedKeys {
    TrustedKeys {
        devices: vec![device().public().into()],
        passkeys: Vec::new(),
    }
}

fn facts(trusted: &TrustedKeys, now: u64) -> KeyFacts<'_> {
    KeyFacts {
        trusted,
        cluster: Some(cluster()),
        member: here(),
        now: at(now),
    }
}

#[test]
fn a_trust_applies_only_inside_its_bundles_window() {
    let step = device_bundle(trust(vec![here()], by_device()));
    let trusted = trusted();
    assert!(verify(&step, &facts(&trusted, ISSUED + 60_000)).is_ok());
    assert_eq!(
        verify(&step, &facts(&trusted, ISSUED + 18 * 60 * 1000)).unwrap_err(),
        AuthorityRefusal::Expired
    );
}

#[test]
fn a_trust_signed_alone_is_refused_since_it_has_no_window() {
    let alone = Signed::sign(&trust(vec![here()], by_device()), &device()).unwrap();
    assert_eq!(
        verify(&alone, &facts(&trusted(), ISSUED)).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: "a proposal"
        }
    );
}

#[test]
fn a_trust_lands_only_on_a_member_it_names() {
    let reset = KeyMember {
        public_key: WireGuardPublicKey([8; 32]),
        ..here()
    };
    let step = device_bundle(trust(vec![reset], by_device()));
    assert_eq!(
        verify(&step, &facts(&trusted(), ISSUED)).unwrap_err(),
        AuthorityRefusal::Mismatch {
            field: "this Machine"
        }
    );
}

#[test]
fn a_removal_stands_after_its_window_alone_or_in_a_bundle() {
    let late = ISSUED + 60 * 60 * 1000;
    let trusted = trusted();
    let alone = Signed::sign(&removal(by_device()), &device()).unwrap();
    assert!(verify(&alone, &facts(&trusted, late)).is_ok());
    let step = device_bundle(removal(by_device()));
    assert!(verify(&step, &facts(&trusted, late)).is_ok());
}

#[test]
fn a_passkey_tap_on_a_bundle_trusts_a_key() {
    let authenticator = Authenticator::eddsa([9; 32]);
    let trusted = TrustedKeys {
        devices: Vec::new(),
        passkeys: vec![crate::authority::TrustedPasskey {
            passkey: authenticator.passkey(),
            approved_by: crate::authority::PasskeyApprover::Server,
        }],
    };
    let passkey = SignerKey::Passkey(crate::authority::OwnPrompt::Passkey);
    let step = bundled(trust(vec![here()], passkey), passkey, |plan| {
        let unsigned = Signed::unsigned(plan).unwrap();
        let challenge = unsigned.challenge();
        unsigned.answered(authenticator.answer(&Answer {
            challenge: &challenge,
            ..HONEST
        }))
    });
    let (_, by) = verify(&step, &facts(&trusted, ISSUED)).unwrap();
    assert_eq!(by, Approver::Passkey(authenticator.passkey().code()));
}

#[test]
fn a_key_step_for_another_cluster_is_refused() {
    let mut other = removal(by_device());
    other.cluster = SealingPublicKey::from_bytes([6; 32]);
    let alone = Signed::sign(&other, &device()).unwrap();
    assert_eq!(
        verify(&alone, &facts(&trusted(), ISSUED)).unwrap_err(),
        AuthorityRefusal::Mismatch { field: "a Cluster" }
    );
}
