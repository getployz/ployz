use std::{
    collections::BTreeMap,
    io,
    net::Ipv6Addr,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use ployz_core::{
    AdvertisedEndpoint, Machine, MachineId, MachineName, MachineRpc, MachineRpcServer,
    MachineRuntime, MachineSubnet, ManagementAddress, MembershipObservation, RegisterRequest,
    Registered, RpcError, RpcErrorCode, RpcResponseBody, WireGuardPublicKey, op,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::watch;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, transport::Server};

use super::{
    LocalMachine, LocalMachineError, LocalMachineStore, RuntimeWatchTelemetry, StoreError,
};
use crate::{
    corrosion::{AdminClient, ReplicatedStore, fake_cluster},
    rpc::{MachineService, REGISTER_FORWARDED_METADATA},
};

#[tokio::test]
async fn register_rejects_empty_endpoints_and_an_uninitialized_machine() {
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
    assert!(matches!(
        empty,
        LocalMachineError::Store(StoreError::MissingEndpoints)
    ));
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
    assert!(matches!(
        duplicate_name,
        LocalMachineError::DuplicateMachine
    ));
    let duplicate_key = local
        .register(request("other", WireGuardPublicKey([1; 32])))
        .await
        .unwrap_err();
    assert!(matches!(duplicate_key, LocalMachineError::DuplicateMachine));

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
    let (allocator_dir, allocator_store, mut reachable) = open_store("ployzd-register-allocator");
    let (allocator_replica, allocator_cluster) = fake_cluster::store().await;
    reachable.management_address = ManagementAddress(Ipv6Addr::LOCALHOST);
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
    tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(machine_service(
                allocator_store,
                allocator_replica.clone(),
                None,
            )))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );

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
    let contact = machine_service(contact_store, contact_replica.clone(), Some(port));

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
    named.management_address = ManagementAddress(Ipv6Addr::LOCALHOST);
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
    let (allocator_dir, allocator_store, mut reachable) = open_store("ployzd-register-moved");
    let (allocator_replica, allocator_cluster) = fake_cluster::store().await;
    reachable.management_address = ManagementAddress(Ipv6Addr::LOCALHOST);
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
    tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(machine_service(
                allocator_store,
                allocator_replica.clone(),
                None,
            )))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );

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
    let contact = machine_service(contact_store, contact_replica.clone(), Some(port));

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
            (founder.management_address, MembershipObservation::Down),
            (peer.management_address, MembershipObservation::Suspect),
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
    let (winner_dir, winner_store, mut winner_machine) = open_store("ployzd-steal-winner");
    let (loser_dir, loser_store, loser_machine) = open_store("ployzd-steal-loser");
    let (replica, cluster) = fake_cluster::store().await;
    winner_machine.management_address = ManagementAddress(Ipv6Addr::LOCALHOST);
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
    tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(winner.clone()))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    let loser = machine_service(loser_store, replica.clone(), Some(port));

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

async fn participating() -> (
    LocalMachine,
    ReplicatedStore,
    Machine,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    let setup = participating_without_allocator().await;
    setup
        .1
        .publish_founder_allocator(&setup.2.id)
        .await
        .unwrap();
    setup
}

async fn participating_without_allocator() -> (
    LocalMachine,
    ReplicatedStore,
    Machine,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    let (replicated, server) = fake_cluster::store().await;
    let (data_dir, store, founder) = open_store("ployzd-register");
    replicated.publish_local_machine(&founder).await.unwrap();
    let local = LocalMachine::new(store, watch::channel(false).0).with_cluster(Some((
        replicated.clone(),
        AdminClient::new("/no/such/ployz-admin.sock"),
    )));
    (local, replicated, founder, data_dir, server)
}

fn open_store(prefix: &str) -> (std::path::PathBuf, Arc<Mutex<LocalMachineStore>>, Machine) {
    let data_dir = std::env::temp_dir().join(format!("{prefix}-{}", MachineId::random()));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    let founder = store
        .initialize(
            MachineName::parse("edge").unwrap(),
            super::FoundingCluster {
                network: "10.210.0.0/16".parse().unwrap(),
                ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
            },
            None,
            vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            None,
            None,
        )
        .unwrap();
    (data_dir, Arc::new(Mutex::new(store)), founder)
}

