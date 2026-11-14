//! A Member enrolls only under an approved capsule, and a record from before that rule
//! awaits its Cluster's activation.

use ployz_core::{
    DeviceKey, Enrolled, LocalMachinePhase, MachineId, ManagementScope, Registered,
    authority::{AuthorityRefusal, Purpose},
};

use super::test_support::{ApprovingCluster, member, passkey, written_before_f7};
use super::{EnrollPlan, LocalMachine, LocalMachineStore, RecordOwner, StoreError};
use crate::network::WireGuardPrivateKey;

fn fresh() -> (tempfile::TempDir, LocalMachineStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalMachineStore::open(dir.path()).unwrap();
    (dir, store)
}

fn refused_unsigned(result: Result<EnrollPlan, StoreError>) -> bool {
    matches!(
        result,
        Err(StoreError::JoinRefused(AuthorityRefusal::Unsigned {
            purpose: Purpose::Join
        }))
    )
}

#[test]
fn a_fresh_daemon_refuses_a_join_without_an_approval() {
    let (dir, mut store) = fresh();
    let original = store.record().clone();
    let cluster = ApprovingCluster::new(1);
    let clouds_device = DeviceKey::from_bytes([7; 32]);
    let registration = Registered {
        sealing_key: None,
        devices: vec![clouds_device],
        ..cluster.registration(&original)
    };

    let refused = store.enroll(member(registration, None));

    assert!(refused_unsigned(refused));
    assert_eq!(store.record(), &original);
    drop(store);
    let reopened = LocalMachineStore::open(dir.path()).unwrap();
    let record = reopened.record();
    assert_eq!(record, &original);
    assert_eq!(record.phase(), LocalMachinePhase::Uninitialized);
    assert_eq!(record.management_scope(&clouds_device.into()), None);
    assert_eq!(record.devices().count(), 0);
}

#[test]
fn a_fresh_daemon_refuses_a_capsule_with_no_approval() {
    let (_dir, mut store) = fresh();
    let original = store.record().clone();
    let cluster = ApprovingCluster::new(1);

    let refused = store.enroll(member(cluster.registration_with(&original, None), None));

    assert!(refused_unsigned(refused));
    assert_eq!(store.record(), &original);
}

#[test]
fn a_pinned_join_refuses_a_forged_approval() {
    let (_dir, mut store) = fresh();
    let original = store.record().clone();
    let cluster = ApprovingCluster::new(1);
    let forger = ApprovingCluster::new(9);
    let forged = forger.registration(&original);

    let refused = store.enroll(member(forged.clone(), Some(cluster.pin())));

    assert!(
        matches!(
            refused,
            Err(StoreError::JoinRefused(AuthorityRefusal::UnknownKey { .. }))
        ),
        "{refused:?}"
    );
    assert_eq!(store.record(), &original);

    // The install line's residual: unpinned, a capsule the forger sealed under its own key
    // and Device opens, and joins the forger's Cluster, not this one.
    assert_eq!(
        store.enroll(member(forged, None)).unwrap(),
        EnrollPlan::Fresh
    );
    assert_eq!(
        store.record().devices().collect::<Vec<_>>(),
        [forger.device()]
    );
    assert_eq!(
        store.record().membership_cluster,
        Some(forger.sealing_key.public_key())
    );
}

#[test]
fn a_pinned_join_under_the_clusters_device_enrolls_trusting_only_the_approvals_devices() {
    let (dir, mut store) = fresh();
    let cluster = ApprovingCluster::new(1);
    let clouds_device = DeviceKey::from_bytes([7; 32]);
    let registration = Registered {
        devices: vec![clouds_device],
        ..cluster.registration(store.record())
    };

    assert_eq!(
        store
            .enroll(member(registration, Some(cluster.pin())))
            .unwrap(),
        EnrollPlan::Fresh
    );

    drop(store);
    let reopened = LocalMachineStore::open(dir.path()).unwrap();
    let record = reopened.record();
    assert_eq!(record.phase(), LocalMachinePhase::Joining);
    assert_eq!(record.bootstrap(), [cluster.admitting.clone()].as_slice());
    assert_eq!(record.min_store_version().get("actor"), Some(&4));
    assert_eq!(record.devices().collect::<Vec<_>>(), [cluster.device()]);
    assert_eq!(
        record.management_scope(&cluster.device().into()),
        Some(ManagementScope::Full)
    );
    assert_eq!(record.management_scope(&clouds_device.into()), None);
    assert!(record.filters_peers());
    assert!(!record.awaits_activation());
}

