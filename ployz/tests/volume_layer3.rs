use std::{
    collections::BTreeMap,
    process::{Command, Output},
    sync::Arc,
};

use ployz::{
    connect::{SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
    volume::{filter_volumes, machine_volumes},
};
use ployz_core::{
    ContainerKind, CreateVolumeRequest, DockerVolumeName, ListMachinesRequest, MachineTarget,
    NameMatches, ProjectName, ResolvedServiceSpec, ServiceId, op,
};
use ployz_testkit::{Cluster, ClusterPlan};
use serde_json::json;

/// L3-008, L3-013, L3-047..L3-055, L3-067, and the machine-local-volume negative family.
#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn volume_cli_mounts_and_partial_results_stay_machine_local() {
    let plan = ClusterPlan::new(&format!("l3-volume-product-{}", std::process::id()), 2).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.initialize_two().await.unwrap();
    let address = cluster.api_socket_address(0).unwrap();

    for (machine, label) in [("machine-1", "one"), ("machine-2", "two")] {
        let output = ployz(
            address,
            [
                "volume",
                "create",
                "shared",
                "--machine",
                machine,
                "--label",
                &format!("site={label}"),
            ],
        );
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let listed = ployz(address, ["volume", "ls"]);
    assert!(listed.status.success());
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout)
            .matches("shared")
            .count(),
        2
    );
    for selector in ["*", "missing-machine"] {
        let create = ployz(
            address,
            ["volume", "create", "invalid", "--machine", selector],
        );
        assert!(!create.status.success());
    }
    assert!(!String::from_utf8_lossy(&ployz(address, ["volume", "ls"]).stdout).contains("invalid"));
    let ambiguous = ployz(address, ["volume", "inspect", "shared"]);
    assert!(!ambiguous.status.success());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("ambiguous"));
    let qualified = ployz(
        address,
        ["volume", "inspect", "shared", "--machine", "machine-1"],
    );
    assert!(qualified.status.success());
    assert!(String::from_utf8_lossy(&qualified.stdout).contains("\"site\": \"one\""));

    let mixed_remove = ployz(
        address,
        [
            "volume",
            "rm",
            "shared",
            "missing",
            "--machine",
            "machine-1",
            "--yes",
        ],
    );
    assert!(!mixed_remove.status.success());
    assert_eq!(
        String::from_utf8_lossy(&ployz(address, ["volume", "ls"]).stdout)
            .matches("shared")
            .count(),
        2
    );

    let cancelled = ployz(
        address,
        ["volume", "rm", "shared", "--machine", "machine-1"],
    );
    assert!(!cancelled.status.success());
    assert_eq!(
        String::from_utf8_lossy(&ployz(address, ["volume", "ls"]).stdout)
            .matches("shared")
            .count(),
        2
    );

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
                ProjectName::parse("app").unwrap(),
                mount_spec(index, &shared),
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
                ProjectName::parse("app").unwrap(),
                mount_spec(9, &missing),
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
            .iter()
            .any(|volume| volume.id.name == missing)
    );

    let mut remove = Command::new(env!("CARGO_BIN_EXE_ployz"));
    remove
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "volume",
            "rm",
            "shared",
            "--machine",
            "machine-1",
        ])
        .env("PLOYZ_AUTO_CONFIRM", "true");
    let removed = remove.output().unwrap();
    assert!(
        removed.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&removed.stdout),
        String::from_utf8_lossy(&removed.stderr)
    );
    let remaining = machine_volumes(&machines, &client.list_volumes(&machines).await);
    assert!(matches!(
        NameMatches::from_matches(filter_volumes(
            &remaining,
            std::slice::from_ref(&shared),
        )),
        NameMatches::One(volume) if volume.machine_name.as_str() == "machine-2"
    ));

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
            .iter()
            .any(|volume| volume.id.name.as_str() == "reachable")
    );
    let [failure] = partial.failures.as_slice() else {
        panic!("expected one failed target: {partial:?}")
    };
    assert_eq!(failure.machine_id, second_machine.machine.id);

    let partial_inspect = ployz(address, ["volume", "inspect", "reachable"]);
    assert!(!partial_inspect.status.success());

    let partial_remove = ployz(address, ["volume", "rm", "reachable", "--yes"]);
    assert!(!partial_remove.status.success());
    let reachable = client
        .list_volumes(std::slice::from_ref(first_machine))
        .await;
    let [success] = reachable.successes.as_slice() else {
        panic!("expected reachable Machine after partial removal: {reachable:?}")
    };
    assert!(
        !success
            .value
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
            {"reference":"alias","source":{"kind":"named","name":name}},
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

fn ployz<const N: usize>(address: std::net::SocketAddr, args: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["--connect", &format!("tcp://{address}")])
        .args(args)
        .output()
        .unwrap()
}
