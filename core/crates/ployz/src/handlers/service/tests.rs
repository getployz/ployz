use ployz_core::{
    HookContainer, MachineFailure, MachineId, PartialResult, RpcError, RpcErrorCode,
    ServiceContainer, ServiceId, ServiceName, derive_live_services,
};
use serde_json::json;

use super::*;

#[test]
fn process_sort_orders_match_the_cli_contract() {
    let beta = observation(
        'b',
        'b',
        "beta",
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
    );
    let alpha = observation(
        'a',
        'c',
        "alpha",
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy,
        },
    );
    let gamma = observation('c', 'a', "gamma", ContainerRuntimeObservation::Created);
    let hook = hook_observation(
        'd',
        'd',
        "delta",
        ContainerRuntimeObservation::Exited { code: 0 },
    );

    let beta = ServiceContainer::try_from(beta).unwrap();
    let alpha = ServiceContainer::try_from(alpha).unwrap();
    let gamma = ServiceContainer::try_from(gamma).unwrap();
    let hook = HookContainer::try_from(hook).unwrap();
    let mut containers = vec![
        ContainerRef::Service(&beta),
        ContainerRef::Service(&alpha),
        ContainerRef::Service(&gamma),
        ContainerRef::Hook(&hook),
    ];
    sort_processes(&mut containers, "service");
    assert_eq!(names(&containers), ["alpha", "beta", "delta", "gamma"]);
    sort_processes(&mut containers, "machine");
    assert_eq!(names(&containers), ["gamma", "beta", "alpha", "delta"]);
    sort_processes(&mut containers, "health");
    assert_eq!(names(&containers), ["alpha", "gamma", "beta", "delta"]);
}

#[test]
fn stop_options_are_only_read_for_stop_actions() {
    for (command, action) in [
        ("start", ContainerAction::Start),
        ("rm", ContainerAction::Remove),
    ] {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "service", command, "api"])
            .unwrap();
        assert_eq!(
            stop_options(leaf_matches(&matches), &[action]).unwrap(),
            (None, None)
        );
    }

    for (command, actions) in [
        ("stop", &[ContainerAction::Stop][..]),
        ("restart", &[ContainerAction::Stop, ContainerAction::Start]),
    ] {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "service", command, "api"])
            .unwrap();
        assert_eq!(
            stop_options(leaf_matches(&matches), actions).unwrap(),
            (Some("SIGTERM".into()), Some(10))
        );
    }
}

#[test]
fn ps_names_servers_and_shows_a_container_id_only_on_rows_that_read_the_same() {
    let running = || ContainerRuntimeObservation::Running {
        health: HealthObservation::NotConfigured,
    };
    let first = ServiceContainer::try_from(observation('a', 'b', "web", running())).unwrap();
    let second = ServiceContainer::try_from(observation('c', 'b', "web", running())).unwrap();
    let api = ServiceContainer::try_from(observation('d', 'e', "api", running())).unwrap();
    let containers = [
        ContainerRef::Service(&first),
        ContainerRef::Service(&second),
        ContainerRef::Service(&api),
    ];
    let name = MachineName::parse("machine-1").unwrap();
    let names = HashMap::from([(MachineId::parse("b".repeat(32)).unwrap(), &name)]);
    let mut out = Vec::new();
    process_table(&containers, true, &names, &HashSet::new())
        .write(&mut out, false)
        .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "SERVICE\tKIND\tSERVER\tSTATE\tCONTAINER\n\
             web\tservice\tmachine-1\trunning\taaaaaaaaaaaa\n\
             web\tservice\tmachine-1\trunning\tcccccccccccc\n\
             api\tservice\t{}\trunning\t-\n",
            "e".repeat(12)
        )
    );

    let mut out = Vec::new();
    process_table(&containers[1..], true, &names, &HashSet::new())
        .write(&mut out, false)
        .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "SERVICE\tKIND\tSERVER\tSTATE\n\
             web\tservice\tmachine-1\trunning\n\
             api\tservice\t{}\trunning\n",
            "e".repeat(12)
        )
    );
}

