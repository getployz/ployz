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

/// A Machine's WireGuard keypair: an X25519 key, as a recipient key is.
fn wireguard(member_id: u8) -> (crate::RecipientSecret, Member) {
    let private = crate::RecipientSecret::generate();
    let member = Member {
        public_key: WireGuardPublicKey(*private.public().as_bytes()),
        ..member(member_id)
    };
    (private, member)
}

fn rotating(
    recipient: SealingPublicKey,
    members: Vec<Member>,
    signer: SignerKey,
) -> SealingRotation {
    SealingRotation {
        cluster: cluster(),
        operation: OperationId::parse("a".repeat(32)).unwrap(),
        recipient,
        members,
        signer,
    }
}

const PASSKEY: SignerKey = SignerKey::Passkey(crate::authority::restart::OwnPrompt::Passkey);

/// Cloud serves the page a passkey answers, so it can have the passkey name a key of its
/// own and seal that key to each Server's public WireGuard key. No Server may take it.
#[test]
fn under_a_passkey_a_key_only_a_member_sealed_installs() {
    let (own, me) = wireguard(1);
    let (generator, holder) = wireguard(2);
    let (outsider, _) = wireguard(3);
    let planted = crate::RecipientSecret::generate();
    let body = rotating(planted.public(), vec![me.clone(), holder], PASSKEY);
    let binding = sealed_key_binding(&body.cluster, &body.operation);

    let by_anyone = planted.seal_to(&me.public_key, &binding).unwrap();
    let by_outsider = planted
        .seal_from(outsider.secret(), &me.public_key, &binding)
        .unwrap();
    let by_member = planted
        .seal_from(generator.secret(), &me.public_key, &binding)
        .unwrap();

    assert!(
        crate::RecipientSecret::open(own.secret(), &binding, &by_anyone).is_some(),
        "an unauthenticated blob opens, so the rotation's signer must vouch for it"
    );
    assert!(opened_key(&body, own.secret(), &by_anyone).is_none());
    assert!(opened_key(&body, own.secret(), &by_outsider).is_none());
    assert_eq!(
        opened_key(&body, own.secret(), &by_member).map(|key| key.public()),
        Some(planted.public())
    );
}

#[test]
fn under_a_device_the_key_it_sealed_installs_and_no_other() {
    let (own, me) = wireguard(1);
    let device = Signer::from_seed(&[7; 32]);
    let chosen = crate::RecipientSecret::generate();
    let other = crate::RecipientSecret::generate();
    let body = rotating(
        chosen.public(),
        vec![me.clone()],
        SignerKey::Device(device.public().into()),
    );
    let binding = sealed_key_binding(&body.cluster, &body.operation);

    let sealed = chosen.seal_to(&me.public_key, &binding).unwrap();
    let swapped = other.seal_to(&me.public_key, &binding).unwrap();

    assert_eq!(
        opened_key(&body, own.secret(), &sealed).map(|key| key.public()),
        Some(chosen.public())
    );
    assert!(opened_key(&body, own.secret(), &swapped).is_none());
}
