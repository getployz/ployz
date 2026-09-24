use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
    sync::Arc,
    time::Duration,
};

use ployz::{
    connect::{SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
    sdk::Session,
};
use ployz_core::{DeployIntent, DeployOutcome, ListMachinesRequest, op};
use ployz_testkit::{Cluster, ClusterPlan, SERVICE_CONTAINER_IMAGE};

/// L3-009..L3-010, L3-014, and L3-045..L3-046.
#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image"]
async fn deploy_scale_and_rename_execute_through_the_sdk_and_cli() {
    let plan = ClusterPlan::new(&format!("l3-workflows-{}", std::process::id()), 2).unwrap();
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
    let session = session(address).await;
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
    let on_machine_1 = format!("node.id=={machine_1}");

    let scaled = intent("scaled-workflow", &on_machine_1, None);
    let outcome = session.run(scaled.clone(), None).await.unwrap();
    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    wait_for_services(&mut client, &["scaled-workflow"], 1).await;
    // An unchanged Deploy Intent plans nothing, so its Containers stay.
    let unchanged = session.preview(scaled).await.unwrap();
    assert!(unchanged.noop(), "{:?}", unchanged.preview());
    unchanged.close();
    assert_success(ployz(address, ["scale", "--yes", "scaled-workflow", "2"]));

    let initial_run = wait_for_services(&mut client, &["scaled-workflow"], 2).await;
    let scaled = observed_service(&initial_run, "scaled-workflow");
    assert_eq!(scaled.containers.len(), 2);
    assert!(
        scaled
            .containers
            .iter()
            .all(|container| &container.as_observation().machine_id == machine_1)
    );
    assert_machine_rename_preserves_containers(address, &mut client, machine_1, &initial_run).await;

    let blocked = session
        .run(intent("blocked", &on_machine_1, Some("exit 17")), None)
        .await
        .unwrap();
    assert!(
        matches!(blocked, DeployOutcome::Failed { .. }),
        "{blocked:?}"
    );
    let blocked = wait_for_hook_only(&mut client, "blocked").await;
    assert!(blocked.containers.is_empty());
    assert!(!blocked.hook_containers.is_empty());
    session.close().await;
}

/// #474: both successful removal paths warn instead of silently losing replicas.
#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image"]
async fn machine_rm_warns_when_replicated_services_are_left_under_replicated() {
    for no_reset in [false, true] {
        let plan = ClusterPlan::new(
            &format!("l3-machine-rm-warning-{no_reset}-{}", std::process::id()),
            2,
        )
        .unwrap();
        let cluster = Cluster::create(plan).unwrap();
        cluster.initialize_two().await.unwrap();
        let address = cluster.api_socket_address(0).unwrap();

        assert_success(ployz(
            address,
            [
                "machine",
                "update",
                "machine-2",
                "--label-add",
                "fixture=machine-2",
            ],
        ));

        let session = session(address).await;
        let outcome = session
            .run(
                intent("replicated", "node.labels.fixture==machine-2", None),
                None,
            )
            .await
            .unwrap();
        assert!(
            matches!(outcome, DeployOutcome::Success { .. }),
            "{outcome:?}"
        );
        session.close().await;

        let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
        command
            .args(["--connect", &format!("tcp://{address}"), "machine", "rm"])
            .args(no_reset.then_some("--no-reset"))
            .args(["--yes", "machine-2"]);
        let removed = command.output().unwrap();
        assert!(
            removed.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&removed.stdout),
            String::from_utf8_lossy(&removed.stderr)
        );
        let stderr = String::from_utf8_lossy(&removed.stderr);
        assert!(
            stderr.contains("Replicated Services may now be under-replicated: workflow/replicated"),
            "{stderr}"
        );
        assert!(
            stderr.contains("Replicas are not re-placed automatically."),
            "{stderr}"
        );
    }
}

async fn session(address: std::net::SocketAddr) -> Session {
    ployz::sdk::connect_connections(
        vec![Connection::tcp(address)],
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap()
}

/// One image Service in Project `workflow`, lowered as Cloud authors it.
/// Cloud authors no placement; these scenarios pin one to observe it.
fn intent(name: &str, constraint: &str, pre_deploy: Option<&str>) -> DeployIntent {
    let mut intent = ployz_core::config::lower_deployment(
        serde_json::from_value(
            serde_json::json!({"projectName": "workflow", "snapshots": [{
                "config": {"version": 2, "privateDns": name, "source": {
                    "type": "image", "version": 1, "image": SERVICE_CONTAINER_IMAGE,
                    "credentials": {"type": "none"}
                }, "startCommand": "sleep 60", "preDeployCommand": pre_deploy,
                "healthcheck": {"type": "none"}, "restartPolicy": "on-failure"}
            }]}),
        )
        .unwrap(),
    )
    .unwrap();
    intent.options.skip_health_monitor = true;
    intent
        .target
        .first_mut()
        .unwrap()
        .placement
        .constraints
        .insert(ployz_core::PlacementConstraint::parse(constraint).unwrap());
    intent
}

async fn assert_machine_rename_preserves_containers(
    address: std::net::SocketAddr,
    client: &mut ployz::connect::Client,
    machine_id: &ployz_core::MachineId,
    initial: &ployz_core::LiveServices<ployz_core::RpcError>,
) {
    let initial_ids = initial
        .services()
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
    let after_rename = wait_for_services(client, &["scaled-workflow"], 2).await;
    assert_eq!(
        after_rename
            .services()
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

fn observed_service<E>(
    live: &ployz_core::LiveServices<E>,
    name: &str,
) -> ployz_core::ServiceObservation {
    live.services()
        .into_iter()
        .find(|service| service.has_name(name))
        .unwrap()
}

async fn wait_for_services(
    client: &mut ployz::connect::Client,
    names: &[&str],
    regular_containers: usize,
) -> ployz_core::LiveServices<ployz_core::RpcError> {
    wait_for_live(client, |live| {
        let services = live.services();
        let observed = services
            .iter()
            .flat_map(|service| &service.containers)
            .map(|container| container.as_observation().resolved_spec.name.as_str())
            .collect::<BTreeSet<_>>();
        let count = services
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
        live.services().into_iter().find(|service| {
            service.containers.is_empty()
                && service.hook_containers.first().is_some_and(|container| {
                    container.as_observation().resolved_spec.name.as_str() == name
                })
        })
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
        .env("PLOYZ_HEALTH_MONITOR_PERIOD", "0s")
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