#[test]
fn ps_folds_a_services_superseded_containers_into_one_draining_row() {
    let running = || ContainerRuntimeObservation::Running {
        health: HealthObservation::NotConfigured,
    };
    let web = ['a', 'b', 'c', 'd']
        .map(|id| ServiceContainer::try_from(observation(id, 'f', "web", running())).unwrap());
    let stopped = ServiceContainer::try_from(observation(
        'e',
        'f',
        "web",
        ContainerRuntimeObservation::Exited { code: 0 },
    ))
    .unwrap();
    let mut containers = web.iter().map(ContainerRef::Service).collect::<Vec<_>>();
    containers.push(ContainerRef::Service(&stopped));
    let name = MachineName::parse("machine-1").unwrap();
    let names = HashMap::from([(MachineId::parse("f".repeat(32)).unwrap(), &name)]);
    let superseded = |ids: &[char]| {
        ids.iter()
            .map(|id| ContainerId::parse(id.to_string().repeat(64)).unwrap())
            .collect::<HashSet<_>>()
    };
    let render = |retiring: &HashSet<ContainerId>| {
        let mut out = Vec::new();
        process_table(&containers, true, &names, retiring)
            .write(&mut out, false)
            .unwrap();
        let json = serde_json::to_value(process_rows(&containers, retiring)).unwrap();
        let draining = json
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                assert!(entry.get("container_id").is_some());
                entry.get("draining").cloned()
            })
            .collect::<Vec<_>>();
        (String::from_utf8(out).unwrap(), draining)
    };
    let yes = Some(json!(true));

    assert_eq!(
        render(&superseded(&[])),
        (
            "SERVICE\tKIND\tSERVER\tSTATE\tCONTAINER\n\
             web\tservice\tmachine-1\trunning\taaaaaaaaaaaa\n\
             web\tservice\tmachine-1\trunning\tbbbbbbbbbbbb\n\
             web\tservice\tmachine-1\trunning\tcccccccccccc\n\
             web\tservice\tmachine-1\trunning\tdddddddddddd\n\
             web\tservice\tmachine-1\texited with code 0\teeeeeeeeeeee\n"
                .into(),
            vec![None; 5]
        )
    );
    assert_eq!(
        render(&superseded(&['b', 'e'])),
        (
            "SERVICE\tKIND\tSERVER\tSTATE\tCONTAINER\n\
             web\tservice\tmachine-1\trunning\taaaaaaaaaaaa\n\
             web\tservice\tmachine-1\tdraining 1\t-\n\
             web\tservice\tmachine-1\trunning\tcccccccccccc\n\
             web\tservice\tmachine-1\trunning\tdddddddddddd\n\
             web\tservice\tmachine-1\texited with code 0\teeeeeeeeeeee\n"
                .into(),
            vec![None, yes.clone(), None, None, None]
        )
    );
    assert_eq!(
        render(&superseded(&['a', 'b', 'c'])),
        (
            "SERVICE\tKIND\tSERVER\tSTATE\tCONTAINER\n\
             web\tservice\tmachine-1\tdraining 3\t-\n\
             web\tservice\tmachine-1\trunning\tdddddddddddd\n\
             web\tservice\tmachine-1\texited with code 0\teeeeeeeeeeee\n"
                .into(),
            vec![yes.clone(), yes.clone(), yes, None, None]
        )
    );
}

#[test]
fn lifecycle_selectors_deduplicate_names_and_ids() {
    let container = observation('a', 'a', "api", ContainerRuntimeObservation::Created);
    let service_id = container.service_id();
    let services = vec![ployz_core::ServiceObservation {
        identity: container.identity(),
        service_id,
        containers: vec![ServiceContainer::try_from(container).unwrap()],
        hook_containers: Vec::new(),
    }];
    let selectors = vec![
        ServiceSelector::parse("api").unwrap(),
        ServiceSelector::from(&service_id),
    ];

    assert_eq!(select_services(&services, &selectors).unwrap().len(), 1);
}

fn names<'a>(containers: &'a [ContainerRef<'a>]) -> Vec<&'a str> {
    containers
        .iter()
        .map(|container| container.as_observation().resolved_spec.name.as_str())
        .collect()
}

fn hook_observation(
    id: char,
    machine: char,
    name: &str,
    runtime: ContainerRuntimeObservation,
) -> ployz_core::ContainerObservation {
    let mut observation = observation(id, machine, name, runtime);
    observation
        .try_update(|parts| parts.kind = ployz_core::ContainerKind::PreDeployHook)
        .unwrap();
    observation
}

fn observation(
    id: char,
    machine: char,
    name: &str,
    runtime: ContainerRuntimeObservation,
) -> ployz_core::ContainerObservation {
    let service_id = ServiceId::parse(id.to_string().repeat(32)).unwrap();
    let service_name = ServiceName::parse(name).unwrap();
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        container_id: ployz_core::ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        display_name: name.into(),
        created_at_unix_nanos: 0,
        machine_id: MachineId::parse(machine.to_string().repeat(32)).unwrap(),
        namespace: ployz_core::Namespace::parse("app").unwrap(),
        kind: ployz_core::ContainerKind::ServiceContainer,
        runtime,
        effective_healthcheck: None,
        resolved_spec: serde_json::from_value(json!({
            "service_id": service_id,
            "name": service_name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
        }))
        .unwrap(),
        address: None,
        labels: Default::default(),
    })
    .unwrap()
}

