use std::{
    collections::BTreeMap,
    net::Ipv6Addr,
    sync::{Arc, Mutex},
};

use ployz_core::{
    AdvertisedEndpoint, JoinRequest, LocalMachinePhase, Machine, MachineId, MachineName,
    MachineRuntime, MachineSubnet, MembershipObservation, RegisterRequest, RpcErrorCode,
    WireGuardPublicKey,
};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

use super::{
    LocalMachine, LocalMachineError, LocalMachineStore, RuntimeWatchTelemetry, StoreError,
};
use crate::{
    corrosion::{AdminClient, fake_cluster},
    machine_api::{MachineApi, MachineService},
};

mod harness;
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
async fn register_assigns_a_free_subnet_publishes_and_rejects_duplicates() {
    let (local, replicated, founder, data_dir, server) = participating().await;
    let missing = local
        .register(RegisterRequest {
            advertised_endpoints: Vec::new(),
            ..request("peer", WireGuardPublicKey([1; 32]))
        })
        .await
        .unwrap_err();
    assert!(matches!(
        missing,
        LocalMachineError::Store(StoreError::MissingEndpoints)
    ));
    let registered = local
        .register(request("peer", WireGuardPublicKey([1; 32])))
        .await
        .unwrap();
    assert_eq!(
        registered.assigned_machine.subnet,
        "10.210.1.0/24".parse().unwrap()
    );
    assert_eq!(registered.visible_peers, vec![founder]);
    assert_eq!(
        replicated
            .machine(registered.assigned_machine.id.as_str())
            .await
            .unwrap()
            .as_ref(),
        Some(&registered.assigned_machine)
    );

    let duplicate_name = local
        .register(request("peer", WireGuardPublicKey([2; 32])))
        .await
        .unwrap_err();
    assert!(matches!(duplicate_name, LocalMachineError::NameTaken));
    let duplicate_key = local
        .register(request("other", WireGuardPublicKey([1; 32])))
        .await
        .unwrap_err();
    assert!(matches!(duplicate_key, LocalMachineError::KeyAlreadyNamed));

    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn register_rpc_exact_replay_returns_the_original_joinable_assignment() {
    let (data_dir, store, founder) = open_store("ployzd-register-replay");
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    replicated
        .publish_founder_allocator(&founder.id)
        .await
        .unwrap();
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
    let identity = request("joiner", public_key);

    let first = rpc_register(&service, identity.clone(), false)
        .await
        .unwrap();
    let replay = rpc_register(&service, identity, false).await.unwrap();

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
        .unwrap();
    assert_eq!(target.record().unwrap().phase(), LocalMachinePhase::Joining);

    let conflict = rpc_register(&service, request("other", public_key), false)
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
    let (allocator, replicated, _founder, data_dir, server) = participating().await;
    let joiner_dir =
        std::env::temp_dir().join(format!("ployzd-register-joining-{}", MachineId::random()));
    let joiner_store = Arc::new(Mutex::new(LocalMachineStore::open(&joiner_dir).unwrap()));
    let public_key = joiner_store
        .lock()
        .unwrap()
        .record()
        .wireguard_private_key
        .public_key();
    let registered = allocator
        .register(request("peer", public_key))
        .await
        .unwrap();
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
        .unwrap();
    assert_eq!(joiner.record().unwrap().phase(), LocalMachinePhase::Joining);

    let error = joiner
        .register(request("peer", public_key))
        .await
        .unwrap_err();
    assert!(matches!(error, LocalMachineError::NotParticipating));

    server.abort();
    drop(allocator);
    drop(joiner);
    let _ = std::fs::remove_dir_all(data_dir);
    let _ = std::fs::remove_dir_all(joiner_dir);
}

#[tokio::test]
async fn register_returns_the_committed_row_after_join_runtime_and_endpoint_drift() {
    let (local, replicated, _founder, data_dir, server) = participating().await;
    let key = WireGuardPublicKey([1; 32]);
    let registered = local.register(request("peer", key)).await.unwrap();
    let assigned_id = registered.assigned_machine.id;
    let mut drifted = registered.assigned_machine.clone();
    drifted.runtime = MachineRuntime {
        daemon_version: "joined".into(),
        ..MachineRuntime::default()
    };
    drifted.public_ip = Some("198.51.100.9".parse().unwrap());
    drifted.advertised_endpoints = vec![AdvertisedEndpoint("198.51.100.9:51820".parse().unwrap())];
    replicated.publish_local_machine(&drifted).await.unwrap();

    let mut replay = request("peer", key);
    replay.public_ip = None;
    replay.runtime = MachineRuntime::default();
    let replayed = local.register(replay).await.unwrap();

    assert_eq!(replayed.assigned_machine.id, assigned_id);
    assert_eq!(replayed.assigned_machine.subnet, drifted.subnet);
    assert_eq!(replayed.assigned_machine.runtime.daemon_version, "joined");
    assert_eq!(
        replayed.assigned_machine.advertised_endpoints,
        drifted.advertised_endpoints
    );
    let stored = replicated
        .machine(assigned_id.as_str())
        .await
        .unwrap()
        .expect("committed Machine remains");
    assert_eq!(stored, drifted);
    assert_eq!(replicated.machines().await.unwrap().observations.len(), 2);

    let renamed = local.register(request("other", key)).await.unwrap_err();
    assert!(matches!(renamed, LocalMachineError::KeyAlreadyNamed));

    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn register_returns_the_committed_row_when_advertised_endpoints_are_empty() {
    let (local, replicated, _founder, data_dir, server) = participating().await;
    let key = WireGuardPublicKey([1; 32]);
    let registered = local.register(request("peer", key)).await.unwrap();
    let assigned_id = registered.assigned_machine.id;
    let committed = registered.assigned_machine.clone();

    let mut replay = request("peer", key);
    replay.advertised_endpoints = Vec::new();
    let replayed = local.register(replay).await.unwrap();

    assert_eq!(replayed.assigned_machine.id, assigned_id);
    assert_eq!(
        replayed.assigned_machine.advertised_endpoints,
        committed.advertised_endpoints
    );
    let stored = replicated
        .machine(assigned_id.as_str())
        .await
        .unwrap()
        .expect("committed Machine remains");
    assert_eq!(stored, committed);
    assert_eq!(replicated.machines().await.unwrap().observations.len(), 2);

    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn register_returns_the_committed_row_when_this_machine_is_not_the_allocator() {
    let (local, replicated, founder, data_dir, server) = participating_without_allocator().await;
    replicated
        .publish_founder_allocator(&MachineId::random())
        .await
        .unwrap();
    let peer = Machine {
        id: MachineId::random(),
        name: MachineName::parse("peer").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        public_key: WireGuardPublicKey([1; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.9:51820".parse().unwrap())],
        runtime: MachineRuntime {
            daemon_version: "joined".into(),
            ..MachineRuntime::default()
        },
    };
    replicated.publish_local_machine(&peer).await.unwrap();

    let replayed = local
        .register(request("peer", WireGuardPublicKey([1; 32])))
        .await
        .unwrap();

    assert_eq!(replayed.assigned_machine, peer);
    assert_eq!(replayed.visible_peers, vec![founder]);
    assert_eq!(replicated.machines().await.unwrap().observations.len(), 2);

    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn overlapping_registers_on_one_daemon_get_distinct_machine_subnets() {
    let (local, replicated, _founder, data_dir, server) = participating().await;
    let publication = replicated.machine_publication().await;
    let first = {
        let local = local.clone();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            started.send(()).unwrap();
            local
                .register(request("peer-a", WireGuardPublicKey([1; 32])))
                .await
        });
        (task, waiting)
    };
    let second = {
        let local = local.clone();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            started.send(()).unwrap();
            local
                .register(request("peer-b", WireGuardPublicKey([2; 32])))
                .await
        });
        (task, waiting)
    };
    first.1.await.unwrap();
    second.1.await.unwrap();
    tokio::task::yield_now().await;
    assert!(
        !first.0.is_finished() && !second.0.is_finished(),
        "both Register calls must still be in flight"
    );
    drop(publication);
    let first = first.0.await.unwrap().unwrap();
    let second = second.0.await.unwrap().unwrap();
    assert_ne!(
        first.assigned_machine.subnet,
        second.assigned_machine.subnet
    );
    let expected = [
        MachineSubnet::parse("10.210.1.0/24").unwrap(),
        MachineSubnet::parse("10.210.2.0/24").unwrap(),
    ];
    assert!(expected.contains(&first.assigned_machine.subnet));
    assert!(expected.contains(&second.assigned_machine.subnet));

    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn register_does_not_allocate_when_this_machine_is_not_the_allocator() {
    let (local, replicated, _founder, data_dir, server) = participating_without_allocator().await;
    replicated
        .publish_founder_allocator(&ployz_core::MachineId::random())
        .await
        .unwrap();
    let error = local
        .register(request("peer", WireGuardPublicKey([1; 32])))
        .await
        .unwrap_err();
    assert!(matches!(error, LocalMachineError::NotAllocator));
    assert_eq!(replicated.machines().await.unwrap().observations.len(), 1);

    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn register_is_not_quiet_when_the_allocator_row_is_young() {
    let (local, replicated, founder, data_dir, server) = participating_without_allocator().await;
    replicated.steal_allocator(&founder.id).await.unwrap();
    let error = local
        .register(request("peer", WireGuardPublicKey([1; 32])))
        .await
        .unwrap_err();
    assert!(matches!(error, LocalMachineError::AllocatorNotQuiet));
    assert_eq!(replicated.machines().await.unwrap().observations.len(), 1);

    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn register_does_not_allocate_when_allocator_row_is_missing() {
    let (local, replicated, _founder, data_dir, server) = participating_without_allocator().await;
    let error = local
        .register(request("peer", WireGuardPublicKey([1; 32])))
        .await
        .unwrap_err();
    assert!(matches!(error, LocalMachineError::NotAllocator));
    assert_eq!(replicated.machines().await.unwrap().observations.len(), 1);

    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn contact_forwards_register_and_returns_the_allocator_payload() {
    let (allocator_dir, allocator_store, reachable) = open_store("ployzd-register-allocator");
    let (allocator_replica, allocator_cluster) = fake_cluster::store().await;
    allocator_replica
        .publish_local_machine(&reachable)
        .await
        .unwrap();
    allocator_replica
        .publish_founder_allocator(&reachable.id)
        .await
        .unwrap();
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(Server::builder().serve_with_incoming(
        MachineApi::from_local(machine_service(
            allocator_store,
            allocator_replica.clone(),
            None,
        )),
        TcpListenerStream::new(listener),
    ));

    let (contact_dir, contact_store, _contact) = open_store("ployzd-register-contact");
    let (contact_replica, contact_cluster) = fake_cluster::store().await;
    contact_replica
        .publish_local_machine(&reachable)
        .await
        .unwrap();
    contact_replica
        .publish_founder_allocator(&reachable.id)
        .await
        .unwrap();
    let contact = machine_service(contact_store, contact_replica.clone(), Some(port))
        .with_allocator_endpoint(
            reachable.id,
            std::net::SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
        );

    let registered = rpc_register(
        &contact,
        request("joiner", WireGuardPublicKey([7; 32])),
        false,
    )
    .await
    .unwrap();

    assert_eq!(registered.assigned_machine.name.as_str(), "joiner");
    assert_eq!(
        registered.assigned_machine.subnet,
        "10.210.1.0/24".parse().unwrap()
    );
    assert!(
        allocator_replica
            .machines()
            .await
            .unwrap()
            .observations
            .iter()
            .any(|machine| machine.name.as_str() == "joiner"),
        "Allocator admits locally"
    );
    assert!(
        contact_replica
            .machines()
            .await
            .unwrap()
            .observations
            .iter()
            .all(|machine| machine.name.as_str() != "joiner"),
        "contact must not allocate locally"
    );
    allocator_cluster.abort();
    contact_cluster.abort();
    let _ = std::fs::remove_dir_all(allocator_dir);
    let _ = std::fs::remove_dir_all(contact_dir);
}

#[tokio::test]
async fn forwarded_register_does_not_admit_or_forward_when_kv_names_another_machine() {
    let (data_dir, store, local_machine) = open_store("ployzd-register-one-hop");
    let other = MachineId::parse("b".repeat(32)).unwrap();
    let mut named = local_machine;
    named.id = other;
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&named).await.unwrap();
    replicated
        .publish_founder_allocator(&named.id)
        .await
        .unwrap();
    let local = machine_service(store, replicated.clone(), Some(1));

    let error = rpc_register(&local, request("joiner", WireGuardPublicKey([7; 32])), true)
        .await
        .unwrap_err();

    assert_eq!(error.message, "this Machine is not the Allocator");
    assert!(
        replicated
            .machines()
            .await
            .unwrap()
            .observations
            .iter()
            .all(|machine| machine.name.as_str() != "joiner")
    );
    assert_eq!(
        replicated
            .allocator()
            .await
            .unwrap()
            .map(|row| row.machine_id),
        Some(other)
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn unreachable_allocator_steals_retryable_not_quiet_without_a_subnet() {
    let (data_dir, store, founder) = open_store("ployzd-register-steal");
    let allocator_id = MachineId::parse("c".repeat(32)).unwrap();
    let named = unreachable_allocator(allocator_id);
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&named).await.unwrap();
    replicated
        .publish_founder_allocator(&named.id)
        .await
        .unwrap();
    let local = machine_service(store, replicated.clone(), Some(1));

    let error = rpc_register(
        &local,
        request("joiner", WireGuardPublicKey([7; 32])),
        false,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "Allocator is not quiet");
    assert_eq!(
        replicated
            .allocator()
            .await
            .unwrap()
            .map(|row| (row.machine_id, row.quiet)),
        Some((founder.id, false))
    );
    assert!(
        replicated
            .machines()
            .await
            .unwrap()
            .observations
            .iter()
            .all(|machine| machine.name.as_str() != "joiner"),
        "steal must not assign a Machine Subnet on that call"
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn stealer_admits_after_the_quiet_gate() {
    let (data_dir, store, founder) = open_store("ployzd-register-steal-admit");
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    replicated
        .publish_local_machine(&unreachable_allocator(
            MachineId::parse("c".repeat(32)).unwrap(),
        ))
        .await
        .unwrap();
    replicated
        .publish_founder_allocator(&MachineId::parse("c".repeat(32)).unwrap())
        .await
        .unwrap();
    let local = machine_service(store, replicated.clone(), Some(1));

    let error = rpc_register(&local, request("first", WireGuardPublicKey([7; 32])), false)
        .await
        .unwrap_err();
    assert_eq!(error.message, "Allocator is not quiet");

    fake_cluster::age_allocator(&replicated).await;
    let registered = rpc_register(
        &local,
        request("joiner", WireGuardPublicKey([8; 32])),
        false,
    )
    .await
    .unwrap();

    assert_eq!(registered.assigned_machine.name.as_str(), "joiner");
    assert_eq!(
        registered.assigned_machine.subnet,
        "10.210.1.0/24".parse().unwrap()
    );
    assert_eq!(
        replicated
            .allocator()
            .await
            .unwrap()
            .map(|row| (row.machine_id, row.quiet)),
        Some((founder.id, true))
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn unreachable_allocator_forwards_when_reread_names_a_reachable_allocator() {
    let (allocator_dir, allocator_store, reachable) = open_store("ployzd-register-moved");
    let (allocator_replica, allocator_cluster) = fake_cluster::store().await;
    allocator_replica
        .publish_local_machine(&reachable)
        .await
        .unwrap();
    allocator_replica
        .publish_founder_allocator(&reachable.id)
        .await
        .unwrap();
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(Server::builder().serve_with_incoming(
        MachineApi::from_local(machine_service(
            allocator_store,
            allocator_replica.clone(),
            None,
        )),
        TcpListenerStream::new(listener),
    ));

    let (contact_dir, contact_store, _contact_machine) = open_store("ployzd-register-reread");
    let (contact_replica, contact_cluster) = fake_cluster::store().await;
    let unreachable_id = MachineId::parse("c".repeat(32)).unwrap();
    contact_replica
        .publish_local_machine(&reachable)
        .await
        .unwrap();
    contact_replica
        .publish_founder_allocator(&unreachable_id)
        .await
        .unwrap();
    fake_cluster::name_allocator_on_reread(&contact_replica, &unreachable_id, &reachable.id).await;
    let contact = machine_service(contact_store, contact_replica.clone(), Some(port))
        .with_allocator_endpoint(
            reachable.id,
            std::net::SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
        );

    let registered = rpc_register(
        &contact,
        request("joiner", WireGuardPublicKey([7; 32])),
        false,
    )
    .await
    .unwrap();

    assert_eq!(registered.assigned_machine.name.as_str(), "joiner");
    assert_eq!(
        contact_replica
            .allocator()
            .await
            .unwrap()
            .map(|row| row.machine_id),
        Some(reachable.id)
    );
    allocator_cluster.abort();
    contact_cluster.abort();
    let _ = std::fs::remove_dir_all(allocator_dir);
    let _ = std::fs::remove_dir_all(contact_dir);
}

#[tokio::test]
async fn membership_down_or_suspect_does_not_steal() {
    let (local, replicated, founder, data_dir, server) = participating().await;
    let before = replicated.allocator().await.unwrap();
    let founder_id = founder.id;
    let peer = unreachable_allocator(MachineId::parse("d".repeat(32)).unwrap());
    let _observations = RuntimeWatchTelemetry {
        states: BTreeMap::from([
            (founder.management_address(), MembershipObservation::Down),
            (peer.management_address(), MembershipObservation::Suspect),
        ]),
        selected_endpoints: BTreeMap::new(),
        rtts: Vec::new(),
    }
    .overlay(vec![founder, peer], &founder_id);

    assert_eq!(replicated.allocator().await.unwrap(), before);
    local
        .register(request("joiner", WireGuardPublicKey([7; 32])))
        .await
        .unwrap();
    assert_eq!(
        replicated
            .allocator()
            .await
            .unwrap()
            .map(|row| row.machine_id),
        Some(founder_id)
    );

    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn two_steals_leave_one_writer_and_the_loser_forwards() {
    let (winner_dir, winner_store, winner_machine) = open_store("ployzd-steal-winner");
    let (loser_dir, loser_store, loser_machine) = open_store("ployzd-steal-loser");
    let (replica, cluster) = fake_cluster::store().await;
    let unreachable_id = MachineId::parse("c".repeat(32)).unwrap();
    replica
        .publish_local_machine(&unreachable_allocator(unreachable_id))
        .await
        .unwrap();
    replica
        .publish_local_machine(&winner_machine)
        .await
        .unwrap();
    replica.publish_local_machine(&loser_machine).await.unwrap();
    replica
        .publish_founder_allocator(&unreachable_id)
        .await
        .unwrap();

    let loser = machine_service(loser_store.clone(), replica.clone(), Some(1));
    let winner = machine_service(winner_store, replica.clone(), Some(1));
    assert_eq!(
        rpc_register(&loser, request("first", WireGuardPublicKey([7; 32])), false)
            .await
            .unwrap_err()
            .message,
        "Allocator is not quiet"
    );
    assert_eq!(
        rpc_register(
            &winner,
            request("second", WireGuardPublicKey([8; 32])),
            false
        )
        .await
        .unwrap_err()
        .message,
        "Allocator is not quiet"
    );
    assert_eq!(
        replica.allocator().await.unwrap().map(|row| row.machine_id),
        Some(winner_machine.id)
    );

    fake_cluster::age_allocator(&replica).await;
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(Server::builder().serve_with_incoming(
        MachineApi::from_local(winner.clone()),
        TcpListenerStream::new(listener),
    ));
    let loser = machine_service(loser_store, replica.clone(), Some(port)).with_allocator_endpoint(
        winner_machine.id,
        std::net::SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
    );

    let forwarded = rpc_register(
        &loser,
        request("joiner", WireGuardPublicKey([9; 32])),
        false,
    )
    .await
    .unwrap();
    assert_eq!(forwarded.assigned_machine.name.as_str(), "joiner");
    assert_eq!(
        replica.allocator().await.unwrap().map(|row| row.machine_id),
        Some(winner_machine.id)
    );

    let admitted = rpc_register(
        &winner,
        request("other", WireGuardPublicKey([10; 32])),
        false,
    )
    .await
    .unwrap();
    assert_eq!(admitted.assigned_machine.name.as_str(), "other");

    cluster.abort();
    let _ = std::fs::remove_dir_all(winner_dir);
    let _ = std::fs::remove_dir_all(loser_dir);
}

#[tokio::test]
async fn forwarded_rpc_metadata_admits_locally_only() {
    let (data_dir, store, founder) = open_store("ployzd-register-metadata");
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    replicated
        .publish_founder_allocator(&founder.id)
        .await
        .unwrap();
    let service = machine_service(store, replicated.clone(), None);
    let registered = rpc_register(
        &service,
        request("joiner", WireGuardPublicKey([7; 32])),
        true,
    )
    .await
    .unwrap();

    assert_eq!(registered.assigned_machine.name.as_str(), "joiner");
    server.abort();
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn isolation_lock_refuses_admit_when_replica_exceeds_three_and_others_are_uncontactable() {
    let (data_dir, store, founder) = open_store("ployzd-register-isolation-admit");
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    publish_peers(&replicated, 3).await;
    replicated
        .publish_founder_allocator(&founder.id)
        .await
        .unwrap();
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
async fn isolation_lock_refuses_steal_when_replica_exceeds_three_and_others_are_uncontactable() {
    let (data_dir, store, founder) = open_store("ployzd-register-isolation-steal");
    let named = unreachable_allocator(MachineId::parse("c".repeat(32)).unwrap());
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    replicated.publish_local_machine(&named).await.unwrap();
    publish_peers(&replicated, 2).await;
    replicated
        .publish_founder_allocator(&named.id)
        .await
        .unwrap();
    let (admin_server, admin, admin_root) = serve_membership(&[]).await;
    let local = MachineService::with_cluster(
        store,
        watch::channel(false).0,
        Some((replicated.clone(), AdminClient::new(&admin))),
    )
    .with_machine_api_port(1);

    let error = rpc_register(
        &local,
        request("joiner", WireGuardPublicKey([7; 32])),
        false,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "this Machine is isolation-locked");
    assert_eq!(
        replicated
            .allocator()
            .await
            .unwrap()
            .map(|row| row.machine_id),
        Some(named.id)
    );
    assert!(
        replicated
            .machines()
            .await
            .unwrap()
            .observations
            .iter()
            .all(|machine| machine.name.as_str() != "joiner"),
        "isolation must not steal or assign a Machine Subnet"
    );
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
    replicated
        .publish_founder_allocator(&founder.id)
        .await
        .unwrap();
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
