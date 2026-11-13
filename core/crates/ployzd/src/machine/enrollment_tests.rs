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

#[test]
fn a_joiner_trusts_the_passkeys_in_its_verified_approval_and_none_from_elsewhere() {
    use ployz_core::authority::{PasskeyApprover, TrustedPasskey};
    let (_dir, mut store) = fresh();
    let original = store.record().clone();
    let cluster = ApprovingCluster::new(1);
    let forger = ApprovingCluster::new(9);
    let approved = |cluster: &ApprovingCluster, passkeys| {
        let mut approval = cluster.approval(&original);
        approval.passkeys = passkeys;
        approval
    };

    let forged = forger.registration_with(&original, Some(&approved(&forger, vec![passkey(8)])));
    assert!(
        store
            .enroll(member(forged, Some(cluster.pin())))
            .is_err_and(|error| matches!(
                error,
                StoreError::JoinRefused(AuthorityRefusal::UnknownKey { .. })
            ))
    );
    assert!(store.record().passkeys().is_empty());

    let genuine = cluster.registration_with(
        &original,
        Some(&approved(&cluster, vec![passkey(5), passkey(6)])),
    );
    assert_eq!(
        store.enroll(member(genuine, Some(cluster.pin()))).unwrap(),
        EnrollPlan::Fresh
    );

    let device = cluster.device().code();
    assert_eq!(
        store.record().passkeys(),
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