#[test]
fn action_result_keeps_machine_failures_apart_from_container_failures() {
    let failed_id = MachineId::parse("2".repeat(32)).unwrap();
    let error = RpcError {
        code: RpcErrorCode::Unavailable,
        message: "offline".into(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    };
    let live = derive_live_services(PartialResult::<Vec<ployz_core::ContainerObservation>, _> {
        successes: Vec::new(),
        failures: vec![MachineFailure {
            machine_id: failed_id,
            error: error.clone(),
        }],
        omissions: Vec::new(),
    });
    let container_id = ContainerId::parse("c".repeat(64)).unwrap();
    let outcome = ServiceActionOutcome {
        containers: Vec::new(),
        container_failures: vec![ContainerFailure {
            machine_id: failed_id,
            container_id,
            error: error.clone(),
        }],
        partial: true,
    };
    assert_eq!(
        serde_json::to_value(outcome.result(&live)).unwrap(),
        json!({
            "containers": [],
            "container_failures": [{
                "machine_id": failed_id,
                "container_id": container_id,
                "error": error,
            }],
            "failures": [{ "machine_id": failed_id, "error": error }],
            "omitted": [],
        })
    );
}

#[derive(Clone, Default)]
struct SlowRow {
    row: crate::serving_publish::tests::FakeRow,
    reads: std::sync::Arc<std::sync::Mutex<u32>>,
}

impl PointerClient for SlowRow {
    async fn pointer_publisher(&mut self) -> Result<MachineTarget, RpcError> {
        self.row.pointer_publisher().await
    }

    async fn read_pointer_at(
        &mut self,
        at: &mut MachineTarget,
        service: &QualifiedService,
    ) -> Result<Option<ployz_core::ServingPointer>, RpcError> {
        *self.reads.lock().unwrap() += 1;
        tokio::time::sleep(crate::connect::TARGET_RPC_TIMEOUT).await;
        self.row.read_pointer_at(at, service).await
    }

    async fn publish_pointer_at(
        &mut self,
        at: &mut MachineTarget,
        request: ployz_core::PublishServingPointerRequest,
    ) -> Result<ployz_core::ServingPointerPublication, RpcError> {
        self.row.publish_pointer_at(at, request).await
    }
}

fn up_machine(hex: char) -> MachineObservation {
    ployz_core::MachineObservation::new(
        ployz_core::Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(format!("machine-{hex}")).unwrap(),
            subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                .parse()
                .unwrap(),
            public_key: ployz_core::WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: Default::default(),
            build_concurrency: None,
        },
        ployz_core::MembershipObservation::Up,
    )
}

async fn retiring_with_publisher(
    reached: bool,
) -> (HashSet<ContainerId>, u32, std::time::Duration) {
    use crate::serving_publish::tests::pointer;
    let machines = [up_machine('a'), up_machine('b')];
    let running = ContainerRuntimeObservation::Running {
        health: HealthObservation::Healthy,
    };
    let publisher = machines[0].machine.id;
    let failures = if reached {
        Vec::new()
    } else {
        vec![MachineFailure {
            machine_id: publisher,
            error: RpcError {
                code: RpcErrorCode::Unavailable,
                message: "ployzd is down".into(),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            },
        }]
    };
    let live = derive_live_services(PartialResult {
        successes: vec![ployz_core::MachineSuccess {
            machine_id: machines[1].machine.id,
            value: vec![observation('b', 'b', "api", running)],
        }],
        failures,
        omissions: Vec::new(),
    });
    let services = live.services();
    let services = services.iter().collect::<Vec<_>>();
    let client = SlowRow::default();
    let (serving, retiring_ids) = (['c'], ['b']);
    *client.row.row.lock().unwrap() = Some(pointer('d', &serving, &retiring_ids));
    let started = tokio::time::Instant::now();
    let ids = retiring(&client, &machines, &live.containers, &services).await;
    let reads = *client.reads.lock().unwrap();
    (ids, reads, started.elapsed())
}

#[tokio::test(start_paused = true)]
async fn ps_does_not_reread_a_publisher_the_listing_could_not_reach() {
    let (ids, reads, elapsed) = retiring_with_publisher(false).await;
    assert!(ids.is_empty());
    assert_eq!(reads, 0);
    assert!(elapsed < crate::connect::TARGET_RPC_TIMEOUT, "{elapsed:?}");
}

#[tokio::test(start_paused = true)]
async fn ps_reads_retiring_ids_from_a_reached_publisher() {
    let (ids, reads, _) = retiring_with_publisher(true).await;
    assert_eq!(
        ids,
        HashSet::from([crate::serving_publish::tests::container('b')])
    );
    assert_eq!(reads, 1);
}
