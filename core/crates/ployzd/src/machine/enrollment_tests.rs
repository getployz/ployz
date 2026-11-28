//! A Member enrolls only under an approval its pin or First Key signed, trusts only what its
//! admitting Server sealed, and participates only once that Server's restrictions arrive.

use ployz_core::{
    DeviceKey, Enrolled, JoinRestrictionSet, LocalMachinePhase, MachineId, ManagementScope,
    Registered,
    authority::{AuthorityRefusal, KeyRef, Purpose, Signed},
    first_key::{FirstKey, FirstKeyFile, LineKey},
};

use super::test_support::{ApprovingCluster, member, passkey, trust_of};
use super::{EnrollPlan, LocalMachine, LocalMachineStore, RecordOwner, StoreError};
use crate::network::WireGuardPrivateKey;
use crate::sealing_key::JoinTrust;

fn fresh() -> (tempfile::TempDir, LocalMachineStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalMachineStore::open(dir.path().join("data")).unwrap();
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

/// A registration whose capsule carries `cluster`'s signed approval for `joiner` beside
/// `trust`, or no trust at all, as an admitting Server from before keys.2 seals it.
fn sealed(
    cluster: &ApprovingCluster,
    joiner: &super::LocalMachineRecord,
    trust: Option<&JoinTrust>,
) -> Registered {
    let approval = Signed::sign(&cluster.approval(joiner), &cluster.signer).unwrap();
    let recipient = joiner.private_key().public_key();
    Registered {
        sealing_key: Some(
            cluster
                .sealing_key
                .seal_join(&recipient, &approval, None, trust)
                .unwrap(),
        ),
        ..cluster.registration(joiner)
    }
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

    let refused = store.enroll(member(registration, Some(cluster.pin())));

    assert!(refused_unsigned(refused));
    assert_eq!(store.record(), &original);
    drop(store);
    let reopened = LocalMachineStore::open(dir.path().join("data")).unwrap();
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

    let refused = store.enroll(member(
        cluster.registration_with(&original, None),
        Some(cluster.pin()),
    ));

    assert!(refused_unsigned(refused));
    assert_eq!(store.record(), &original);
}

/// Cloud can seal a capsule under a Cluster of its own and sign it with a Device of its own.
/// Only the request's pin or the install line's First Key says which signer to believe, and a
/// join with neither is refused rather than trusting whoever sealed it.
#[test]
fn a_join_refuses_a_cluster_neither_its_pin_nor_its_first_key_signed() {
    let (dir, mut store) = fresh();
    let original = store.record().clone();
    let cluster = ApprovingCluster::new(1);
    let forger = ApprovingCluster::new(9);
    let forged = forger.registration(&original);

    let unknown = |refused: &Result<EnrollPlan, StoreError>| {
        matches!(
            refused,
            Err(StoreError::JoinRefused(AuthorityRefusal::UnknownKey { .. }))
        )
    };
    let refused = store.enroll(member(forged.clone(), Some(cluster.pin())));
    assert!(unknown(&refused), "{refused:?}");
    let refused = store.enroll(member(forged.clone(), None));
    assert!(
        matches!(refused, Err(StoreError::JoinUnanchored)),
        "{refused:?}"
    );

    let first_key = FirstKeyFile::beside(&dir.path().join("data"));
    let line = LineKey::Device {
        key: cluster.device(),
    };
    first_key.lock().unwrap().arm(&line).unwrap();
    let refused = store.enroll(member(forged, None));
    assert!(
        matches!(&refused, Err(StoreError::NotFirstKey { expected }) if *expected == line.describe()),
        "{refused:?}"
    );
    assert_eq!(store.record(), &original);
    assert_eq!(
        first_key.lock().unwrap().read().unwrap(),
        Some(FirstKey::Waiting { key: line })
    );

    let genuine = cluster.registration(&original);
    assert_eq!(
        store.enroll(member(genuine, None)).unwrap(),
        EnrollPlan::Fresh(())
    );
    assert!(matches!(
        first_key.lock().unwrap().read().unwrap(),
        Some(FirstKey::Spent { .. })
    ));
}

/// Cloud relays the body a person signs, so its lists can name a Device or passkey of
/// Cloud's own. The joiner installs exactly what its admitting Server sealed beside it.
#[test]
fn a_join_trusts_what_its_admitting_server_sealed_and_not_the_approvals_lists() {
    use ployz_core::authority::PasskeyApprover;
    let (dir, mut store) = fresh();
    let cluster = ApprovingCluster::new(1);
    let clouds_device = DeviceKey::from_bytes([7; 32]);
    let mut approval = cluster.approval(store.record());
    let sealed_trust = JoinTrust {
        passkeys: vec![passkey(5)],
        ..trust_of(&approval)
    };
    approval.devices.push(clouds_device);
    approval.passkeys.push(passkey(8));
    let recipient = store.record().private_key().public_key();
    let registration = Registered {
        sealing_key: Some(
            cluster
                .sealing_key
                .seal_join(
                    &recipient,
                    &Signed::sign(&approval, &cluster.signer).unwrap(),
                    None,
                    Some(&sealed_trust),
                )
                .unwrap(),
        ),
        devices: vec![clouds_device],
        ..cluster.registration(store.record())
    };

    assert_eq!(
        store
            .enroll(member(registration, Some(cluster.pin())))
            .unwrap(),
        EnrollPlan::Fresh(())
    );

    drop(store);
    let reopened = LocalMachineStore::open(dir.path().join("data")).unwrap();
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
    assert_eq!(
        record
            .passkeys()
            .iter()
            .map(|trusted| (trusted.passkey.clone(), trusted.approved_by))
            .collect::<Vec<_>>(),
        [(
            passkey(5),
            PasskeyApprover::Device {
                code: cluster.device().code()
            }
        )]
    );
    assert!(record.filters_peers());
}

/// A joiner holds a CI key its admitting Server removed as a removed CI key, so `ployz key
/// rm` run again still finds it there, and never takes a grant the sender didn't remove.
#[test]
fn a_join_keeps_which_removed_keys_were_ci_or_agent_keys() {
    let (_dir, mut store) = fresh();
    let cluster = ApprovingCluster::new(1);
    let ci = ployz_core::authority::SigningPublicKey::from(DeviceKey::from_bytes([21; 32]));
    let unremoved = ployz_core::authority::SigningPublicKey::from(DeviceKey::from_bytes([22; 32]));
    let approval = cluster.approval(store.record());
    let trust = JoinTrust {
        removed_keys: vec![KeyRef::Signing { key: ci.into() }],
        removed_grants: vec![ci, unremoved],
        ..trust_of(&approval)
    };
    let registration = sealed(&cluster, store.record(), Some(&trust));

    assert_eq!(
        store
            .enroll(member(registration, Some(cluster.pin())))
            .unwrap(),
        EnrollPlan::Fresh(())
    );
    assert_eq!(store.record().removed_grants(), [ci]);
}

/// An admitting Server from before keys.2 seals no trust, and its approval's lists are
/// Cloud's to rewrite, so the joiner refuses rather than fall back to them.
#[test]
fn a_join_refuses_a_capsule_its_admitting_server_sealed_without_trust() {
    let (_dir, mut store) = fresh();
    let original = store.record().clone();
    let cluster = ApprovingCluster::new(1);

    let refused = store.enroll(member(
        sealed(&cluster, &original, None),
        Some(cluster.pin()),
    ));

    assert!(
        matches!(refused, Err(StoreError::JoinTrustMissing { .. })),
        "{refused:?}"
    );
    assert_eq!(store.record(), &original);
}

#[test]
fn a_replayed_join_keeps_the_devices_it_enrolled_with() {
    let (_dir, mut store) = fresh();
    let cluster = ApprovingCluster::new(1);
    let registration = cluster.registration(store.record());
    assert_eq!(
        store
            .enroll(member(registration.clone(), Some(cluster.pin())))
            .unwrap(),
        EnrollPlan::Fresh(())
    );

    let other_issuer = Registered {
        devices: vec![DeviceKey::from_bytes([4; 32])],
        ..registration
    };

    assert!(matches!(
        store
            .enroll(member(other_issuer, Some(cluster.pin())))
            .unwrap(),
        EnrollPlan::Replay(_)
    ));
    assert_eq!(
        store.record().devices().collect::<Vec<_>>(),
        [cluster.device()]
    );
}

/// A key removed on the admitting Server after it sealed the join reaches the joiner through
/// the restrictions it MACs on confirmation. The joiner won't participate until a set with a
/// valid tag arrives, and a set Cloud forged or widened is refused.
#[tokio::test]
async fn a_joiner_participates_only_under_its_admitting_servers_restrictions() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = LocalMachineStore::open(dir.path().join("data")).unwrap();
    let cluster = ApprovingCluster::new(1);
    let removed = DeviceKey::from_bytes([6; 32]);
    let mut approval = cluster.approval(store.record());
    approval.devices.push(removed);
    let signed = Signed::sign(&approval, &cluster.signer).unwrap();
    let recipient = store.record().private_key().public_key();
    let registration = Registered {
        sealing_key: Some(
            cluster
                .sealing_key
                .seal_join(&recipient, &signed, None, Some(&trust_of(&approval)))
                .unwrap(),
        ),
        ..cluster.registration(store.record())
    };
    assert_eq!(
        store
            .enroll(member(registration, Some(cluster.pin())))
            .unwrap(),
        EnrollPlan::Fresh(())
    );
    assert!(store.record().management_scope(&removed.into()).is_some());
    assert!(matches!(
        store.complete_catch_up(),
        Err(StoreError::JoinRestrictionsPending)
    ));
    let local = LocalMachine::new(RecordOwner::spawn(store).unwrap());

    let confirmation = cluster.sealing_key.confirm_join(&signed);
    let set = JoinRestrictionSet {
        removed_keys: vec![KeyRef::Signing { key: removed }],
        removed_members: Vec::new(),
        floors: Default::default(),
    };
    let genuine = cluster
        .sealing_key
        .restrict_join(&signed, &confirmation, set.clone());
    let widened = ployz_core::JoinRestrictions {
        set: JoinRestrictionSet {
            removed_keys: vec![KeyRef::Signing {
                key: cluster.device(),
            }],
            ..set.clone()
        },
        tag: genuine.tag,
    };
    let forger = ApprovingCluster::new(9)
        .sealing_key
        .restrict_join(&signed, &confirmation, set);
    for refused in [widened, forger] {
        assert!(
            local
                .confirm_join(ployz_core::ConfirmJoinRequest {
                    restrictions: refused
                })
                .await
                .is_err()
        );
        assert!(local.record().management_scope(&removed.into()).is_some());
    }

    local
        .confirm_join(ployz_core::ConfirmJoinRequest {
            restrictions: genuine,
        })
        .await
        .unwrap();
    let record = local.record();
    assert_eq!(record.management_scope(&removed.into()), None);
    assert_eq!(record.removed_keys(), [KeyRef::Signing { key: removed }]);
    assert_eq!(record.devices().collect::<Vec<_>>(), [cluster.device()]);
}

