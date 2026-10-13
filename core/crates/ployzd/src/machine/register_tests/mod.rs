use ployz_core::{LocalMachinePhase, MachineId, RegisterRequest, RpcErrorCode, WireGuardPublicKey};

use super::{LocalMachine, LocalMachineError, LocalMachineStore, RecordOwner};
use crate::corrosion::{AdminClient, fake_cluster};

mod harness;
mod policy;
use harness::*;

#[tokio::test]
async fn register_rejects_an_uninitialized_machine() {
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-register-errors-{}",
        ployz_core::MachineId::random()
    ));
    let local =
        LocalMachine::new(RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap());
    let empty = local
        .register(RegisterRequest {
            machine_id: MachineId::random(),
            assigned_subnet: None,
            advertised_endpoints: Vec::new(),
            ..request("peer", WireGuardPublicKey([1; 32]))
        })
        .await
        .unwrap_err();
    assert!(matches!(empty, LocalMachineError::NotParticipating));
    let uninitialized = local
        .register(request("peer", WireGuardPublicKey([1; 32])))
        .await
        .unwrap_err();
    assert!(matches!(uninitialized, LocalMachineError::NotParticipating));
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn register_rpc_exact_replay_returns_the_original_joinable_assignment() {
    let (data_dir, store, founder) = open_store("ployzd-register-replay");
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    let service = machine_service(store, replicated.clone(), None);

    let target_dir = std::env::temp_dir().join(format!(
        "ployzd-register-replay-target-{}",
        MachineId::random()
    ));
    let target_store = RecordOwner::spawn(LocalMachineStore::open(&target_dir).unwrap()).unwrap();
    let public_key = target_store.record().wireguard_private_key.public_key();
    let mut identity = request("joiner", public_key);
    identity.machine_id = target_store.record().id();

    let first = rpc_register(&service, identity.clone()).await.unwrap();
    let replay = rpc_register(&service, identity).await.unwrap();

    assert_eq!(replay.assigned_machine.id, first.assigned_machine.id);
    assert_eq!(
        replay.assigned_machine.subnet,
        first.assigned_machine.subnet
    );
    assert_eq!(replicated.machines().await.unwrap().observations.len(), 2);

    let target = LocalMachine::new(target_store);
    target
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Member {
                registration: replay,
            },
            wireguard_mtu: None,
        })
        .await
        .unwrap();
    assert_eq!(target.record().phase(), LocalMachinePhase::Joining);

    server.abort();
    drop(service);
    drop(target);
    let _ = std::fs::remove_dir_all(data_dir);
    let _ = std::fs::remove_dir_all(target_dir);
}

#[tokio::test]
async fn register_does_not_reconstruct_membership_while_joining() {
    let (entry, replicated, _founder, data_dir, server) = participating().await;
    let joiner_dir =
        std::env::temp_dir().join(format!("ployzd-register-joining-{}", MachineId::random()));
    let joiner_store = RecordOwner::spawn(LocalMachineStore::open(&joiner_dir).unwrap()).unwrap();
    let public_key = joiner_store.record().wireguard_private_key.public_key();
    let mut identity = request("peer", public_key);
    identity.machine_id = joiner_store.record().id();
    let registered = entry.register(identity).await.unwrap();
    let joiner = LocalMachine::new(joiner_store).with_cluster(Some((
        replicated.clone(),
        AdminClient::new("/no/such/ployz-admin.sock"),
    )));
    joiner
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Member {
                registration: registered,
            },
            wireguard_mtu: None,
        })
        .await
        .unwrap();
    assert_eq!(joiner.record().phase(), LocalMachinePhase::Joining);

    let error = joiner
        .register(request("peer", public_key))
        .await
        .unwrap_err();
    assert!(matches!(error, LocalMachineError::NotParticipating));

    server.abort();
    drop(entry);
    drop(joiner);
    let _ = std::fs::remove_dir_all(data_dir);
    let _ = std::fs::remove_dir_all(joiner_dir);
}

#[tokio::test]
async fn client_assignment_publishes_through_entry_and_a_changed_register_overwrites_it() {
    let (local, replicated, founder, data_dir, server) = participating().await;
    let id = MachineId::random();
    let mut identity = request("edge", WireGuardPublicKey([21; 32]));
    identity.machine_id = id;
    identity.assigned_subnet = Some("10.210.1.0/24".parse().unwrap());
    let first = local.register(identity.clone()).await.unwrap();
    assert_eq!(first.assigned_machine.id, id);
    assert_eq!(first.visible_peers, vec![founder]);
    assert_eq!(
        replicated.machine(id.as_str()).await.unwrap(),
        Some(first.assigned_machine.clone())
    );
    assert_eq!(local.register(identity.clone()).await.unwrap(), first);
    let mut rekeyed = identity;
    rekeyed.public_key = WireGuardPublicKey([22; 32]);
    let overwritten = local.register(rekeyed).await.unwrap().assigned_machine;
    assert_eq!(overwritten.public_key, WireGuardPublicKey([22; 32]));
    assert_eq!(
        replicated.machine(id.as_str()).await.unwrap(),
        Some(overwritten)
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn register_requires_a_valid_assignment_and_preserves_the_row_on_refusal() {
    let (data_dir, store, founder) = open_store("ployzd-register-publication");
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    let service = machine_service(store, replicated.clone(), None);
    let mut identity = request("peer", WireGuardPublicKey([1; 32]));
    identity.assigned_subnet = None;
    assert_eq!(
        rpc_register(&service, identity.clone())
            .await
            .unwrap_err()
            .code,
        RpcErrorCode::InvalidArgument
    );
    assert_eq!(
        replicated.machines().await.unwrap().observations,
        vec![founder]
    );
    identity.assigned_subnet = Some("10.210.1.0/24".parse().unwrap());
    let first = rpc_register(&service, identity.clone()).await.unwrap();
    for change in 0..2 {
        let mut invalid = identity.clone();
        match change {
            0 => invalid.advertised_endpoints.clear(),
            _ => invalid.assigned_subnet = Some("10.211.1.0/24".parse().unwrap()),
        }
        assert!(rpc_register(&service, invalid).await.is_err());
        assert_eq!(
            replicated
                .machine(identity.machine_id.as_str())
                .await
                .unwrap(),
            Some(first.assigned_machine.clone())
        );
    }
    server.abort();
    drop(service);
    let _ = std::fs::remove_dir_all(data_dir);
}
