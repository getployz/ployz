use std::sync::{Arc, Mutex};

use ployz_core::{
    AdvertisedEndpoint, Machine, MachineName, MachineRuntime, MachineSubnet, RegisterRequest,
    WireGuardPublicKey,
};
use tokio::sync::watch;

use super::{LocalMachine, LocalMachineError, LocalMachineStore, StoreError};
use crate::corrosion::{AdminClient, ReplicatedStore, fake_cluster};

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

async fn participating() -> (
    LocalMachine,
    ReplicatedStore,
    Machine,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    let (replicated, server) = fake_cluster::store().await;
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-register-{}",
        ployz_core::MachineId::random()
    ));
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
    replicated.publish_local_machine(&founder).await.unwrap();
    let local = LocalMachine::new(Arc::new(Mutex::new(store)), watch::channel(false).0)
        .with_cluster(Some((
            replicated.clone(),
            AdminClient::new("/no/such/ployz-admin.sock"),
        )));
    (local, replicated, founder, data_dir, server)
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
