//! The floors an admitting Machine seals beside the Sealing Key to a joiner.

use ployz_core::{FORGET_VERSIONS_CAPABILITY, SET_CLUSTER_DOORS_CAPABILITY};

use super::*;
use crate::network::WireGuardPrivateKey;

/// Register `claims` at an entry that keeps a floor for `app/api`, as Cloud does: no
/// runtime, only the joiner's claim. Returns the entry's Sealing Key, the Service it
/// forgot, its answer and the joiner's WireGuard key.
async fn cloud_register(
    claims: &[&str],
) -> (
    SealingKey,
    ployz_core::QualifiedService,
    ployz_core::Registered,
    WireGuardPrivateKey,
) {
    let (entry, _replicated, _founder, data_dir, server) = participating().await;
    let service = forget_api(&entry).await;
    let joiner = WireGuardPrivateKey::generate();
    let mut cloud = request("joiner", joiner.public_key());
    cloud.capabilities = claims.iter().map(|claim| (*claim).to_owned()).collect();
    assert!(cloud.runtime.capabilities.is_empty());
    let registered = register_approved(&entry, &cloud).await.unwrap();
    let key = SealingKey::load(&data_dir).unwrap().unwrap();
    server.abort();
    drop(entry);
    let _ = std::fs::remove_dir_all(data_dir);
    (key, service, registered, joiner)
}

/// A Server added through Cloud carries no runtime in its Register, so its claim alone
/// tells the entry it opens the floors.
#[tokio::test]
async fn a_register_that_claims_the_floors_gets_them_though_it_carries_no_runtime() {
    let (_, service, registered, joiner) =
        cloud_register(&[SET_CLUSTER_DOORS_CAPABILITY, FORGET_VERSIONS_CAPABILITY]).await;

    let sealed = registered.sealing_key.expect("the entry seals its key");
    let floors = SealingKey::open_join(&sealed, &joiner).unwrap().floors;
    assert!(floors.contains_key(&service), "{floors:?}");
}

/// A joiner that doesn't claim the floors may run an older ployzd, which refuses anything
/// after the approval, so it gets the key and approval alone.
#[tokio::test]
async fn a_register_that_does_not_claim_the_floors_gets_a_seal_an_older_joiner_opens() {
    let (key, _, registered, joiner) = cloud_register(&[SET_CLUSTER_DOORS_CAPABILITY]).await;

    let sealed = registered.sealing_key.expect("the entry seals its key");
    let (opened, approval) = SealingKey::open_join_before_floors(&sealed, &joiner).unwrap();
    assert!(opened == key);
    assert_eq!(Some(approval), registered.assigned_machine.join_approval);
}

/// A forget can reach a Server after its admitting Server read the floors it answers the
/// join with, and before the joiner participates. The joiner takes it then, so a forget
/// that lists it never leaves it able to start what was forgotten.
#[tokio::test]
async fn a_server_still_joining_takes_a_forget() {
    use ployz_core::authority::{PayloadDigest, ServiceVersion, Signed, SignerKey, Timestamp};
    use ployz_core::{EnrollAs, EnrollRequest, ForgetOutcome, ForgetVersionsRequest};
    let (entry, _replicated, _founder, data_dir, server) = participating().await;
    forget_api(&entry).await;
    let join_dir = tempfile::tempdir().unwrap();
    let joiner = LocalMachine::new(
        RecordOwner::spawn(LocalMachineStore::open(join_dir.path()).unwrap()).unwrap(),
    );
    let mut joining = request("joiner", joiner.record().private_key().public_key());
    joining.machine_id = joiner.record().id();
    joining.capabilities = vec![
        SET_CLUSTER_DOORS_CAPABILITY.to_owned(),
        FORGET_VERSIONS_CAPABILITY.to_owned(),
        ployz_core::JOIN_TRUST_CAPABILITY.to_owned(),
    ];
    let registration = register_approved(&entry, &joining).await.unwrap();
    Box::pin(joiner.enroll(EnrollRequest {
        role: EnrollAs::Member {
            pin: crate::machine::test_support::pin_of(&registration),
            registration,
        },
        wireguard_mtu: None,
        pinned: Default::default(),
    }))
    .await
    .unwrap();
    assert_eq!(joiner.record().phase(), LocalMachinePhase::Joining);

    let signer = join_signer();
    let later = ServiceVersion {
        forget_older: true,
        cluster: SealingKey::load(&data_dir).unwrap().unwrap().public_key(),
        namespace: ployz_core::Namespace::parse("app").unwrap(),
        service: ployz_core::ServiceName::parse("api").unwrap(),
        digest: PayloadDigest::from_bytes([2; 32]),
        signed_at: Timestamp::now().plus(1_000),
        signer: SignerKey::Device(signer.public().into()),
        proposal: None,
    };
    let forgot = joiner
        .forget_versions(ForgetVersionsRequest {
            versions: vec![Signed::sign(&later, &signer).unwrap()],
        })
        .await
        .unwrap();
    assert!(
        matches!(forgot.outcomes[..], [ForgetOutcome::Raised { .. }]),
        "{forgot:?}"
    );
    server.abort();
    drop(entry);
    let _ = std::fs::remove_dir_all(data_dir);
}
