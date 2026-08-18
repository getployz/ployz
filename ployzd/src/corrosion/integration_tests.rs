use std::{
    collections::BTreeMap,
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use ployz_core::{
    AdvertisedEndpoint, ContainerId, ContainerObservation, DockerVolume, DockerVolumeId,
    DockerVolumeName, IngressHost, LocalMachinePhase, Machine, MachineId, MachineName,
    ManagementAddress, WireGuardPublicKey,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    ApiClient, CertificateMaterial, CorrosionConfig, ReplicatedStore, Statement,
    run_machine_publisher, wait_for_catch_up,
};
use crate::machine::{LocalMachineBody, LocalMachineRecord, LocalMachineStore};
use crate::network::WireGuardPrivateKey;

#[tokio::test]
#[ignore = "requires Docker and the pinned Corrosion image"]
async fn replicated_store_preserves_partial_and_contradictory_observations() {
    let root = TestRoot::new();
    let api_addr = unused_address();
    let gossip_addr = unused_address();
    let name = format!("ployz-corrosion-{}", MachineId::random());
    let mut running = CorrosionConfig::new(
        root.0.join("data"),
        root.0.join("run"),
        api_addr,
        gossip_addr,
        name,
    )
    .start()
    .await
    .unwrap();
    let store = running.store().clone();
    running.membership_states().await.unwrap();

    let local = machine("duplicate", 1);
    store.publish_local_machine(&local).await.unwrap();
    let version_after_first_publish = store.version().await.unwrap();
    store.publish_local_machine(&local).await.unwrap();
    assert_eq!(store.version().await.unwrap(), version_after_first_publish);

    let duplicate = machine("duplicate", 2);
    store.publish_local_machine(&duplicate).await.unwrap();
    let incomplete_machine_id = MachineId::random();
    running
        .store()
        .api()
        .execute([Statement::new(
            "INSERT INTO machines (id) VALUES (?)",
            [json!(&incomplete_machine_id)],
        )])
        .await
        .unwrap();

    let machines = store.machines().await.unwrap();
    assert_eq!(machines.observations.len(), 2);
    assert_eq!(machines.incomplete_ids.len(), 1);
    assert!(machines.observations.contains(&local));
    assert!(machines.observations.contains(&duplicate));
    store.remove_machine(&incomplete_machine_id).await.unwrap();
    assert!(store.machines().await.unwrap().incomplete_ids.is_empty());

    let mut container_changes = store.subscribe_container_changes().await.unwrap();
    let observation = container(&local.id, "a");
    store.publish_container(&observation).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), container_changes.changed())
        .await
        .unwrap()
        .unwrap();
    let duplicate_service_name = container(&local.id, "c");
    store
        .publish_container(&duplicate_service_name)
        .await
        .unwrap();
    let incomplete_id = ContainerId::parse("a".repeat(64)).unwrap();
    running
        .store()
        .api()
        .execute([Statement::new(
            "INSERT INTO containers (id) VALUES (?)",
            [json!(&incomplete_id)],
        )])
        .await
        .unwrap();
    let containers = store.containers().await.unwrap();
    assert_eq!(
        containers.observations,
        vec![observation, duplicate_service_name]
    );
    assert_eq!(containers.incomplete_ids, vec![incomplete_id]);

    let target = store.version().await.unwrap();
    let actor = target.keys().next().unwrap();
    running
        .store()
        .api()
        .execute([Statement::new(
            "INSERT INTO __corro_bookkeeping_gaps (actor_id, start, end) VALUES (?, 1, 1)",
            [json!(hex_bytes(actor))],
        )])
        .await
        .unwrap();
    assert!(store.has_known_missing_changes().await.unwrap());
    assert!(
        tokio::time::timeout(
            Duration::from_millis(600),
            wait_for_catch_up(&store, &target),
        )
        .await
        .is_err()
    );
    running
        .store()
        .api()
        .execute([Statement::new("DELETE FROM __corro_bookkeeping_gaps", [])])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), wait_for_catch_up(&store, &target))
        .await
        .unwrap()
        .unwrap();

    let mut ahead = target;
    *ahead.values_mut().next().unwrap() += 1;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(600),
            wait_for_catch_up(&store, &ahead),
        )
        .await
        .is_err()
    );
    running
        .store()
        .api()
        .execute([Statement::new(
            "INSERT INTO cluster (key, value) VALUES ('catch-up', 1)",
            [],
        )])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), wait_for_catch_up(&store, &ahead))
        .await
        .unwrap()
        .unwrap();

    let published = machine("publisher", 3);
    let local_dir = root.0.join("local-machine");
    write_record(
        &local_dir,
        &LocalMachineRecord {
            body: LocalMachineBody::Joining {
                machine: published.clone(),
                bootstrap: Vec::new(),
                min_store_version: store.version().await.unwrap(),
            },
            wireguard_private_key: WireGuardPrivateKey::generate(),
            wireguard_mtu: None,
            selected_endpoints: BTreeMap::new(),
        },
    );
    let local = Arc::new(Mutex::new(LocalMachineStore::open(&local_dir).unwrap()));
    let published = local.lock().unwrap().record().machine().cloned().unwrap();
    let shutdown = CancellationToken::new();
    let (participating, participating_rx) = tokio::sync::watch::channel(false);
    let publisher = tokio::spawn(run_machine_publisher(
        Some(store.clone()),
        Arc::clone(&local),
        participating,
        shutdown.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if store.machine(published.id.as_str()).await.unwrap() == Some(published.clone()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    shutdown.cancel();
    publisher.await.unwrap().unwrap();
    let persisted: LocalMachineRecord =
        serde_json::from_slice(&fs::read(local_dir.join("machine.json")).unwrap()).unwrap();
    assert_eq!(persisted.phase(), LocalMachinePhase::Participating);
    assert!(persisted.min_store_version().is_empty());
    assert!(*participating_rx.borrow());

    let interrupted_dir = root.0.join("interrupted-machine");
    let target = BTreeMap::from([("unreachable-actor".to_owned(), 1)]);
    write_record(
        &interrupted_dir,
        &LocalMachineRecord {
            body: LocalMachineBody::Joining {
                machine: machine("interrupted", 4),
                bootstrap: Vec::new(),
                min_store_version: target.clone(),
            },
            wireguard_private_key: WireGuardPrivateKey::generate(),
            wireguard_mtu: None,
            selected_endpoints: BTreeMap::new(),
        },
    );
    let interrupted = Arc::new(Mutex::new(
        LocalMachineStore::open(&interrupted_dir).unwrap(),
    ));
    let unavailable =
        ReplicatedStore::new(ApiClient::new(unused_address(), &"a".repeat(64)).unwrap());
    let shutdown = CancellationToken::new();
    let (participating, participating_rx) = tokio::sync::watch::channel(false);
    let publisher = tokio::spawn(run_machine_publisher(
        Some(unavailable),
        interrupted,
        participating,
        shutdown.clone(),
    ));
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!publisher.is_finished());
    shutdown.cancel();
    publisher.await.unwrap().unwrap();
    let persisted: LocalMachineRecord =
        serde_json::from_slice(&fs::read(interrupted_dir.join("machine.json")).unwrap()).unwrap();
    assert_eq!(persisted.phase(), LocalMachinePhase::Joining);
    assert_eq!(persisted.min_store_version(), &target);
    assert!(!*participating_rx.borrow());

    running.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Docker and the pinned Corrosion image"]
async fn certificates_round_trip_and_notify_on_change() {
    let root = TestRoot::new();
    let mut running = CorrosionConfig::new(
        root.0.join("data"),
        root.0.join("run"),
        unused_address(),
        unused_address(),
        format!("ployz-corrosion-certs-{}", MachineId::random()),
    )
    .start()
    .await
    .unwrap();
    let store = running.store().clone();
    let hostname = IngressHost::parse("app.example.com").unwrap();
    let material = CertificateMaterial::new("CERT", "KEY").unwrap();
    let mut changes = store.subscribe_certificate_changes().await.unwrap();

    store
        .publish_certificate(&hostname, &material)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), changes.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.certificate(&hostname).await.unwrap().as_ref(),
        Some(&material)
    );
    assert_eq!(
        store.certificates().await.unwrap().get(&hostname),
        Some(&material)
    );

    store
        .publish_certificate(&hostname, &material)
        .await
        .unwrap();
    let updated = CertificateMaterial::new("CERT-2", "KEY-2").unwrap();
    store
        .publish_certificate(&hostname, &updated)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), changes.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.certificate(&hostname).await.unwrap().as_ref(),
        Some(&updated)
    );

    running.cleanup().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Docker and the pinned Corrosion image"]