/// Cloud relays the body a person signs, so it can name a Device of its own beside theirs.
/// The joiner installs only Devices both the pin and the signed body name.
#[test]
fn a_pinned_join_trusts_no_device_the_pin_leaves_out_even_when_the_approval_names_it() {
    let (dir, mut store) = fresh();
    let cluster = ApprovingCluster::new(1);
    let clouds_device = DeviceKey::from_bytes([7; 32]);
    let unsigned_pin = DeviceKey::from_bytes([8; 32]);
    let mut approval = cluster.approval(store.record());
    approval.devices.push(clouds_device);
    let registration = cluster.registration_with(store.record(), Some(&approval));
    let mut pin = cluster.pin();
    pin.devices.push(unsigned_pin);

    assert_eq!(
        store.enroll(member(registration, Some(pin))).unwrap(),
        EnrollPlan::Fresh
    );

    drop(store);
    let reopened = LocalMachineStore::open(dir.path()).unwrap();
    let record = reopened.record();
    assert_eq!(record.devices().collect::<Vec<_>>(), [cluster.device()]);
    assert_eq!(record.management_scope(&clouds_device.into()), None);
    assert_eq!(record.management_scope(&unsigned_pin.into()), None);
}

/// A Device Cloud named in the body could otherwise sign the recipient rotation, so the
/// joiner would seal new values to a key Cloud holds.
#[test]
fn a_pinned_joiner_takes_no_recipient_key_from_a_device_the_pin_leaves_out() {
    use ployz_core::authority::{SealingRotation, Signed, Signer, SignerKey};
    let (dir, mut store) = fresh();
    let cluster = ApprovingCluster::new(1);
    let cloud = Signer::from_seed(&[7; 32]);
    let mut approval = cluster.approval(store.record());
    approval.devices.push(cloud.public().into());
    let mut registration = cluster.registration_with(store.record(), Some(&approval));
    let operation = ployz_core::OperationId::random();
    let clouds_recipient = ployz_core::RecipientSecret::generate();
    let rotation = SealingRotation {
        cluster: cluster.sealing_key.public_key(),
        operation,
        recipient: clouds_recipient.public(),
        members: Vec::new(),
        signer: SignerKey::Device(cloud.public().into()),
    };
    registration.recipient = Some(ployz_core::JoinRecipient {
        rotation: Signed::sign(&rotation, &cloud).unwrap(),
        sealed: clouds_recipient
            .seal_to(
                &store.record().private_key().public_key(),
                &ployz_core::authority::rotation::sealed_key_binding(
                    &cluster.sealing_key.public_key(),
                    &operation,
                ),
            )
            .unwrap(),
    });

    assert_eq!(
        store
            .enroll(member(registration, Some(cluster.pin())))
            .unwrap(),
        EnrollPlan::Fresh
    );

    assert!(
        crate::sealing_key::SealingKey::load_recipient(dir.path())
            .unwrap()
            .is_none()
    );
    assert!(store.record().activation().is_none());
}

#[test]
fn a_pinned_join_refuses_an_approval_that_leaves_out_its_own_signer() {
    let (_dir, mut store) = fresh();
    let original = store.record().clone();
    let cluster = ApprovingCluster::new(1);
    let mut approval = cluster.approval(&original);
    approval.devices = vec![DeviceKey::from_bytes([7; 32])];
    let registration = cluster.registration_with(&original, Some(&approval));

    let refused = store.enroll(member(registration, Some(cluster.pin())));

    assert!(
        matches!(
            refused,
            Err(StoreError::JoinRefused(AuthorityRefusal::Mismatch {
                field: "approved Devices"
            }))
        ),
        "{refused:?}"
    );
    assert_eq!(store.record(), &original);
}

