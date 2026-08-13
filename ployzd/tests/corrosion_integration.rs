use std::{net::TcpListener, time::Duration};

use ployz_core::{
    AdvertisedEndpoint, ContainerObservation, Machine, MachineId, MachineName, MachineSubnet,
    ManagementAddress, WireGuardPublicKey,
};
use ployzd::corrosion::{CorrosionConfig, ReplicatedStore, Statement, wait_for_catch_up};
use serde_json::json;

use test_dir::TestDir;

mod test_dir;

#[tokio::test]
#[ignore = "requires Docker and the pinned Corrosion image"]
async fn replicated_store_preserves_partial_and_contradictory_observations() {
    let root = TestDir::new("ployzd-corrosion");
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
    let store = ReplicatedStore::new(running.api().clone());
    running
        .admin()
        .command(&json!({"Cluster": "MembershipStates"}))
        .await
        .unwrap();

    let local = machine("duplicate", 1);
    store.publish_local_machine(&local).await.unwrap();
    store.publish_local_machine(&local).await.unwrap();

    let duplicate = machine("duplicate", 2);
    store.publish_local_machine(&duplicate).await.unwrap();
    running
        .api()
        .execute([Statement::new(
            "INSERT INTO machines (id) VALUES (?)",
            [json!(MachineId::random())],
        )])
        .await
        .unwrap();

    let machines = store.machines().await.unwrap();
    assert_eq!(machines.observations.len(), 2);
    assert_eq!(machines.incomplete_ids.len(), 1);
    assert!(machines.observations.contains(&local));
    assert!(machines.observations.contains(&duplicate));

    let synced = container(&local.id, "a");
    store.publish_container(&synced).await.unwrap();
    let unsynced = container(&local.id, "b");
    running
        .api()
        .execute([Statement::new(
            "INSERT INTO containers (id, container, machine_id, docker_sync_status) VALUES (?, ?, ?, '')",
            [
                json!(unsynced.container_id),
                json!(serde_json::to_string(&unsynced).unwrap()),
                json!(unsynced.machine_id),
            ],
        )])
        .await
        .unwrap();
    assert_eq!(
        store
            .containers()
            .await
            .unwrap()
            .observations
            .into_iter()
            .map(|record| record.observation)
            .collect::<Vec<_>>(),
        vec![synced]
    );

    let target = store.version().await.unwrap();
    assert!(store.known_missing_changes().await.unwrap().is_empty());
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

    running.cleanup().await.unwrap();
}

fn unused_address() -> std::net::SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

fn machine(name: &str, seed: u8) -> Machine {
    Machine {
        id: MachineId::random(),
        name: MachineName::parse(name).unwrap(),
        subnet: MachineSubnet(format!("10.210.{seed}.0/24").parse().unwrap()),
        management_address: ManagementAddress(format!("fdcc::{seed}").parse().unwrap()),
        public_key: WireGuardPublicKey([seed; 32]),
        advertised_endpoints: vec![AdvertisedEndpoint(
            format!("192.0.2.{seed}:51000").parse().unwrap(),
        )],
    }
}

fn container(machine_id: &MachineId, suffix: &str) -> ContainerObservation {
    serde_json::from_value(json!({
        "container_id": format!("{suffix:0<64}"),
        "display_name": format!("service-{suffix}"),
        "machine_id": machine_id,
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