async fn replicated_volumes_round_trip_incomplete_additive_and_machine_removal() {
    let root = TestRoot::new();
    let mut running = CorrosionConfig::new(
        root.0.join("data"),
        root.0.join("run"),
        unused_address(),
        unused_address(),
        format!("ployz-corrosion-volumes-{}", MachineId::random()),
    )
    .start()
    .await
    .unwrap();
    let store = running.store().clone();
    let mut volume_changes = store
        .api()
        .subscribe(Statement::new(
            "SELECT machine_id, name, volume FROM volumes",
            [],
        ))
        .await
        .unwrap();

    let kept = machine("kept", 1);
    let gone = machine("gone", 2);
    let kept_volume = volume(&kept.id, "data");
    let gone_volume = volume(&gone.id, "data");
    store.publish_volume(&kept_volume).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), volume_changes.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.volume(&kept_volume.id).await.unwrap().as_ref(),
        Some(&kept_volume)
    );
    assert_eq!(
        store.volumes().await.unwrap().observations,
        vec![kept_volume.clone()]
    );

    store.publish_volume(&gone_volume).await.unwrap();
    let incomplete_id = DockerVolumeId {
        machine_id: gone.id,
        name: DockerVolumeName::parse("incomplete").unwrap(),
    };
    running
        .store()
        .api()
        .execute([Statement::new(
            "INSERT INTO volumes (machine_id, name) VALUES (?, ?)",
            [json!(&incomplete_id.machine_id), json!(&incomplete_id.name)],
        )])
        .await
        .unwrap();

    let mut encoded = serde_json::to_value(&kept_volume).unwrap();
    encoded
        .as_object_mut()
        .unwrap()
        .insert("quota_bytes".into(), json!(1073741824));
    running
        .store()
        .api()
        .execute([Statement::new(
            "UPDATE volumes SET volume = ? WHERE machine_id = ? AND name = ?",
            [
                json!(encoded.to_string()),
                json!(&kept_volume.id.machine_id),
                json!(&kept_volume.id.name),
            ],
        )])
        .await
        .unwrap();

    let volumes = store.volumes().await.unwrap();
    assert!(volumes.observations.contains(&kept_volume));
    assert!(volumes.observations.contains(&gone_volume));
    assert_eq!(volumes.observations.len(), 2);
    assert_eq!(volumes.incomplete_ids, vec![incomplete_id.clone()]);
    assert_eq!(
        store.volume(&kept_volume.id).await.unwrap().as_ref(),
        Some(&kept_volume)
    );
    assert!(store.volume(&incomplete_id).await.unwrap().is_none());

    store.remove_machine(&gone.id).await.unwrap();
    let volumes = store.volumes().await.unwrap();
    assert_eq!(volumes.observations, vec![kept_volume.clone()]);
    assert!(volumes.incomplete_ids.is_empty());
    assert!(store.volume(&gone_volume.id).await.unwrap().is_none());
    assert!(store.volume(&incomplete_id).await.unwrap().is_none());

    running.cleanup().await.unwrap();
}

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ployzd-corrosion-{}",
            ployz_core::MachineId::random()
        ));
        Self(path)
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unused_address() -> std::net::SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

