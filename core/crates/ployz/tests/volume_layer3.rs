use std::{collections::BTreeMap, sync::Arc};

use ployz::{
    connect::{SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
};
use ployz_core::{
    ContainerKind, CreateVolumeRequest, DockerVolumeId, DockerVolumeName, ListMachinesRequest,
    MachineTarget, Namespace, RemoveVolumesRequest, ResolvedServiceSpec, ServiceId,
    VolumeRemovalOutcome, op,
};
use ployz_testkit::{Cluster, ClusterPlan};
use serde_json::json;

/// L3-008, L3-013, L3-047..L3-055, L3-067, and the machine-local-volume negative family.
#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image"]
async fn volume_mounts_and_partial_results_stay_machine_local() {
    let plan = ClusterPlan::new(&format!("l3-volume-product-{}", std::process::id()), 2).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.initialize_two().await.unwrap();
    let address = cluster.api_socket_address(0).unwrap();

    let mut client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp(address)],
        },
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();
    let machines = client
        .call::<op::ListMachines>(ListMachinesRequest {}, None)
        .await
        .unwrap()
        .machines;
    let [first_machine, second_machine] = machines.as_slice() else {
        panic!("expected two Machines: {machines:?}")
    };
    let shared = DockerVolumeName::parse("shared").unwrap();
    for machine in &machines {
        create_volume(&mut client, &machine.machine.id, &shared).await;
    }
    let shared_ids = |volumes: &[ployz_core::MachineSuccess<ployz_core::VolumeInventory>]| {
        volumes
            .iter()
            .flat_map(|success| &success.value.volumes)
            .filter(|volume| volume.id.name == shared)
            .map(|volume| volume.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        shared_ids(&client.list_volumes(&machines).await.successes).len(),
        2
    );
    let mut mounted_containers = Vec::new();
    for (index, machine) in machines.iter().enumerate() {
        cluster
            .prepare_bind(index, "/tmp/ployz-bind", &format!("bind-{index}"))
            .unwrap();
        cluster
            .write_volume_data(index, &shared, &format!("volume-{index}"))
            .unwrap();
        let created = client
            .create_container(
                machine.machine.id,
                ContainerKind::ServiceContainer,
                Namespace::parse("app").unwrap(),
                mount_spec(index, &shared),
                None,
            )
            .await
            .unwrap();
        cluster
            .start_container(index, &created.container_id)
            .unwrap();
        assert_eq!(
            cluster
                .read_container_file(index, &created.container_id, "/data/value")
                .unwrap(),
            format!("volume-{index}")
        );
        assert_eq!(
            cluster
                .read_container_file(index, &created.container_id, "/host/value")
                .unwrap(),
            format!("bind-{index}")
        );
        let mounts = cluster
            .container_mounts(index, &created.container_id)
            .unwrap();
        for mount_type in ["bind", "volume", "tmpfs"] {
            assert!(
                mounts.contains(&format!("\"Type\":\"{mount_type}\"")),
                "{mounts}"
            );
        }
        mounted_containers.push((index, created.container_id));
    }
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    for (index, container_id) in &mounted_containers {
        assert_eq!(
            cluster
                .read_container_file(*index, container_id, "/data/value")
                .unwrap(),
            format!("volume-{index}")
        );
        assert!(
            cluster
                .read_container_file(1_usize - *index, container_id, "/data/value")
                .is_err(),
            "Service Container moved to the other Machine"
        );
        cluster.remove_container(*index, container_id).unwrap();
    }

    let missing = DockerVolumeName::parse("missing").unwrap();
    assert!(
        client
            .create_container(
                first_machine.machine.id,
                ContainerKind::ServiceContainer,
                Namespace::parse("app").unwrap(),
                mount_spec(9, &missing),
                None,
            )
            .await
            .is_err()
    );
    let listed_first = client
        .list_volumes(std::slice::from_ref(first_machine))
        .await;
    let [first_success] = listed_first.successes.as_slice() else {
        panic!("expected one successful target: {listed_first:?}")
    };
    assert!(
        !first_success
            .value
            .volumes
            .iter()
            .any(|volume| volume.id.name == missing)
    );

    let removed = client
        .remove_volumes(RemoveVolumesRequest {
            volumes: vec![DockerVolumeId {
                machine_id: first_machine.machine.id,
                name: shared.clone(),
            }],
            force: false,
        })
        .await
        .unwrap();
    assert!(
        matches!(removed.as_slice(), [removal] if removal.outcome == VolumeRemovalOutcome::Removed),
        "{removed:?}"
    );
    let remaining = shared_ids(&client.list_volumes(&machines).await.successes);
    assert_eq!(
        remaining.iter().map(|id| id.machine_id).collect::<Vec<_>>(),
        [second_machine.machine.id]
    );

    client
        .call::<op::CreateVolume>(
            CreateVolumeRequest {
                name: DockerVolumeName::parse("reachable").unwrap(),
                driver: "local".into(),
                options: BTreeMap::new(),
                labels: BTreeMap::new(),
            },
            Some(&MachineTarget::from(&first_machine.machine.id)),
        )
        .await
        .unwrap();
    cluster.stop(1).unwrap();
    let partial = client.list_volumes(&machines).await;
    let [success] = partial.successes.as_slice() else {
        panic!("expected one reachable target: {partial:?}")
    };
    assert!(
        success
            .value
            .volumes
            .iter()
            .any(|volume| volume.id.name.as_str() == "reachable")
    );
    let [failure] = partial.failures.as_slice() else {
        panic!("expected one failed target: {partial:?}")
    };
    assert_eq!(failure.machine_id, second_machine.machine.id);

    // The reachable Machine's Docker Volume goes; the other Machine is never asked.
    let partial_remove = client
        .remove_volumes(RemoveVolumesRequest {
            volumes: vec![DockerVolumeId {
                machine_id: first_machine.machine.id,
                name: DockerVolumeName::parse("reachable").unwrap(),
            }],
            force: false,
        })
        .await
        .unwrap();
    assert!(
        matches!(partial_remove.as_slice(), [removal] if removal.outcome == VolumeRemovalOutcome::Removed),
        "{partial_remove:?}"
    );
    let reachable = client
        .list_volumes(std::slice::from_ref(first_machine))
        .await;
    let [success] = reachable.successes.as_slice() else {
        panic!("expected reachable Machine after partial removal: {reachable:?}")
    };
    assert!(
        !success
            .value
            .volumes
            .iter()
            .any(|volume| volume.id.name.as_str() == "reachable")
    );

    cluster.teardown().unwrap();
}

fn mount_spec(index: usize, name: &DockerVolumeName) -> ResolvedServiceSpec {
    serde_json::from_value(json!({
        "service_id": ServiceId::random(),
        "name": format!("volume-{index}"),
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "alpine:3.23.3", "command": ["sleep", "60"], "pull_policy": "missing" },
        "volumes": [
            {"reference":"host","source":{"kind":"bind","machine_path":"/tmp/ployz-bind"}},
            {"reference":"alias","source":{"kind":"external","name":name}},
            {"reference":"memory","source":{"kind":"tmpfs","size_bytes":4096}}
        ],
        "mounts": [
            {"volume":"host","target":"/host"},
            {"volume":"alias","target":"/data"},
            {"volume":"memory","target":"/cache"}
        ]
    }))
    .unwrap()
}

async fn create_volume(
    client: &mut ployz::connect::Client,
    machine: &ployz_core::MachineId,
    name: &DockerVolumeName,
) {
    client
        .call::<op::CreateVolume>(
            CreateVolumeRequest {
                name: name.clone(),
                driver: "local".into(),
                options: BTreeMap::new(),
                labels: BTreeMap::new(),
            },
            Some(&MachineTarget::from(machine)),
        )
        .await
        .unwrap();
}
