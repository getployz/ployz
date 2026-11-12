use super::*;
use crate::{
    DeviceKey, MachineId, WireGuardPublicKey,
    authority::{Signer, trusted_key::TrustedKeys},
};

fn member(fill: u8) -> Member {
    Member {
        id: MachineId::parse(char::from(b'0' + fill).to_string().repeat(32)).unwrap(),
        public_key: WireGuardPublicKey([fill; 32]),
        subnet: format!("10.210.{fill}.0/24").parse().unwrap(),
    }
}

fn cluster() -> SealingPublicKey {
    SealingPublicKey::from_bytes([1; 32])
}

fn signed(signer: &Signer, cluster: SealingPublicKey) -> Signed<SealingRotation> {
    let body = SealingRotation {
        cluster,
        operation: OperationId::parse("a".repeat(32)).unwrap(),
        recipient: SealingPublicKey::from_bytes([2; 32]),
        members: vec![member(1), member(2)],
        signer: SignerKey::Device(signer.public().into()),
    };
    Signed::sign(&body, signer).unwrap()
}

fn trusting(signer: &Signer) -> TrustedKeys {
    TrustedKeys {
        devices: vec![DeviceKey::from(signer.public())],
        passkeys: Vec::new(),
    }
}

#[test]
fn a_rotation_a_trusted_device_signed_for_this_cluster_and_member_opens() {
    let signer = Signer::from_seed(&[7; 32]);

    let (body, approver) = verify(
        &signed(&signer, cluster()),
        &trusting(&signer),
        cluster(),
        &member(2),
        Timestamp::now(),
    )
    .unwrap();

    assert_eq!(body.recipient, SealingPublicKey::from_bytes([2; 32]));
    assert_eq!(approver, Approver::Device(signer.public()));
}

#[test]
fn a_rotation_signed_by_a_retired_key_is_refused() {
    let retired = Signer::from_seed(&[7; 32]);
    let kept = Signer::from_seed(&[8; 32]);

    let refused = verify(
        &signed(&retired, cluster()),
        &trusting(&kept),
        cluster(),
        &member(1),
        Timestamp::now(),
    )
    .unwrap_err();

    assert!(matches!(refused, AuthorityRefusal::UnknownKey { .. }));
}

#[test]
fn a_rotation_for_another_cluster_or_member_is_refused() {
    let signer = Signer::from_seed(&[7; 32]);
    let other = SealingPublicKey::from_bytes([9; 32]);

    let elsewhere = verify(
        &signed(&signer, other),
        &trusting(&signer),
        cluster(),
        &member(1),
        Timestamp::now(),
    )
    .unwrap_err();
    let unnamed = verify(
        &signed(&signer, cluster()),
        &trusting(&signer),
        cluster(),
        &member(3),
        Timestamp::now(),
    )
    .unwrap_err();

    assert!(matches!(elsewhere, AuthorityRefusal::Mismatch { field } if field == field::CLUSTER));
    assert!(matches!(unnamed, AuthorityRefusal::Mismatch { field } if field == "this Machine"));
}

#[test]
fn a_sealed_key_is_bound_to_one_cluster_and_activation() {
    let first = OperationId::parse("a".repeat(32)).unwrap();
    let second = OperationId::parse("b".repeat(32)).unwrap();

    assert_ne!(
        sealed_key_binding(&cluster(), &first),
        sealed_key_binding(&cluster(), &second)
    );
    assert_ne!(
        sealed_key_binding(&cluster(), &first),
        sealed_key_binding(&SealingPublicKey::from_bytes([9; 32]), &first)
    );
}