/// The install line already runs Cloud's script, so an unpinned joiner may trust the
/// passkeys its approval carries. A pinned join must not: Cloud can relay the person a body
/// under a Sealing Key of its own, and a passkey of its own would then approve workflows here.
#[test]
fn only_an_unpinned_joiner_trusts_the_passkeys_its_approval_carries() {
    use ployz_core::authority::{PasskeyApprover, TrustedPasskey};
    let approved = |cluster: &ApprovingCluster, joiner: &super::LocalMachineRecord, passkeys| {
        let mut approval = cluster.approval(joiner);
        approval.passkeys = passkeys;
        cluster.registration_with(joiner, Some(&approval))
    };
    let cluster = ApprovingCluster::new(1);
    let clouds_cluster = ApprovingCluster::new(1);

    let (_dir, mut pinned) = fresh();
    let relayed = approved(&clouds_cluster, &pinned.record().clone(), vec![passkey(8)]);
    assert_eq!(
        pinned.enroll(member(relayed, Some(cluster.pin()))).unwrap(),
        EnrollPlan::Fresh
    );
    assert_eq!(
        pinned.record().membership_cluster,
        Some(clouds_cluster.sealing_key.public_key())
    );
    assert!(pinned.record().passkeys().is_empty());

    let (_dir, mut unpinned) = fresh();
    let genuine = approved(
        &cluster,
        &unpinned.record().clone(),
        vec![passkey(5), passkey(6)],
    );
    assert_eq!(
        unpinned.enroll(member(genuine, None)).unwrap(),
        EnrollPlan::Fresh
    );
    let device = cluster.device().code();
    assert_eq!(
        unpinned.record().passkeys(),
        [passkey(5), passkey(6)].map(|passkey| TrustedPasskey {
            passkey,
            approved_by: PasskeyApprover::Device { code: device },
        })
    );
}

#[test]
fn a_replayed_join_keeps_the_devices_it_enrolled_with() {
    let (_dir, mut store) = fresh();
    let cluster = ApprovingCluster::new(1);
    let registration = cluster.registration(store.record());
    assert_eq!(
        store.enroll(member(registration.clone(), None)).unwrap(),
        EnrollPlan::Fresh
    );

    let other_issuer = Registered {
        devices: vec![DeviceKey::from_bytes([4; 32])],
        ..registration
    };

    assert!(matches!(
        store.enroll(member(other_issuer, None)).unwrap(),
        EnrollPlan::Replay(_)
    ));
    assert_eq!(
        store.record().devices().collect::<Vec<_>>(),
        [cluster.device()]
    );
}

#[tokio::test]
async fn join_preserves_identity_rejects_wrong_inputs_and_resumes_after_lost_response() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalMachineStore::open(dir.path()).unwrap();
    let id = store.record().id();
    let registration = ApprovingCluster::new(1).registration(store.record());
    let with_mtu = |registration: Registered, wireguard_mtu| ployz_core::EnrollRequest {
        wireguard_mtu,
        ..member(registration, None)
    };
    let request = with_mtu(registration.clone(), Some(1380));
    let local = LocalMachine::new(RecordOwner::spawn(store).unwrap());
    let mut restart_observation = local.owner().restart_requested();
    for wrong_id in [true, false] {
        let mut invalid = registration.clone();
        if wrong_id {
            invalid.assigned_machine.id = MachineId::random();
        } else {
            invalid.assigned_machine.public_key = WireGuardPrivateKey::generate().public_key();
        }
        assert!(local.enroll(with_mtu(invalid, Some(1380))).await.is_err());
        assert_eq!(local.record().phase(), LocalMachinePhase::Uninitialized);
        assert_eq!(local.record().id(), id);
    }
    let enrolled = local.enroll(request.clone()).await.unwrap();
    assert!(matches!(enrolled, Enrolled::Enrolled { .. }));
    assert!(*restart_observation.borrow_and_update());
    assert_eq!(local.enroll(request.clone()).await.unwrap(), enrolled);
    assert!(!restart_observation.has_changed().unwrap());
    drop(local);
    let mut reopened = LocalMachineStore::open(dir.path()).unwrap();
    reopened.complete_catch_up().unwrap();
    let local = LocalMachine::new(RecordOwner::spawn(reopened).unwrap());
    assert_eq!(local.enroll(request).await.unwrap(), enrolled);
    assert!(matches!(
        local.enroll(with_mtu(registration, None)).await.unwrap(),
        Enrolled::Refused { .. }
    ));
    assert_eq!(local.record().id(), id);
}

#[test]
fn a_record_from_before_f7_awaits_activation() {
    for device_enforcement in [false, true] {
        let (dir, mut store) = fresh();
        let cluster = ApprovingCluster::new(1);
        store.enroll(cluster.join(store.record())).unwrap();
        assert!(!store.record().awaits_activation());
        assert!(store.record().filters_peers());
        drop(store);

        written_before_f7(dir.path(), device_enforcement);
        let reopened = LocalMachineStore::open(dir.path()).unwrap();

        assert!(
            reopened.record().awaits_activation(),
            "device_enforcement {device_enforcement}"
        );
        assert_eq!(reopened.record().filters_peers(), device_enforcement);
    }
}

#[test]
fn an_uninitialized_record_never_awaits_activation() {
    let (_dir, store) = fresh();
    assert!(!store.record().awaits_activation());
}
