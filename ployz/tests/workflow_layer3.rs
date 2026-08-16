use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    process::Command,
    sync::Arc,
    time::Duration,
};

use ployz::{
    connect::{SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
};
use ployz_core::{ListMachinesRequest, PortPublication, op};
use ployz_testkit::{Cluster, ClusterPlan, SERVICE_CONTAINER_IMAGE};

/// L3-005..L3-007, L3-009..L3-010, L3-014, L3-029..L3-030,
/// L3-040..L3-041, and L3-045..L3-046.
#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image and Docker Compose"]
async fn run_deploy_and_scale_execute_through_the_real_cli() {
    let plan = ClusterPlan::new(&format!("l3-workflows-{}", std::process::id()), 2).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.initialize_two().await.unwrap();
    let address = cluster.api_socket_address(0).unwrap();

    assert_success(ployz(
        address,
        [
            "run",
            "--mode",
            "global",
            "--machine",
            "machine-1",
            SERVICE_CONTAINER_IMAGE,
            "sleep",
            "60",
        ],
    ));
    assert_success(ployz(
        address,
        [
            "run",
            "--name",
            "scaled-workflow",
            "--machine",
            "machine-1",
            SERVICE_CONTAINER_IMAGE,
            "sleep",
            "60",
        ],
    ));
    assert_success(ployz(address, ["scale", "--yes", "scaled-workflow", "2"]));

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
    let machine_ids = machines
        .iter()
        .map(|machine| (machine.machine.name.to_string(), machine.machine.id))
        .collect::<BTreeMap<_, _>>();
    let machine_1 = machine_ids.get("machine-1").unwrap();
    let machine_2 = machine_ids.get("machine-2").unwrap();
    let initial_run = wait_for_services(&mut client, &["scaled-workflow"], 3).await;
    let scaled = observed_service(&initial_run, "scaled-workflow");
    assert_eq!(scaled.containers.len(), 2);
    assert!(
        scaled
            .containers
            .iter()
            .all(|container| &container.as_observation().machine_id == machine_1)
    );
    let generated_global = initial_run
        .services
        .iter()
        .find(|service| {
            service.containers.first().is_some_and(|container| {
                container
                    .as_observation()
                    .service_name
                    .as_str()
                    .starts_with("alpine-")
            })
        })
        .unwrap();
    assert_eq!(generated_global.containers.len(), 1);
    assert_eq!(
        &generated_global
            .containers
            .first()
            .unwrap()
            .as_observation()
            .machine_id,
        machine_1
    );
    assert_machine_rename_preserves_containers(address, &mut client, machine_1, &initial_run).await;

    let root = std::env::temp_dir().join(format!("ployz-l3-workflows-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("message.txt"), "hello\n").unwrap();
    fs::write(
        root.join("basic.yaml"),
        format!(
            "services:\n  basic:\n    image: {SERVICE_CONTAINER_IMAGE}\n    command: [sleep, '60']\n    x-ports: [18081:8080/tcp@host]\n"
        ),
    )
    .unwrap();
    let basic = deploy(address, &root, "basic.yaml", true, false);
    assert_success(basic);
    wait_for_ids(&mut client, &["basic"], None).await;

    fs::write(
        root.join("blocked.yaml"),
        format!(
            "services:\n  blocked:\n    image: {SERVICE_CONTAINER_IMAGE}\n    command: [sleep, '60']\n    x-pre_deploy: {{command: [sh, -c, 'exit 17']}}\n"
        ),
    )
    .unwrap();
    let blocked = deploy(address, &root, "blocked.yaml", true, false);
    assert!(!blocked.status.success());
    let blocked = wait_for_hook_only(&mut client, "blocked").await;
    assert!(blocked.containers.is_empty());
    assert!(!blocked.hook_containers.is_empty());

    fs::write(
        root.join("missing.yaml"),
        format!(
            "services:\n  impossible:\n    image: {SERVICE_CONTAINER_IMAGE}\n    x-machines: [missing-machine]\n"
        ),
    )
    .unwrap();
    let impossible = deploy(address, &root, "missing.yaml", true, false);
    assert!(!impossible.status.success());
    fs::write(
        root.join("compose.yaml"),
        format!(
            r#"name: workflow
services:
  database:
    image: {SERVICE_CONTAINER_IMAGE}
    command: [sleep, "60"]
    x-machines: [machine-1]
  api:
    image: {SERVICE_CONTAINER_IMAGE}
    command: [sleep, "60"]
    depends_on: [database]
    x-machines: [machine-2]
    x-ports: [18080:8080/tcp@host]
    x-pre_deploy: {{command: [sh, -c, "exit 0"]}}
    configs: [{{source: message, target: /message.txt}}]
    volumes: [{{type: volume, source: data, target: /data}}]
configs:
  message: {{file: message.txt}}
volumes:
  data: {{name: workflow_data}}
"#
        ),
    )
    .unwrap();

    let declined = deploy(address, &root, "compose.yaml", false, false);
    assert!(!declined.status.success());
    assert!(String::from_utf8_lossy(&declined.stderr).contains("confirmation requires a terminal"));
    assert!(
        service_ids(&mut client, &["api", "database"])
            .await
            .is_empty()
    );
    assert!(
        client
            .list_volumes(&machines)
            .await
            .successes
            .iter()
            .flat_map(|success| &success.value)
            .all(|volume| volume.id.name.as_str() != "data")
    );

    assert_success(deploy(address, &root, "compose.yaml", true, false));
    let first_ids = wait_for_ids(&mut client, &["api", "database"], None).await;
    assert_success(deploy(address, &root, "compose.yaml", true, false));
    let unchanged_ids = wait_for_ids(&mut client, &["api", "database"], None).await;
    assert_eq!(unchanged_ids, first_ids);
    assert_success(deploy(address, &root, "compose.yaml", true, true));
    let recreated_ids = wait_for_ids(&mut client, &["api", "database"], Some(&first_ids)).await;
    assert_ne!(recreated_ids, first_ids);

    let deployed = wait_for_services(
        &mut client,
        &["api", "basic", "database", "scaled-workflow"],
        6,
    )
    .await;
    let api = observed_service(&deployed, "api");
    let api_container = api.containers.first().unwrap().as_observation();
    assert_eq!(api.containers.len(), 1);
    assert_eq!(&api_container.machine_id, machine_2);
    assert!(!api.hook_containers.is_empty());
    assert_eq!(
        api_container.resolved_spec.configs.first().unwrap().content,
        b"hello\n"
    );
    assert!(matches!(
        api_container.resolved_spec.ports.as_slice(),
        [PortPublication::Host { .. }]
    ));
    let database = observed_service(&deployed, "database");
    assert_eq!(
        &database
            .containers
            .first()
            .unwrap()
            .as_observation()
            .machine_id,
        machine_1
    );
    let current_machines = client
        .call::<op::ListMachines>(ListMachinesRequest {}, None)
        .await
        .unwrap()
        .machines;
    let volumes = client.list_volumes(&current_machines).await;
    assert!(
        volumes
            .successes
            .iter()
            .flat_map(|success| &success.value)
            .any(|volume| volume.id.name.as_str() == "data"),
        "{volumes:?}"
    );
    fs::remove_dir_all(root).unwrap();
}

fn deploy(
    address: std::net::SocketAddr,
    root: &std::path::Path,
    file: &str,
    yes: bool,
    recreate: bool,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
    command
        .args(["--connect", &format!("tcp://{address}"), "deploy"])
        .args(["--file", file])
        .args(recreate.then_some("--recreate"))
        .args(["--no-build"])
        .args(yes.then_some("--yes"))
        .current_dir(root)
        .env_remove("PLOYZ_AUTO_CONFIRM");
    command.output().unwrap()
}

async fn assert_machine_rename_preserves_containers(
    address: std::net::SocketAddr,
    client: &mut ployz::connect::Client,
    machine_id: &ployz_core::MachineId,
    initial: &ployz_core::LiveServices<ployz_core::RpcError>,
) {
    let initial_ids = initial
        .services
        .iter()
        .flat_map(|service| &service.containers)
        .map(|container| container.as_observation().container_id)
        .collect::<BTreeSet<_>>();
    assert!(
        !ployz(address, ["machine", "rename", "machine-1", ""])
            .status
            .success()
    );
    assert_success(ployz(
        address,
        ["machine", "rename", "machine-1", "workflow-renamed"],
    ));
    wait_for_machine_name(client, machine_id, "workflow-renamed").await;
    let after_rename = wait_for_services(client, &["scaled-workflow"], 3).await;
    assert_eq!(
        after_rename
            .services
            .iter()
            .flat_map(|service| &service.containers)
            .map(|container| container.as_observation().container_id)
            .collect::<BTreeSet<_>>(),
        initial_ids
    );
    assert_success(ployz(
        address,
        ["machine", "rename", machine_id.as_str(), "machine-1"],
    ));
    wait_for_machine_name(client, machine_id, "machine-1").await;
}

fn observed_service<'a, E>(
    live: &'a ployz_core::LiveServices<E>,
    name: &str,
) -> &'a ployz_core::ServiceObservation {
    live.services
        .iter()
        .find(|service| service.has_name(name))
        .unwrap()
}

async fn wait_for_services(
    client: &mut ployz::connect::Client,
    names: &[&str],
    regular_containers: usize,
) -> ployz_core::LiveServices<ployz_core::RpcError> {
    wait_for_live(client, |live| {
        let observed = live
            .services
            .iter()
            .flat_map(|service| &service.containers)
            .map(|container| container.as_observation().service_name.as_str())
            .collect::<BTreeSet<_>>();
        let count = live
            .services
            .iter()
            .map(|service| service.containers.len())
            .sum::<usize>();
        (names.iter().all(|name| observed.contains(name)) && count == regular_containers)
            .then_some(live)
    })
    .await
}

async fn wait_for_hook_only(
    client: &mut ployz::connect::Client,
    name: &str,
) -> ployz_core::ServiceObservation {
    wait_for_live(client, |live| {
        live.services.into_iter().find(|service| {
            service.containers.is_empty()
                && service.hook_containers.first().is_some_and(|container| {
                    container.as_observation().service_name.as_str() == name
                })
        })
    })
    .await
}

async fn service_ids(
    client: &mut ployz::connect::Client,
    names: &[&str],
) -> BTreeMap<String, BTreeSet<String>> {
    service_ids_from(client.live_services().await.unwrap(), names)
}

fn service_ids_from(
    live: ployz_core::LiveServices<ployz_core::RpcError>,
    names: &[&str],
) -> BTreeMap<String, BTreeSet<String>> {
    live.services
        .into_iter()
        .filter_map(|service| {
            let name = service
                .containers
                .first()?
                .as_observation()
                .service_name
                .to_string();
            names.contains(&name.as_str()).then(|| {
                (
                    name,
                    service
                        .containers
                        .into_iter()
                        .map(|container| container.as_observation().container_id.to_string())
                        .collect(),
                )
            })
        })
        .collect()
}

async fn wait_for_ids(
    client: &mut ployz::connect::Client,
    names: &[&str],
    different_from: Option<&BTreeMap<String, BTreeSet<String>>>,
) -> BTreeMap<String, BTreeSet<String>> {
    wait_for_live(client, |live| {
        let ids = service_ids_from(live, names);
        (ids.len() == names.len() && different_from.is_none_or(|old| old != &ids)).then_some(ids)
    })
    .await
}

async fn wait_for_machine_name(
    client: &mut ployz::connect::Client,
    id: &ployz_core::MachineId,
    name: &str,
) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if client
                .call::<op::ListMachines>(ListMachinesRequest {}, None)
                .await
                .is_ok_and(|list| {
                    list.machines.iter().any(|machine| {
                        &machine.machine.id == id && machine.machine.name.as_str() == name
                    })
                })
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_for_live<T>(
    client: &mut ployz::connect::Client,
    mut select: impl FnMut(ployz_core::LiveServices<ployz_core::RpcError>) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(live) = client.live_services().await
                && let Some(value) = select(live)
            {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap()
}

fn ployz<const N: usize>(address: std::net::SocketAddr, args: [&str; N]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["--connect", &format!("tcp://{address}")])
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: std::process::Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
