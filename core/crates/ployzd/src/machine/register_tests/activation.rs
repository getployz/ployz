//! Activation as a Machine sees it: which sets and keys it admits from where, what it holds
//! as legacy, and the recipient key that follows.

use ployz_core::{
    ActivationState, DeviceKey, OperationId, PlatformPurpose, RecipientKeySet, ResealValue,
    ResealValuesRequest, SetDeviceEnforcementRequest, SetDeviceRequest, SetRecipientKeyRequest,
    authority::{Member, SealingRotation, Signed, Signer, SignerKey, StartingSet, Timestamp},
};

use super::*;
use crate::machine::local_machine::activation::Refusal;

fn starting_set(signer: &Signer, members: Vec<Member>) -> Signed<StartingSet> {
    let issued_at = Timestamp::now();
    let set = StartingSet {
        operation: OperationId::random(),
        members,
        issued_at,
        expires_at: issued_at.plus(60_000),
        signer: SignerKey::Device(signer.public().into()),
        kept_keys: Vec::new(),
    };
    Signed::sign(&set, signer).unwrap()
}

fn enforce(set: Signed<StartingSet>) -> SetDeviceEnforcementRequest {
    SetDeviceEnforcementRequest {
        enforced: true,
        starting_set: Some(set),
    }
}

async fn approve(entry: &LocalMachine, signer: &Signer) {
    entry
        .set_device(SetDeviceRequest::Approve {
            key: signer.public().into(),
        })
        .await
        .unwrap();
}

fn refusal(result: Result<ployz_core::DeviceEnforcementSet, LocalMachineError>) -> Refusal {
    match result {
        Err(LocalMachineError::ActivationRefused { reason, .. }) => reason,
        other => panic!("not an activation refusal: {other:?}"),
    }
}

