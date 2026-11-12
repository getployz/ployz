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
    let (entry, _, founder, directory, server) = participating().await;
    let late = entry
        .register(request("late", WireGuardPublicKey([4; 32])))
        .await
        .unwrap()
        .assigned_machine;
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
    let (entry, _, founder, directory, server) = participating().await;
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
