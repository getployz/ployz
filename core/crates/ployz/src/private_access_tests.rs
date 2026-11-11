use ployz_core::MachineName;
use ployz_testkit::fake_fleet::{FakeFleet, Scenario};

use super::*;

fn two_servers() -> ployz_testkit::fake_fleet::FleetFile {
    serde_json::from_value(serde_json::json!({"machines": [
        {"name": "edge", "id": "1".repeat(32), "version": "0.4.0", "online": true, "containers": []},
        {"name": "web", "id": "2".repeat(32), "version": "0.4.0", "online": true, "containers": []},
    ]}))
    .unwrap()
}

async fn client_of(fleet: &FakeFleet) -> Client {
    let socket = fleet.socket(&MachineName::parse("edge").unwrap()).unwrap();
    crate::connect::connect_selected_with(
        crate::context::SelectedConnections {
            source: crate::context::ConnectionSource::Direct,
            connections: vec![crate::context::Connection::unix(socket).unwrap()],
        },
        std::sync::Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap()
}

fn sealed() -> Ciphertext {
    Ciphertext::from_bytes(vec![3; 48])
}

// Catches `on` leaving a Machine without a router, and `off` leaving one behind: one
// router per Machine, and none after.
#[tokio::test]
async fn on_runs_one_router_per_machine_and_off_removes_them_all() {
    let sockets = tempfile::tempdir().unwrap();
    let fleet = FakeFleet::start(two_servers(), Scenario::default(), sockets.path(), None).unwrap();
    let mut client = client_of(&fleet).await;
    let cancel = CancellationToken::new();

    let observed = observe_access(&mut client).await.unwrap();
    assert_eq!(observed.machines.len(), 2);
    assert!(observed.running.is_empty());

    let on = turn_on(&client, observed.machines.clone(), sealed(), None, &cancel).await;
    assert!(on.failures.is_empty(), "{:?}", on.failures);
    assert_eq!(on.machines.len(), 2);
    assert!(on.machines.iter().all(|machine| machine.router));
    let after = observe_access(&mut client).await.unwrap();
    assert_eq!(after.running.len(), 2);
    assert!(shown(&after).iter().all(|machine| machine.router));

    let off = turn_off(&client, observed.machines, &cancel).await;
    assert!(off.failures.is_empty(), "{:?}", off.failures);
    assert!(off.machines.iter().all(|machine| !machine.router));
    assert!(
        observe_access(&mut client)
            .await
            .unwrap()
            .running
            .is_empty()
    );
}

// Catches `on` quietly skipping a Machine whose daemon can't run the router, and `off`
// failing on one that has none to remove.
#[tokio::test]
async fn a_daemon_that_predates_the_route_is_named_on_on_and_has_nothing_to_remove_on_off() {
    let sockets = tempfile::tempdir().unwrap();
    let fleet = FakeFleet::start(two_servers(), Scenario::default(), sockets.path(), None).unwrap();
    let mut client = client_of(&fleet).await;
    let cancel = CancellationToken::new();
    let mut machines = observe_access(&mut client).await.unwrap().machines;
    let old = machines.get_mut(1).unwrap();
    old.supported = false;
    let old = old.clone();

    let on = turn_on(&client, machines.clone(), sealed(), None, &cancel).await;
    assert_eq!(on.machines.len(), 1);
    let [failure] = on.failures.as_slice() else {
        panic!("one Machine is named: {:?}", on.failures);
    };
    assert_eq!(failure.machine_id, old.id);
    assert_eq!(failure.error.code, RpcErrorCode::Unsupported);

    let off = turn_off(&client, machines, &cancel).await;
    assert!(off.failures.is_empty());
    assert_eq!(off.machines.len(), 2);
}

fn router_on(machine: char, runtime: ContainerRuntimeObservation) -> ServiceObservation {
    let identity = QualifiedService::system_tailscale();
    let service_id = ployz_core::ServiceId::parse("c".repeat(32)).unwrap();
    let observation = ployz_core::ContainerObservation::try_from(
        ployz_core::ContainerObservationParts {
            rotation: None,
            r#gen: None,
            container_id: ployz_core::ContainerId::parse(machine.to_string().repeat(64)).unwrap(),
            display_name: "tailscale".into(),
            created_at_unix_nanos: 0,
            machine_id: MachineId::parse(machine.to_string().repeat(32)).unwrap(),
            namespace: identity.namespace.clone(),
            kind: ployz_core::ContainerKind::ServiceContainer,
            runtime,
            effective_healthcheck: None,
            resolved_spec: serde_json::from_value(serde_json::json!({
                "service_id": service_id,
                "name": identity.name,
                "mode": { "mode": "global" },
                "container": { "image": "tailscale:1", "pull_policy": "missing" }
            }))
            .unwrap(),
            address: None,
            labels: Default::default(),
            stopped: false,
            run: None,
        },
    )
    .unwrap();
    ServiceObservation {
        identity,
        service_id,
        containers: vec![ployz_core::ServiceContainer::try_from(observation).unwrap()],
        hook_containers: Vec::new(),
    }
}

// Catches a router Tailscale refused the key for reading as running while Docker keeps
// restarting it: its container runs, but the node never joined the tailnet. A router
// from a daemon that predates the probe says nothing either way, so it still counts.
#[test]
fn a_router_counts_as_running_only_once_its_node_joined_the_tailnet() {
    let health = |health| ContainerRuntimeObservation::Running { health };
    let services = [
        router_on('1', health(ployz_core::HealthObservation::Healthy)),
        router_on('2', health(ployz_core::HealthObservation::NotConfigured)),
        router_on('3', health(ployz_core::HealthObservation::Starting)),
        router_on('4', health(ployz_core::HealthObservation::Unhealthy)),
        router_on('5', health(ployz_core::HealthObservation::Failing)),
        router_on('6', ContainerRuntimeObservation::Restarting),
        router_on('7', ContainerRuntimeObservation::Exited { code: 0 }),
    ];
    let id = |digit: &str| MachineId::parse(digit.repeat(32)).unwrap();
    assert_eq!(running(&services), [id("1"), id("2")].into());
}

#[test]
fn a_refused_key_is_told_from_a_router_that_merely_stopped() {
    let server = MachineName::parse("edge").unwrap();
    let refused = not_serving(
        &server,
        PrivateAccessProblem::KeyRefused,
        "invalid key".into(),
    );
    assert!(key_refused(&refused));
    assert!(refused.message.contains("refused the auth key"));
    assert_eq!(refused.cause, ["invalid key"]);
    let stopped = not_serving(&server, PrivateAccessProblem::NotRunning, String::new());
    assert!(!key_refused(&stopped));
    assert!(stopped.cause.is_empty());

    let set = |problem| PrivateAccessSet {
        failure: Some(ployz_core::PrivateAccessFailure {
            problem,
            detail: "exited".into(),
        }),
        ..PrivateAccessSet::default()
    };
    assert!(
        failure_of(&set(PrivateAccessProblem::NotRunning))
            .unwrap()
            .contains("isn't running. (exited)")
    );
    assert_eq!(failure_of(&PrivateAccessSet::default()), None);
}

#[test]
fn the_auth_key_is_sealed_for_this_cluster_and_never_in_the_clear() {
    let observed = Access {
        machines: Vec::new(),
        running: Default::default(),
        listed: PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        },
        sealing: None,
    };
    let refusal = seal_key(&observed, "tskey-auth-secret")
        .unwrap_err()
        .report();
    assert_eq!(refusal.code, RpcErrorCode::Unavailable);
    assert!(!refusal.message.contains("tskey-auth-secret"));
}