fn write_record(data_dir: &Path, record: &LocalMachineRecord) {
    fs::create_dir_all(data_dir).unwrap();
    fs::write(
        data_dir.join("machine.json"),
        serde_json::to_vec(record).unwrap(),
    )
    .unwrap();
}

fn hex_bytes(actor: &str) -> Vec<u8> {
    actor
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn machine(name: &str, seed: u8) -> Machine {
    Machine {
        id: MachineId::random(),
        name: MachineName::parse(name).unwrap(),
        subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
        management_address: ManagementAddress(format!("fdcc::{seed}").parse().unwrap()),
        public_key: WireGuardPublicKey([seed; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint(
            format!("192.0.2.{seed}:51000").parse().unwrap(),
        )],
        runtime: Default::default(),
    }
}

fn volume(machine_id: &MachineId, name: &str) -> DockerVolume {
    DockerVolume {
        id: DockerVolumeId {
            machine_id: *machine_id,
            name: DockerVolumeName::parse(name).unwrap(),
        },
        driver: "local".into(),
        options: BTreeMap::from([("type".into(), "none".into())]),
        labels: BTreeMap::from([("purpose".into(), "database".into())]),
    }
}

fn container(machine_id: &MachineId, suffix: &str) -> ContainerObservation {
    serde_json::from_value(json!({
        "container_id": format!("{suffix:0<64}"),
        "display_name": format!("service-{suffix}"),
        "machine_id": machine_id,
        "project_name": "app",
        "service_id": format!("{suffix:0<32}"),
        "service_name": "service",
        "kind": "service_container",
        "runtime": { "state": "created" },
        "resolved_spec": {
            "service_id": format!("{suffix:0<32}"),
            "name": "service",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "busybox", "pull_policy": "missing" }
        }
    }))
    .unwrap()
}
