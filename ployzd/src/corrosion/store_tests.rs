use std::{
    collections::BTreeMap,
    net::TcpListener,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use ployz_core::{
    DockerVolume, DockerVolumeId, DockerVolumeName, IngressHost, IssuanceClock, IssuanceFailure,
    Machine, MachineId,
};
use serde_json::json;

use super::ReplicatedStore;
use crate::corrosion::ApiClient;
use crate::machine::{LocalMachineBody, LocalMachinePrior, LocalMachineRecord, LocalMachineStore};
use crate::runtime_watch::RuntimeWatchSnapshot;

#[tokio::test]
async fn catch_up_waits_for_removal_and_rechecks_phase() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let store = ReplicatedStore::new(
        ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
    );
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-catch-up-reset-{}",
        ployz_core::MachineId::random()
    ));
    let mut local = LocalMachineStore::open(&data_dir).unwrap();
    let public_key = local.record().wireguard_private_key.public_key();
    let machine: Machine = serde_json::from_value(json!({
        "id": "b".repeat(32),
        "name": "joining",
        "subnet": "10.210.1.0/24",
        "management_address": "fdcc::1",
        "public_key": public_key.0,
    }))
    .unwrap();
    local
        .join(machine.clone(), vec![machine], BTreeMap::new(), None)
        .unwrap();
    let local = Arc::new(Mutex::new(local));

    let first = store.machine_publication().await;
    let clone = store.clone();
    let task_local = Arc::clone(&local);
    let (started, waiting) = tokio::sync::oneshot::channel();
    let second = tokio::spawn(async move {
        started.send(()).unwrap();
        let publication = clone.machine_publication().await;
        publication.complete_catch_up(&mut task_local.lock().unwrap())
    });
    waiting.await.unwrap();
    tokio::task::yield_now().await;
    assert!(!second.is_finished());

    local.lock().unwrap().begin_reset().unwrap();
    drop(first);
    let completed = tokio::time::timeout(std::time::Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!completed);
    drop(local);
    std::fs::remove_dir_all(data_dir).unwrap();
}

#[tokio::test]
async fn container_publication_waits_for_removal() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let store = ReplicatedStore::new(
        ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
    );
    let (machine, _local) = participating_record();
    let machine_id = machine.id;
    let first = store.machine_publication().await;
    let clone = store.clone();
    let (started, waiting) = tokio::sync::oneshot::channel();
    let second = tokio::spawn(async move {
        started.send(()).unwrap();
        let publication = clone.machine_publication().await;
        publication
            .apply_container_rows(&machine_id, &[], &[])
            .await
    });
    waiting.await.unwrap();
    tokio::task::yield_now().await;
    assert!(!second.is_finished());

    drop(first);
    tokio::time::timeout(std::time::Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn volume_publication_waits_for_removal() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let store = ReplicatedStore::new(
        ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
    );
    let (machine, _local) = participating_record();
    let machine_id = machine.id;
    let first = store.machine_publication().await;
    let clone = store.clone();
    let (started, waiting) = tokio::sync::oneshot::channel();
    let second = tokio::spawn(async move {
        started.send(()).unwrap();
        let publication = clone.machine_publication().await;
        publication.apply_volume_rows(&machine_id, &[], &[]).await
    });
    waiting.await.unwrap();
    tokio::task::yield_now().await;
    assert!(!second.is_finished());

    drop(first);
    tokio::time::timeout(std::time::Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn volume_store_is_an_error_when_the_store_is_unreachable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let store = ReplicatedStore::new(ApiClient::new(address, &"a".repeat(64)).unwrap());
    let id = DockerVolumeId {
        machine_id: MachineId::random(),
        name: DockerVolumeName::parse("data").unwrap(),
    };
    let volume = DockerVolume {
        id: id.clone(),
        driver: "local".into(),
        options: BTreeMap::new(),
        labels: BTreeMap::new(),
    };
    assert!(store.publish_volume(&volume).await.is_err());
    assert!(store.volume(&id).await.is_err());
    assert!(store.volumes().await.is_err());
}

#[tokio::test]
async fn runtime_watch_snapshot_is_an_error_when_the_store_is_unreachable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let store = ReplicatedStore::new(ApiClient::new(address, &"a".repeat(64)).unwrap());
    assert!(store.certificate_rows().await.is_err());
    assert!(RuntimeWatchSnapshot::from_store(&store).await.is_err());
    assert!(store.subscribe_runtime_watch_changes().await.is_err());
    assert!(
        crate::runtime_watch::serve_replicated_runtime_watch(store, MachineId::random())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn record_certificate_failure_is_an_error_when_the_store_is_unreachable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let store = ReplicatedStore::new(ApiClient::new(address, &"a".repeat(64)).unwrap());
    let hostname = IngressHost::parse("app.example.com").unwrap();
    assert!(
        store
            .record_certificate_failure(
                &hostname,
                "does not resolve",
                IssuanceClock::new(1, SystemTime::UNIX_EPOCH, IssuanceFailure::DoesNotResolve,),
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn publication_guard_rechecks_the_local_phase() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let store = ReplicatedStore::new(
        ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
    );
    let (machine, local) = participating_record();
    let publication = store.machine_publication().await;
    assert_eq!(publication.publishable_machine(&local), Some(machine));

    let LocalMachineBody::Participating {
        machine: body_machine,
        cluster_network,
        bootstrap,
    } = local.body
    else {
        panic!("fixture is participating");
    };
    let local = LocalMachineRecord {
        body: LocalMachineBody::Resetting {
            prior: Box::new(LocalMachinePrior::Participating {
                machine: body_machine,
                cluster_network,
                bootstrap,
            }),
        },
        ..local
    };
    assert_eq!(publication.publishable_machine(&local), None);
}

fn participating_record() -> (Machine, LocalMachineRecord) {
    let machine: Machine = serde_json::from_value(json!({
        "id": "b".repeat(32),
        "name": "machine",
        "subnet": "10.210.1.0/24",
        "management_address": "fdcc::1",
        "public_key": vec![3; 32],
    }))
    .unwrap();
    let local = LocalMachineRecord {
        body: LocalMachineBody::Participating {
            machine: machine.clone(),
            cluster_network: None,
            bootstrap: Vec::new(),
        },
        wireguard_private_key: crate::network::WireGuardPrivateKey::from_bytes([0; 32]),
        wireguard_mtu: None,
        selected_endpoints: BTreeMap::new(),
    };
    (machine, local)
}