#[tokio::test]
async fn join_preserves_identity_rejects_wrong_inputs_and_resumes_after_lost_response() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let store = LocalMachineStore::open(&data).unwrap();
    let id = store.record().id();
    let cluster = ApprovingCluster::new(1);
    let registration = cluster.registration(store.record());
    let with_mtu = |registration: Registered, wireguard_mtu| ployz_core::EnrollRequest {
        wireguard_mtu,
        ..member(registration, Some(cluster.pin()))
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
    let mut reopened = LocalMachineStore::open(&data).unwrap();
    reopened
        .apply_join_restrictions(
            &crate::machine::KeyChangeHeld::for_test(),
            &JoinRestrictionSet {
                removed_keys: Vec::new(),
                removed_members: Vec::new(),
                floors: Default::default(),
            },
        )
        .unwrap();
    reopened.complete_catch_up().unwrap();
    let local = LocalMachine::new(RecordOwner::spawn(reopened).unwrap());
    assert_eq!(local.enroll(request).await.unwrap(), enrolled);
    assert!(matches!(
        local.enroll(with_mtu(registration, None)).await.unwrap(),
        Enrolled::Refused { .. }
    ));
    assert_eq!(local.record().id(), id);
}

/// A crash between the enrollment commit and the First Key write, or a disk enrolled before
/// keys.2, must not leave a key waiting to found or join again: the daemon spends it on start,
/// and a reset spends it before it deletes the record.
#[test]
fn an_enrolled_disk_leaves_no_first_key_waiting_after_a_crash_or_a_reset() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let mut store = LocalMachineStore::open(&data).unwrap();
    let cluster = ApprovingCluster::new(1);
    let line = LineKey::Device {
        key: cluster.device(),
    };
    let first_key = FirstKeyFile::beside(&data);
    first_key.lock().unwrap().arm(&line).unwrap();
    store
        .enroll(member(cluster.registration(store.record()), None))
        .unwrap();
    drop(store);
    let spent = || {
        matches!(
            first_key.lock().unwrap().read().unwrap(),
            Some(FirstKey::Spent { .. })
        )
    };

    first_key.lock().unwrap().rearm(&line).unwrap();
    let store = LocalMachineStore::open(&data).unwrap();
    assert!(spent());
    drop(store);
    std::fs::remove_file(first_key.path()).unwrap();
    let mut store = LocalMachineStore::open(&data).unwrap();
    assert!(spent());

    store.begin_reset().unwrap();
    first_key.lock().unwrap().rearm(&line).unwrap();
    store.complete_reset().unwrap();
    assert!(spent());
}

