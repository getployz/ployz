use std::{
    collections::BTreeMap,
    num::NonZeroU32,
    process::{Command, Output},
    sync::Arc,
};

use ployz::{
    compose::{parse_normalized, plan_compose_deploy},
    connect::{SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
    deploy::{
        DeployOperation, DeploySnapshot, ObservedDockerVolume, PlanError, PlanOptions, plan_deploy,
    },
    volume::{filter_volumes, machine_volumes},
};
use ployz_core::{
    CreateVolumeRequest, DockerVolumeName, MachineObservation, NameMatches, RequestedServiceSpec,
    ResolvedServiceSpec, ServiceId, ServiceMode, VolumeSource,
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
    let machines = client.list_machines().await.unwrap();
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
            .create_service_container(&machine.machine.id, mount_spec(index, &shared))
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
            .create_service_container(&first_machine.machine.id, mount_spec(9, &missing))
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

    assert_mapped_plans_execute(&mut client, &machines).await;

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
        .create_volume(
            &first_machine.machine.id,
            CreateVolumeRequest {
                name: DockerVolumeName::parse("reachable").unwrap(),
                driver: "local".into(),
                options: BTreeMap::new(),
                labels: BTreeMap::new(),
            },
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

async fn assert_mapped_plans_execute(
    client: &mut ployz::connect::Client,
    machines: &[MachineObservation],
) {
    let [first_machine, second_machine] = machines else {
        panic!("expected two Machines: {machines:?}")
    };
    let compose = parse_normalized(
        r#"
name: layer3
services:
  first: {image: alpine:3.23.3, volumes: [{type: volume, source: data, target: /first}]}
  second: {image: alpine:3.23.3, volumes: [{type: volume, source: data, target: /second}]}
volumes: {data: {name: compose_data}}
"#,
        ".",
    )
    .unwrap();
    let plan = plan_compose_deploy(
        &compose,
        &live_snapshot(client, machines).await,
        PlanOptions::default(),
    )
    .unwrap();
    assert_eq!(plan.volume_operations.len(), 1);
    execute(client, plan.operations()).await;

    let replicated = requested(
        "replicated",
        ServiceMode::Replicated {
            replicas: NonZeroU32::new(3).unwrap(),
        },
        &["auto_data"],
    );
    let plan = plan_deploy(
        &replicated,
        &live_snapshot(client, machines).await,
        ServiceId::random(),
        PlanOptions::default(),
    )
    .unwrap();
    assert!(matches!(
        plan.operations().first(),
        Some(DeployOperation::CreateVolume { .. })
    ));
    assert_eq!(plan.operations().len(), 4);
    execute(client, plan.operations().iter().collect()).await;

    client
        .create_volume(
            &first_machine.machine.id,
            CreateVolumeRequest {
                name: DockerVolumeName::parse("multi_existing").unwrap(),
                driver: "local".into(),
                options: BTreeMap::new(),
                labels: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
    let multiple_with_one_missing = requested(
        "multiple-with-one-missing",
        ServiceMode::Replicated {
            replicas: NonZeroU32::new(2).unwrap(),
        },
        &["multi_existing", "multi_missing"],
    );
    let plan = plan_deploy(
        &multiple_with_one_missing,
        &live_snapshot(client, machines).await,
        ServiceId::random(),
        PlanOptions::default(),
    )
    .unwrap();
    assert!(matches!(
        plan.operations(),
        [DeployOperation::CreateVolume { machine_id, volume }, rest @ ..]
            if machine_id == &first_machine.machine.id
                && matches!(&volume.source, VolumeSource::Named { name, .. } if name.as_str() == "multi_missing")
                && rest.iter().all(|operation| matches!(operation,
                    DeployOperation::RunContainer { machine_id, .. }
                        if machine_id == &first_machine.machine.id))
    ));
    execute(client, plan.operations().iter().collect()).await;

    let global = requested("global", ServiceMode::Global, &["global_data"]);
    let plan = plan_deploy(
        &global,
        &live_snapshot(client, machines).await,
        ServiceId::random(),
        PlanOptions::default(),
    )
    .unwrap();
    assert_eq!(
        plan.operations()
            .iter()
            .filter(|operation| matches!(operation, DeployOperation::CreateVolume { .. }))
            .count(),
        2
    );
    execute(client, plan.operations().iter().collect()).await;

    client
        .create_volume(
            &first_machine.machine.id,
            CreateVolumeRequest {
                name: DockerVolumeName::parse("global_partial").unwrap(),
                driver: "local".into(),
                options: BTreeMap::new(),
                labels: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
    let global_partial = requested("global-partial", ServiceMode::Global, &["global_partial"]);
    let plan = plan_deploy(
        &global_partial,
        &live_snapshot(client, machines).await,
        ServiceId::random(),
        PlanOptions::default(),
    )
    .unwrap();
    assert!(matches!(
        plan.operations().first(),
        Some(DeployOperation::CreateVolume { machine_id, .. })
            if machine_id == &second_machine.machine.id
    ));
    assert_eq!(
        plan.operations()
            .iter()
            .filter(|operation| matches!(operation, DeployOperation::CreateVolume { .. }))
            .count(),
        1
    );
    execute(client, plan.operations().iter().collect()).await;

    let global_existing = requested("global-existing", ServiceMode::Global, &["global_data"]);
    let plan = plan_deploy(
        &global_existing,
        &live_snapshot(client, machines).await,
        ServiceId::random(),
        PlanOptions::default(),
    )
    .unwrap();
    assert!(
        plan.operations()
            .iter()
            .all(|operation| !matches!(operation, DeployOperation::CreateVolume { .. }))
    );
    execute(client, plan.operations().iter().collect()).await;

    for (name, machine) in [
        ("intersect_a", first_machine),
        ("intersect_b", first_machine),
        ("split_a", first_machine),
        ("split_b", second_machine),
    ] {
        client
            .create_volume(
                &machine.machine.id,
                CreateVolumeRequest {
                    name: DockerVolumeName::parse(name).unwrap(),
                    driver: "local".into(),
                    options: BTreeMap::new(),
                    labels: BTreeMap::new(),
                },
            )
            .await
            .unwrap();
    }
    let snapshot = live_snapshot(client, machines).await;
    let intersection = requested(
        "intersection",
        ServiceMode::Replicated {
            replicas: NonZeroU32::new(2).unwrap(),
        },
        &["intersect_a", "intersect_b"],
    );
    let plan = plan_deploy(
        &intersection,
        &snapshot,
        ServiceId::random(),
        PlanOptions::default(),
    )
    .unwrap();
    assert!(plan.operations().iter().all(|operation| matches!(
        operation,
        DeployOperation::RunContainer { machine_id, .. }
            if machine_id == &first_machine.machine.id
    )));
    execute(client, plan.operations().iter().collect()).await;

    let split = requested(
        "split",
        ServiceMode::Replicated {
            replicas: NonZeroU32::new(2).unwrap(),
        },
        &["split_a", "split_b"],
    );
    assert_eq!(
        plan_deploy(
            &split,
            &snapshot,
            ServiceId::random(),
            PlanOptions::default(),
        ),
        Err(PlanError::NoEligibleMachines)
    );
}

async fn live_snapshot(
    client: &mut ployz::connect::Client,
    machines: &[MachineObservation],
) -> DeploySnapshot {
    let volumes = client
        .list_volumes(machines)
        .await
        .successes
        .into_iter()
        .flat_map(|success| success.value)
        .map(|volume| ObservedDockerVolume {
            id: volume.id,
            driver: volume.driver,
            options: volume.options,
        })
        .collect();
    DeploySnapshot {
        machines: machines.to_vec(),
        containers: Vec::new(),
        volumes,
    }
}

async fn execute(client: &mut ployz::connect::Client, operations: Vec<&DeployOperation>) {
    for operation in operations {
        match operation {
            DeployOperation::CreateVolume { machine_id, volume } => {
                let VolumeSource::Named {
                    name,
                    driver,
                    labels,
                    ..
                } = &volume.source
                else {
                    panic!("planner created a non-named Docker Volume")
                };
                client
                    .create_volume(
                        machine_id,
                        CreateVolumeRequest {
                            name: name.clone(),
                            driver: driver
                                .as_ref()
                                .map_or_else(|| "local".into(), |driver| driver.name.clone()),
                            options: driver
                                .as_ref()
                                .map_or_else(BTreeMap::new, |driver| driver.options.clone()),
                            labels: labels.clone(),
                        },
                    )
                    .await
                    .unwrap();
            }
            DeployOperation::RunContainer {
                machine_id, spec, ..
            } => {
                client
                    .create_service_container(machine_id, spec.clone())
                    .await
                    .unwrap();
            }
            operation @ (DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::ReplaceContainer(_)
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::Sequence { .. }) => {
                panic!("new-deploy test received {operation:?}")
            }
        }
    }
}

fn requested(name: &str, mode: ServiceMode, names: &[&str]) -> RequestedServiceSpec {
    let volumes = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            json!({"reference":format!("volume-{index}"),"source":{"kind":"named","name":name}})
        })
        .collect::<Vec<_>>();
    let mounts = names
        .iter()
        .enumerate()
        .map(|(index, _)| {
            json!({"volume":format!("volume-{index}"),"target":format!("/data-{index}")})
        })
        .collect::<Vec<_>>();
    serde_json::from_value(json!({
        "name": name,
        "mode": mode,
        "container": { "image": "alpine:3.23.3", "command": ["sleep", "60"], "pull_policy": "missing" },
        "volumes": volumes,
        "mounts": mounts
    }))
    .unwrap()
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