/// A record from before this daemon's first start: a harness Member that joined with
/// enforcement off, a key added from the mesh, and even one added at the console, all
/// before the snapshot existed.
#[tokio::test]
async fn a_member_from_before_the_first_f6_start_is_legacy() {
    let (replicated, server) = fake_cluster::store().await;
    let (directory, mut store, founder) = founded_store("ployzd-before-f6");
    let mesh_key = DeviceKey::from(Signer::from_seed(&[3; 32]).public());
    let console_key = DeviceKey::from(Signer::from_seed(&[1; 32]).public());
    store.approve_device(mesh_key, None).unwrap();
    store
        .approve_device(console_key, Some(crate::machine::Evidence::LocalSocket))
        .unwrap();
    drop(store);
    let record = directory.join("machine.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    json.as_object_mut().unwrap().remove("trusted_snapshot");
    json.as_object_mut().unwrap().remove("born_enforcing");
    json.as_object_mut()
        .unwrap()
        .insert("device_enforcement".into(), false.into());
    std::fs::write(&record, serde_json::to_vec(&json).unwrap()).unwrap();
    let mut harness = founder.clone();
    harness.id = MachineId::random();
    harness.name = ployz_core::MachineName::parse("harness").unwrap();
    harness.public_key = WireGuardPublicKey([4; 32]);
    harness.subnet = "10.210.4.0/24".parse().unwrap();
    replicated.publish_local_machine(&founder).await.unwrap();
    replicated.publish_local_machine(&harness).await.unwrap();

    let local = LocalMachine::new(
        RecordOwner::spawn(LocalMachineStore::open(&directory).unwrap()).unwrap(),
    )
    .with_cluster(Some((
        replicated,
        AdminClient::new("/no/such/ployz-admin.sock"),
    )));
    let report = local.activation_report().await;

    assert_eq!(report.state, ActivationState::Awaiting);
    assert_eq!(report.legacy_members, vec![harness.id]);
    let legacy: Vec<_> = report.legacy_keys.iter().map(|key| key.code).collect();
    assert!(legacy.contains(&mesh_key.code()));
    assert!(legacy.contains(&console_key.code()));
    let snapshot = local.record().trusted_snapshot().cloned().unwrap();
    assert!(snapshot.keys().is_empty());
    assert!(snapshot.members().is_empty());
    server.abort();
    drop(local);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_starting_set_outside_the_snapshot_is_refused() {
    let (entry, raw, founder, directory, server) = participating_awaiting().await;
    let late = joined_before_activation(&raw, &founder).await;
    let mesh_key = Signer::from_seed(&[3; 32]);
    approve(&entry, &mesh_key).await;
    let console_key = Signer::from_seed(&[1; 32]);
    approve(&entry.clone().on_local_socket(), &console_key).await;
    let mesh_code = DeviceKey::from(mesh_key.public()).code();

    let by_mesh_key = entry
        .set_device_enforcement(enforce(starting_set(&mesh_key, vec![(&founder).into()])))
        .await;
    assert!(matches!(refusal(by_mesh_key), Refusal::LegacyKey(code) if code == mesh_code));
    let with_late = entry
        .set_device_enforcement(enforce(starting_set(
            &console_key,
            vec![(&founder).into(), (&late).into()],
        )))
        .await;
    assert!(matches!(refusal(with_late), Refusal::LegacyMember(id) if id == late.id));
    assert_eq!(
        entry.activation_report().await.state,
        ActivationState::Awaiting
    );

    entry
        .set_device_enforcement(enforce(starting_set(&console_key, vec![(&founder).into()])))
        .await
        .unwrap();

    assert_eq!(
        entry.activation_report().await.state,
        ActivationState::Activating
    );
    entry.commit_activation_as_if_restarted().await;
    let record = entry.record();
    assert!(!record.devices().any(|key| key.code() == mesh_code));
    assert!(!record.accepts_machine(&late));
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// A set signed by a snapshot key but sent over the mesh, as Cloud or the dashboard sends
/// one, can't keep a key nobody verified: Cloud planted it, and keeping it would give it
/// Full scope after activation.
#[tokio::test]
async fn a_set_from_the_mesh_keeps_no_key_outside_the_snapshot() {
    let (entry, _, founder, directory, server) = participating_awaiting().await;
    let planted = Signer::from_seed(&[3; 32]);
    approve(&entry, &planted).await;
    let console_key = Signer::from_seed(&[1; 32]);
    approve(&entry.clone().on_local_socket(), &console_key).await;
    let planted_code = DeviceKey::from(planted.public()).code();
    let keeping = |kept_keys| {
        let issued_at = Timestamp::now();
        let set = StartingSet {
            operation: OperationId::random(),
            members: vec![(&founder).into()],
            issued_at,
            expires_at: issued_at.plus(60_000),
            signer: SignerKey::Device(console_key.public().into()),
            kept_keys,
        };
        Signed::sign(&set, &console_key).unwrap()
    };

    let over_mesh = entry
        .set_device_enforcement(enforce(keeping(vec![planted_code])))
        .await;

    assert!(matches!(refusal(over_mesh), Refusal::LegacyKey(code) if code == planted_code));
    assert_eq!(
        entry.activation_report().await.state,
        ActivationState::Awaiting
    );
    entry
        .clone()
        .on_local_socket()
        .set_device_enforcement(enforce(keeping(vec![planted_code])))
        .await
        .unwrap();
    entry.commit_activation_as_if_restarted().await;
    assert!(
        entry
            .record()
            .devices()
            .any(|key| key.code() == planted_code),
        "at the console a person may keep a key they checked"
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// A participating founder holding a Sealing Key, activated at its console by
/// `console_key` with `retired` approved from the mesh and so retired at the commit.
async fn activated(
    console_key: &Signer,
    retired: &Signer,
) -> (
    LocalMachine,
    ployz_core::Machine,
    OperationId,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    let (entry, _, founder, directory, server) = participating_awaiting().await;
    if SealingKey::load(&directory).unwrap().is_none() {
        SealingKey::set(&directory, Some(&SealingKey::generate())).unwrap();
    }
    approve(&entry, retired).await;
    let console = entry.clone().on_local_socket();
    approve(&console, console_key).await;
    let set = starting_set(console_key, vec![(&founder).into()]);
    let operation = set.open().unwrap().operation;
    console.set_device_enforcement(enforce(set)).await.unwrap();
    entry.commit_activation_as_if_restarted().await;
    (entry, founder, operation, directory, server)
}

fn rotation(
    signer: &Signer,
    cluster: ployz_core::SealingPublicKey,
    operation: OperationId,
    recipient: ployz_core::SealingPublicKey,
    members: Vec<Member>,
) -> Signed<SealingRotation> {
    let body = SealingRotation {
        cluster,
        operation,
        recipient,
        members,
        signer: SignerKey::Device(signer.public().into()),
    };
    Signed::sign(&body, signer).unwrap()
}

#[tokio::test]
async fn a_machine_installs_only_the_recipient_a_verified_rotation_names() {
    let console_key = Signer::from_seed(&[1; 32]);
    let retired = Signer::from_seed(&[3; 32]);
    let (entry, founder, operation, directory, server) = activated(&console_key, &retired).await;
    let identity = SealingKey::load(&directory).unwrap().unwrap().public_key();
    let RecipientKeySet::Generated { recipient } = entry
        .set_recipient_key(SetRecipientKeyRequest::Generate { operation })
        .await
        .unwrap()
    else {
        panic!("not generated");
    };
    let install = |rotation| SetRecipientKeyRequest::Install {
        rotation,
        sealed: None,
    };
    let members = vec![Member::from(&founder)];

    let by_retired = rotation(&retired, identity, operation, recipient, members.clone());
    assert!(matches!(
        entry.set_recipient_key(install(by_retired)).await,
        Err(LocalMachineError::RecipientRefused { .. })
    ));
    let other = SealingKey::generate().public_key();
    let elsewhere = rotation(&console_key, identity, operation, other, members.clone());
    assert!(matches!(
        entry.set_recipient_key(install(elsewhere)).await,
        Err(LocalMachineError::RecipientRefused { .. })
    ));
    let another = rotation(
        &console_key,
        identity,
        OperationId::random(),
        recipient,
        members.clone(),
    );
    assert!(matches!(
        entry.set_recipient_key(install(another)).await,
        Err(LocalMachineError::RecipientRefused { .. })
    ));
    assert_eq!(
        entry.held_recipient(ployz_core::LocalMachinePhase::Participating),
        None
    );

    let named = rotation(&console_key, identity, operation, recipient, members);
    entry.set_recipient_key(install(named)).await.unwrap();

    assert_eq!(
        entry.held_recipient(ployz_core::LocalMachinePhase::Participating),
        Some(recipient)
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// A rotation brings its own Member list. Cloud can have one name a WireGuard key of its
/// own beside the real Members; the Machine must neither take a key from that Member nor
/// share its key with it. Only Members its snapshot holds count.
#[tokio::test]
async fn a_rotation_naming_a_member_outside_the_snapshot_installs_and_shares_nothing() {
    let console_key = Signer::from_seed(&[1; 32]);
    let (entry, founder, operation, directory, server) =
        activated(&console_key, &Signer::from_seed(&[3; 32])).await;
    let identity = SealingKey::load(&directory).unwrap().unwrap().public_key();
    let RecipientKeySet::Generated { recipient } = entry
        .set_recipient_key(SetRecipientKeyRequest::Generate { operation })
        .await
        .unwrap()
    else {
        panic!("not generated");
    };
    let injected = Member {
        id: MachineId::parse("f".repeat(32)).unwrap(),
        public_key: crate::network::WireGuardPrivateKey::generate().public_key(),
        subnet: "10.210.250.0/24".parse().unwrap(),
    };
    let widened = rotation(
        &console_key,
        identity,
        operation,
        recipient,
        vec![(&founder).into(), injected.clone()],
    );

    let installed = entry
        .set_recipient_key(SetRecipientKeyRequest::Install {
            rotation: widened.clone(),
            sealed: None,
        })
        .await;
    assert!(
        matches!(installed, Err(LocalMachineError::RecipientRefused { .. })),
        "{installed:?}"
    );
    entry
        .set_recipient_key(SetRecipientKeyRequest::Install {
            rotation: rotation(
                &console_key,
                identity,
                operation,
                recipient,
                vec![(&founder).into()],
            ),
            sealed: None,
        })
        .await
        .unwrap();
    let shared = entry
        .set_recipient_key(SetRecipientKeyRequest::Share {
            rotation: widened,
            to: injected.id,
        })
        .await;
    assert!(
        matches!(shared, Err(LocalMachineError::RecipientRefused { .. })),
        "{shared:?}"
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_machine_reseals_only_to_its_installed_recipient() {
    let console_key = Signer::from_seed(&[1; 32]);
    let (entry, founder, operation, directory, server) =
        activated(&console_key, &Signer::from_seed(&[3; 32])).await;
    let identity_key = SealingKey::load(&directory).unwrap().unwrap();
    let identity = identity_key.public_key();
    let sealed = ployz_core::seal_platform(
        &identity,
        &identity,
        PlatformPurpose::PrivateAccess,
        b"tskey",
    )
    .unwrap();
    let request = || ResealValuesRequest {
        values: vec![ResealValue::Platform {
            purpose: PlatformPurpose::PrivateAccess,
            sealed: sealed.clone(),
        }],
    };
    assert!(matches!(
        entry.reseal_values(request()).await,
        Err(LocalMachineError::RecipientRefused { .. })
    ));
    let RecipientKeySet::Generated { recipient } = entry
        .set_recipient_key(SetRecipientKeyRequest::Generate { operation })
        .await
        .unwrap()
    else {
        panic!("not generated");
    };
    entry
        .set_recipient_key(SetRecipientKeyRequest::Install {
            rotation: rotation(
                &console_key,
                identity,
                operation,
                recipient,
                vec![(&founder).into()],
            ),
            sealed: None,
        })
        .await
        .unwrap();

    let resealed = entry.reseal_values(request()).await.unwrap();

    assert_eq!(resealed.recipient, recipient);
    let moved = resealed.values.first().cloned().flatten().unwrap();
    assert!(
        identity_key
            .open_platform(PlatformPurpose::PrivateAccess, &moved)
            .is_none()
    );
    let keys = crate::sealing_key::ClusterKeys::load(&directory)
        .unwrap()
        .unwrap();
    assert_eq!(
        keys.open_platform(PlatformPurpose::PrivateAccess, &moved)
            .unwrap()
            .as_slice(),
        b"tskey"
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// An activated founder holding the recipient key `console_key` named, and the operation.
async fn with_recipient(
    console_key: &Signer,
) -> (
    LocalMachine,
    ployz_core::SealingPublicKey,
    ployz_core::SealingPublicKey,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    let (entry, founder, operation, directory, server) =
        activated(console_key, &Signer::from_seed(&[3; 32])).await;
    let identity = SealingKey::load(&directory).unwrap().unwrap().public_key();
    let RecipientKeySet::Generated { recipient } = entry
        .set_recipient_key(SetRecipientKeyRequest::Generate { operation })
        .await
        .unwrap()
    else {
        panic!("not generated");
    };
    entry
        .set_recipient_key(SetRecipientKeyRequest::Install {
            rotation: rotation(
                console_key,
                identity,
                operation,
                recipient,
                vec![(&founder).into()],
            ),
            sealed: None,
        })
        .await
        .unwrap();
    (entry, identity, recipient, directory, server)
}

/// Approve `joiner`'s join through `entry` with `signer`, and the registration it answers.
async fn approved_registration(
    entry: &LocalMachine,
    signer: &Signer,
    joiner: &LocalMachineStore,
) -> ployz_core::Registered {
    let mut request = request("joiner", joiner.record().private_key().public_key());
    request.machine_id = joiner.record().id();
    assert!(entry.register(request.clone()).await.is_err());
    let mut body = entry.pending_joins().unwrap().joins.pop().unwrap();
    body.signer = signer.public().into();
    entry
        .approve_join(ployz_core::ApproveJoinRequest {
            approval: Signed::sign(&body, signer).unwrap(),
        })
        .await
        .unwrap();
    entry.register(request).await.unwrap()
}

fn as_member(registration: ployz_core::Registered) -> ployz_core::EnrollRequest {
    ployz_core::EnrollRequest {
        role: ployz_core::EnrollAs::Member {
            registration,
            pin: None,
        },
        wireguard_mtu: None,
        pinned: Default::default(),
    }
}

#[tokio::test]
async fn a_server_joining_after_activation_opens_values_sealed_after_it() {
    let console_key = Signer::from_seed(&[1; 32]);
    let (entry, identity, recipient, directory, server) = with_recipient(&console_key).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let registration = approved_registration(&entry, &console_key, &joiner).await;
    assert!(registration.recipient.is_some());

    joiner.enroll(as_member(registration)).unwrap();

    let keys = crate::sealing_key::ClusterKeys::load(join_dir.path())
        .unwrap()
        .unwrap();
    assert_eq!(keys.sealing_to().public_key(), recipient);
    let sealed = ployz_core::seal_platform(
        &identity,
        &recipient,
        PlatformPurpose::PrivateAccess,
        b"tskey",
    )
    .unwrap();
    assert_eq!(
        keys.open_platform(PlatformPurpose::PrivateAccess, &sealed)
            .unwrap()
            .as_slice(),
        b"tskey"
    );
    assert!(joiner.record().activation().is_some());
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// A compromised Cloud relays the registration, so it can swap the rotation for one its own
/// key signed, naming a recipient it holds.
#[tokio::test]
async fn a_joiner_refuses_a_forged_rotation_in_its_capsule() {
    let console_key = Signer::from_seed(&[1; 32]);
    let (entry, identity, _, directory, server) = with_recipient(&console_key).await;
    let join_dir = tempfile::tempdir().unwrap();
    let mut joiner = LocalMachineStore::open(join_dir.path()).unwrap();
    let mut registration = approved_registration(&entry, &console_key, &joiner).await;
    let real = registration.recipient.clone().unwrap();
    let operation = real.rotation.open().unwrap().operation;
    let cloud_key = Signer::from_seed(&[7; 32]);
    let cloud_recipient = ployz_core::RecipientSecret::generate();
    let joiner_key = joiner.record().private_key().public_key();
    let forged = ployz_core::JoinRecipient {
        rotation: rotation(
            &cloud_key,
            identity,
            operation,
            cloud_recipient.public(),
            Vec::new(),
        ),
        sealed: cloud_recipient
            .seal_to(
                &joiner_key,
                &ployz_core::authority::rotation::sealed_key_binding(&identity, &operation),
            )
            .unwrap(),
    };
    registration.recipient = Some(forged);

    joiner.enroll(as_member(registration)).unwrap();

    assert!(
        SealingKey::load_recipient(join_dir.path())
            .unwrap()
            .is_none()
    );
    assert!(joiner.record().activation().is_none());
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_lost_recipient_key_comes_back_only_under_its_code_and_a_trusted_rotation() {
    use crate::machine::local_machine::RecoveryError;
    use ployz_core::{
        RecoverSealingKeyRequest, RecoveryCode, SealingKeyRole, WrapSealingKeyRequest,
    };

    let console_key = Signer::from_seed(&[1; 32]);
    let (entry, identity, recipient, directory, server) = with_recipient(&console_key).await;
    let code = RecoveryCode::generate();
    let wrapped = entry
        .wrap_sealing_key(
            WrapSealingKeyRequest {
                code: code.clone(),
                key: SealingKeyRole::Recipient,
            },
            false,
        )
        .unwrap();
    assert_eq!(wrapped.public_key, recipient);
    let named = wrapped.rotation.clone().unwrap();
    std::fs::remove_file(directory.join("recipient.key")).unwrap();
    let recover = |code: &RecoveryCode, rotation| RecoverSealingKeyRequest {
        code: code.clone(),
        backup: Some(wrapped.backup.clone()),
        rotation: Some(rotation),
    };

    assert!(matches!(
        entry
            .recover_sealing_key(recover(&RecoveryCode::generate(), named.clone()), false)
            .await,
        Err(LocalMachineError::Recovery(RecoveryError::WrongCode))
    ));
    let untrusted = rotation(
        &Signer::from_seed(&[9; 32]),
        identity,
        named.open().unwrap().operation,
        recipient,
        Vec::new(),
    );
    assert!(matches!(
        entry
            .recover_sealing_key(recover(&code, untrusted), false)
            .await,
        Err(LocalMachineError::Recovery(RecoveryError::RotationRefused(
            _
        )))
    ));
    assert!(matches!(
        entry
            .recover_sealing_key(recover(&code, named.clone()), true)
            .await,
        Err(LocalMachineError::Recovery(RecoveryError::CloudRouted))
    ));
    assert_eq!(
        entry.held_recipient(ployz_core::LocalMachinePhase::Participating),
        None
    );

    let recovered = entry
        .recover_sealing_key(recover(&code, named), false)
        .await
        .unwrap();

    assert_eq!(recovered.public_key, recipient);
    assert_eq!(
        entry.held_recipient(ployz_core::LocalMachinePhase::Participating),
        Some(recipient)
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}

/// A Machine whose record predates enforcement has no keys anyone vouched for, so until its
/// Cluster activates it admits no joiner and hands its Sealing Key to no Member, even under
/// an approval its console trusts.
#[tokio::test]
async fn an_awaiting_machine_refuses_register_and_share_sealing_key() {
    let (entry, raw, founder, directory, server) = participating_awaiting().await;
    assert!(entry.record().awaits_activation());

    let register = entry
        .register(request("joiner", WireGuardPublicKey([9; 32])))
        .await
        .unwrap_err();
    assert!(
        matches!(
            register,
            LocalMachineError::AwaitingActivation {
                capability: ployz_core::REGISTER_MACHINE_CAPABILITY
            }
        ),
        "{register:?}"
    );

    let late = joined_before_activation(&raw, &founder).await;
    let share = entry
        .share_sealing_key(ployz_core::ShareSealingKeyRequest {
            join_approval: Some(approve_recipient(&entry, &late).await),
            machine: late.id,
            public_key: late.public_key,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(
            share,
            LocalMachineError::AwaitingActivation {
                capability: ployz_core::SHARE_SEALING_KEY_CAPABILITY
            }
        ),
        "{share:?}"
    );
    server.abort();
    drop(entry);
    let _ = std::fs::remove_dir_all(directory);
}

/// A Server that enforced from its first start reports itself activated and refuses a
/// starting set, so running `ployz device enforce` against it can't drop the Devices its
/// joins brought.
#[tokio::test]
async fn a_server_born_enforcing_refuses_activation() {
    let (entry, _replicated, founder, directory, server) = participating().await;
    let console_key = Signer::from_seed(&[1; 32]);
    approve(&entry.clone().on_local_socket(), &console_key).await;

    let report = entry.activation_report().await;
    assert_eq!(report.state, ActivationState::Activated);
    assert_eq!(report.operation, None);
    assert!(report.legacy_keys.is_empty());
    let refused = entry
        .clone()
        .on_local_socket()
        .set_device_enforcement(enforce(starting_set(&console_key, vec![(&founder).into()])))
        .await;
    assert!(matches!(refusal(refused), Refusal::BornEnforcing));
    assert!(
        entry
            .record()
            .devices()
            .any(|key| key == DeviceKey::from(console_key.public()))
    );
    server.abort();
    drop(entry);
    std::fs::remove_dir_all(directory).unwrap();
}
