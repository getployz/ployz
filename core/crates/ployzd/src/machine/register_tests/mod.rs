use std::sync::{Arc, Mutex};

use ployz_core::{
    JoinRequest, LocalMachinePhase, MachineId, RegisterRequest, RpcErrorCode, WireGuardPublicKey,
};
use tokio::sync::watch;

use super::{LocalMachine, LocalMachineError, LocalMachineStore};
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
    let local = LocalMachine::new(
        Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap())),
        watch::channel(false).0,
    );
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
    let target_store = Arc::new(Mutex::new(LocalMachineStore::open(&target_dir).unwrap()));
    let public_key = target_store
        .lock()
        .unwrap()
        .record()
        .wireguard_private_key
        .public_key();
    let mut identity = request("joiner", public_key);
    identity.machine_id = target_store.lock().unwrap().record().id();

    let first = rpc_register(&service, identity.clone()).await.unwrap();
    let replay = rpc_register(&service, identity).await.unwrap();

    assert_eq!(replay.assigned_machine.id, first.assigned_machine.id);
    assert_eq!(
        replay.assigned_machine.subnet,
        first.assigned_machine.subnet
    );
    assert_eq!(replicated.machines().await.unwrap().observations.len(), 2);

    let target = LocalMachine::new(target_store, watch::channel(false).0);
    target
        .join(JoinRequest {
            registration: replay,
            wireguard_mtu: None,
            cloud_pairing: None,
        })
        .await
        .unwrap();
    assert_eq!(target.record().unwrap().phase(), LocalMachinePhase::Joining);

    let conflict = rpc_register(&service, request("other", public_key))
        .await
        .unwrap_err();
    assert_eq!(conflict.code, RpcErrorCode::Conflict);

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
    let joiner_store = Arc::new(Mutex::new(LocalMachineStore::open(&joiner_dir).unwrap()));
    let public_key = joiner_store
        .lock()
        .unwrap()
        .record()
        .wireguard_private_key
        .public_key();
    let mut identity = request("peer", public_key);
    identity.machine_id = joiner_store.lock().unwrap().record().id();
    let registered = entry.register(identity).await.unwrap();
    let joiner = LocalMachine::new(joiner_store, watch::channel(false).0).with_cluster(Some((
        replicated.clone(),
        AdminClient::new("/no/such/ployz-admin.sock"),
    )));
    joiner
        .join(JoinRequest {
            registration: registered,
            wireguard_mtu: None,
            cloud_pairing: None,
        })
        .await
        .unwrap();
    assert_eq!(joiner.record().unwrap().phase(), LocalMachinePhase::Joining);

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
async fn isolation_lock_refuses_admit_when_replica_exceeds_three_and_others_are_uncontactable() {
    let (data_dir, store, founder) = open_store("ployzd-register-isolation-admit");
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    publish_peers(&replicated, 3).await;
    let (admin_server, admin, admin_root) = serve_membership(&[]).await;
    let local = LocalMachine::new(store, watch::channel(false).0)
        .with_cluster(Some((replicated.clone(), AdminClient::new(&admin))));
    let error = local
        .register(request("joiner", WireGuardPublicKey([1; 32])))
        .await
        .unwrap_err();
    assert!(matches!(error, LocalMachineError::IsolationLocked));
    assert_eq!(replicated.machines().await.unwrap().observations.len(), 4);

    admin_server.abort();
    let _ = std::fs::remove_dir_all(admin_root);
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn isolation_lock_does_not_fire_when_a_peer_is_still_up() {
    let (data_dir, store, founder) = open_store("ployzd-register-isolation-split-admit");
    let (replicated, cluster) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    let peers = publish_peers(&replicated, 3).await;
    let visible = peers.first().expect("three peers");
    let (admin_server, admin, admin_root) = serve_membership(&[(visible, "Alive")]).await;
    let local = LocalMachine::new(store, watch::channel(false).0)
        .with_cluster(Some((replicated.clone(), AdminClient::new(&admin))));

    let registered = local
        .register(request("joiner", WireGuardPublicKey([1; 32])))
        .await
        .unwrap();
    assert_eq!(registered.assigned_machine.name.as_str(), "joiner");

    admin_server.abort();
    let _ = std::fs::remove_dir_all(admin_root);
    cluster.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn client_assignment_publishes_through_entry_and_replays_without_overwriting() {
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
    let mut wrong_key = identity.clone();
    wrong_key.public_key = WireGuardPublicKey([22; 32]);
    assert!(local.register(wrong_key).await.is_err());
    let mut occupied = identity;
    occupied.machine_id = MachineId::random();
    occupied.public_key = WireGuardPublicKey([23; 32]);
    assert!(local.register(occupied).await.is_err());
    assert_eq!(
        replicated.machine(id.as_str()).await.unwrap(),
        Some(first.assigned_machine)
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn register_requires_assignment_and_preserves_observed_rows_on_conflicts() {
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
    for change in 0..4 {
        let mut invalid = identity.clone();
        match change {
            0 => invalid.advertised_endpoints.clear(),
            1 => invalid.assigned_subnet = Some("10.211.1.0/24".parse().unwrap()),
            2 => invalid.public_key = WireGuardPublicKey([2; 32]),
            _ => invalid.machine_id = MachineId::random(),
        }
        assert_eq!(
            rpc_register(&service, invalid).await.unwrap_err().code,
            RpcErrorCode::Conflict
        );
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
