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
use ployz_core::{ContainerKind, PortPublication};
use ployz_testkit::{Cluster, ClusterPlan, SERVICE_CONTAINER_IMAGE};

/// L3-005..L3-007, L3-009..L3-010, L3-014, L3-040..L3-041, and L3-045..L3-046.
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
    let machines = client.list_machines().await.unwrap();
    let machine_ids = machines
        .iter()
        .map(|machine| (machine.machine.name.to_string(), machine.machine.id.clone()))
        .collect::<BTreeMap<_, _>>();
    let machine_1 = machine_ids.get("machine-1").unwrap();
    let machine_2 = machine_ids.get("machine-2").unwrap();
    let initial_run = wait_for_services(&mut client, &["scaled-workflow"], 3).await;
    let scaled = initial_run
        .services
        .iter()
        .find(|service| {
            service
                .containers
                .first()
                .is_some_and(|container| container.service_name.as_str() == "scaled-workflow")
        })
        .unwrap();
    assert_eq!(scaled.containers.len(), 2);
    assert!(
        scaled
            .containers
            .iter()
            .all(|container| &container.machine_id == machine_1)
    );
    let generated_global = initial_run
        .services
        .iter()
        .find(|service| {
            service
                .containers
                .first()
                .is_some_and(|container| container.service_name.as_str().starts_with("alpine-"))
        })
        .unwrap();
    assert_eq!(generated_global.containers.len(), 1);
    assert_eq!(
        &generated_global.containers.first().unwrap().machine_id,
        machine_1
    );

    let root = std::env::temp_dir().join(format!("ployz-l3-workflows-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("message.txt"), "hello\n").unwrap();
    fs::write(
        root.join("missing.yaml"),
        format!(
            "services:\n  impossible:\n    image: {SERVICE_CONTAINER_IMAGE}\n    x-machines: [missing-machine]\n"
        ),
    )
    .unwrap();
    let impossible = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "deploy",
            "--file",
            "missing.yaml",
            "--no-build",
            "--yes",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
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

    let declined = deploy(address, &root, false, false);
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

    assert_success(deploy(address, &root, true, false));
    let first_ids = wait_for_ids(&mut client, &["api", "database"], None).await;
    assert_success(deploy(address, &root, true, false));
    let unchanged_ids = wait_for_ids(&mut client, &["api", "database"], None).await;
    assert_eq!(unchanged_ids, first_ids);
    assert_success(deploy(address, &root, true, true));
    let recreated_ids = wait_for_ids(&mut client, &["api", "database"], Some(&first_ids)).await;
    assert_ne!(recreated_ids, first_ids);

    let deployed = wait_for_services(&mut client, &["api", "database", "scaled-workflow"], 5).await;
    let api = deployed
        .services
        .iter()
        .find(|service| {
            service
                .containers
                .first()
                .is_some_and(|container| container.service_name.as_str() == "api")
        })
        .unwrap();
    let api_container = api.containers.first().unwrap();
    assert_eq!(api.containers.len(), 1);
    assert_eq!(&api_container.machine_id, machine_2);
    assert!(!api.hook_containers.is_empty());
    assert!(
        api.hook_containers
            .iter()
            .all(|container| container.kind == ContainerKind::PreDeployHook)
    );
    assert_eq!(
        api_container.resolved_spec.configs.first().unwrap().content,
        b"hello\n"
    );
    assert!(matches!(
        api_container.resolved_spec.ports.as_slice(),
        [PortPublication::Host { .. }]
    ));
    let database = deployed
        .services
        .iter()
        .find(|service| {
            service
                .containers
                .first()
                .is_some_and(|container| container.service_name.as_str() == "database")
        })
        .unwrap();
    assert_eq!(&database.containers.first().unwrap().machine_id, machine_1);
    let current_machines = client.list_machines().await.unwrap();
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
    yes: bool,
    recreate: bool,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
    command
        .args(["--connect", &format!("tcp://{address}"), "deploy"])
        .args(recreate.then_some("--recreate"))
        .args(["--no-build"])
        .args(yes.then_some("--yes"))
        .current_dir(root)
        .env_remove("PLOYZ_AUTO_CONFIRM");
    command.output().unwrap()
}

async fn wait_for_services(
    client: &mut ployz::connect::Client,
    names: &[&str],
    regular_containers: usize,
) -> ployz_core::LiveServices<ployz_core::RpcError> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(live) = client.live_services().await {
                let observed = live
                    .services
                    .iter()
                    .flat_map(|service| &service.containers)
                    .map(|container| container.service_name.as_str())
                    .collect::<BTreeSet<_>>();
                let count = live
                    .services
                    .iter()
                    .map(|service| service.containers.len())
                    .sum::<usize>();
                if names.iter().all(|name| observed.contains(name)) && count == regular_containers {
                    return live;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap()
}

async fn service_ids(
    client: &mut ployz::connect::Client,
    names: &[&str],
) -> BTreeMap<String, BTreeSet<String>> {
    client
        .live_services()
        .await
        .unwrap()
        .services
        .into_iter()
        .filter_map(|service| {
            let name = service.containers.first()?.service_name.to_string();
            names.contains(&name.as_str()).then(|| {
                (
                    name,
                    service
                        .containers
                        .into_iter()
                        .map(|container| container.container_id.to_string())
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
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let ids = service_ids(client, names).await;
            if ids.len() == names.len() && different_from.is_none_or(|old| old != &ids) {
                return ids;
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
