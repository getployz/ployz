use std::{
    net::Ipv6Addr,
    sync::{Arc, Mutex},
};

use ployz_core::{
    AdvertisedEndpoint, Machine, MachineId, MachineName, MachineRpc, MachineRpcServer,
    MachineRuntime, MachineSubnet, ManagementAddress, RegisterRequest, Registered, RpcError,
    RpcResponseBody, WireGuardPublicKey, op,
};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, transport::Server};

use super::{LocalMachine, LocalMachineError, LocalMachineStore, StoreError};
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
    fake_cluster::insert_young_allocator(&replicated, &founder.id).await;
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
async fn unreachable_allocator_does_not_steal() {
    let (data_dir, store, _local_machine) = open_store("ployzd-register-unreachable");
    let allocator_id = MachineId::parse("c".repeat(32)).unwrap();
    let named = Machine {
        id: allocator_id,
        name: MachineName::parse("allocator").unwrap(),
        subnet: "10.210.0.0/24".parse().unwrap(),
        management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
        public_key: WireGuardPublicKey([3; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.3:51820".parse().unwrap())],
        runtime: MachineRuntime::default(),
    };
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

    assert_eq!(error.message, "Allocator is unreachable");
    assert_eq!(
        replicated
            .allocator()
            .await
            .unwrap()
            .map(|row| row.machine_id),
        Some(allocator_id)
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
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
            "10.210.0.0/16".parse().unwrap(),
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

fn request(name: &str, public_key: WireGuardPublicKey) -> RegisterRequest {
    RegisterRequest {
        name: MachineName::parse(name).unwrap(),
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