fn machine_service(
    store: Arc<Mutex<LocalMachineStore>>,
    replicated: ReplicatedStore,
    port: Option<u16>,
) -> MachineService {
    let service = MachineService::with_cluster(
        store,
        watch::channel(false).0,
        Some((replicated, AdminClient::new("/no/such/ployz-admin.sock"))),
    );
    match port {
        Some(port) => service.with_machine_api_port(port),
        None => service,
    }
}

async fn publish_peers(replicated: &ReplicatedStore, count: usize) -> Vec<Machine> {
    let mut peers = Vec::with_capacity(count);
    for index in 0..count {
        let seed = u8::try_from(index + 10).expect("peer seeds fit u8");
        let machine = Machine {
            id: MachineId::random(),
            name: MachineName::parse(format!("peer-{seed}")).unwrap(),
            subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
            management_address: ManagementAddress(format!("fdcc::{seed}").parse().unwrap()),
            public_key: WireGuardPublicKey([seed; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint(
                format!("192.0.2.{seed}:51820").parse().unwrap(),
            )],
            runtime: MachineRuntime::default(),
        };
        replicated.publish_local_machine(&machine).await.unwrap();
        peers.push(machine);
    }
    peers
}

async fn serve_membership(
    states: &[(&Machine, &'static str)],
) -> (tokio::task::JoinHandle<()>, PathBuf, PathBuf) {
    let states: Vec<_> = states
        .iter()
        .map(|&(machine, state)| (format!("[{}]:51001", machine.management_address.0), state))
        .collect();
    let root = std::env::temp_dir().join(format!("ployzd-register-admin-{}", MachineId::random()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("admin.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            if read_admin_frame(&mut stream).await.is_err() {
                continue;
            }
            for (addr, state) in &states {
                let payload = serde_json::json!({
                    "Json": {"id": {"addr": addr}, "state": state}
                });
                if write_admin_frame(&mut stream, &serde_json::to_vec(&payload).unwrap())
                    .await
                    .is_err()
                {
                    break;
                }
            }
            let _ = write_admin_frame(&mut stream, br#""Success""#).await;
        }
    });
    (server, path, root)
}

async fn read_admin_frame(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let length = stream.read_u32().await?;
    let mut data = vec![0; length as usize];
    stream.read_exact(&mut data).await?;
    Ok(data)
}

async fn write_admin_frame(stream: &mut UnixStream, data: &[u8]) -> io::Result<()> {
    stream.write_u32(data.len() as u32).await?;
    stream.write_all(data).await
}

fn unreachable_allocator(id: MachineId) -> Machine {
    Machine {
        id,
        name: MachineName::parse("allocator").unwrap(),
        subnet: "10.210.0.0/24".parse().unwrap(),
        management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
        public_key: WireGuardPublicKey([3; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.3:51820".parse().unwrap())],
        runtime: MachineRuntime::default(),
    }
}

fn request(name: &str, public_key: WireGuardPublicKey) -> RegisterRequest {
    RegisterRequest {
        name: MachineName::parse(name).unwrap(),
        storage: ployz_core::StorageChoice::None,
        public_key,
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.9:51820".parse().unwrap())],
        runtime: MachineRuntime::default(),
    }
}

async fn rpc_register(
    service: &MachineService,
    body: RegisterRequest,
    forwarded: bool,
) -> Result<Registered, RpcError> {
    let mut request = Request::new(op::Register::into_request(body).encode().unwrap());
    if forwarded {
        request.metadata_mut().insert(
            REGISTER_FORWARDED_METADATA,
            "1".parse().expect("ASCII metadata"),
        );
    }
    let response = service
        .register(request)
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap();
    if let RpcResponseBody::Error(error) = &response.body {
        return Err(error.clone());
    }
    Ok(response.decode::<op::Register>().unwrap())
}