/// A Cloud founding trusts the key its install line armed, and with none it founds nothing:
/// otherwise Cloud alone could start a Cluster that trusts no key.
#[test]
fn a_cloud_founding_trusts_its_first_key_and_refuses_without_one() {
    let founding = || ployz_core::EnrollRequest {
        role: ployz_core::EnrollAs::Founder {
            initial_policy: Default::default(),
            name: ployz_core::MachineName::parse("first").unwrap(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            public_ip: None,
            advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                "192.0.2.1:51820".parse().unwrap(),
            )],
            grant: Some(ployz_core::FoundingGrant::Claim {
                claim: ployz_core::FoundingClaim::of_pairing("ppair_fake"),
            }),
        },
        wireguard_mtu: None,
        pinned: Default::default(),
    };
    let (dir, mut store) = fresh();
    let refused = store.enroll(founding());
    assert!(
        matches!(refused, Err(StoreError::NoFirstKey)),
        "{refused:?}"
    );
    assert_eq!(store.record().phase(), LocalMachinePhase::Uninitialized);

    let device = ApprovingCluster::new(1).device();
    FirstKeyFile::beside(&dir.path().join("data"))
        .lock()
        .unwrap()
        .arm(&LineKey::Device { key: device })
        .unwrap();
    store.enroll(founding()).unwrap();
    assert!(store.record().devices().any(|trusted| trusted == device));
}
