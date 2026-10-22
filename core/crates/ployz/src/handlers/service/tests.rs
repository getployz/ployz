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
        ("restart", &[ContainerAction::Restart]),
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
fn ps_names_servers_and_shows_container_ids_on_every_row_once_two_rows_read_the_same() {
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
    process_table(&containers, Some(&scoped(&[])), &names, &HashSet::new())
        .write(&mut out, false)
        .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "SERVICE\tKIND\tSERVER\tSTATE\tCONTAINER\n\
             web\tservice\tmachine-1\trunning\taaaaaaaaaaaa\n\
             web\tservice\tmachine-1\trunning\tcccccccccccc\n\
             api\tservice\t{}\trunning\tdddddddddddd\n",
            "e".repeat(12)
        )
    );

    let mut out = Vec::new();
    process_table(
        &containers[1..],
        Some(&scoped(&[])),
        &names,
        &HashSet::new(),
    )
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
fn ps_reads_a_run_its_machine_stopped_as_stopped_and_any_other_exit_by_its_code() {
    let exited = || ContainerRuntimeObservation::Exited { code: 1 };
    let mut parts = observation('a', 'b', "web", exited()).into_parts();
    parts.stopped = true;
    let stopped =
        ServiceContainer::try_from(ployz_core::ContainerObservation::try_from(parts).unwrap())
            .unwrap();
    let crashed = ServiceContainer::try_from(observation('c', 'b', "api", exited())).unwrap();
    let containers = [
        ContainerRef::Service(&stopped),
        ContainerRef::Service(&crashed),
    ];
    let name = MachineName::parse("machine-1").unwrap();
    let names = HashMap::from([(MachineId::parse("b".repeat(32)).unwrap(), &name)]);
    let mut out = Vec::new();
    process_table(&containers, Some(&scoped(&[])), &names, &HashSet::new())
        .write(&mut out, false)
        .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "SERVICE\tKIND\tSERVER\tSTATE\n\
         web\tservice\tmachine-1\tstopped\n\
         api\tservice\tmachine-1\texited with code 1\n"
    );
}

#[test]
fn ps_names_a_renamed_service_by_its_name_now_not_its_private_name() {
    let running = || ContainerRuntimeObservation::Running {
        health: HealthObservation::NotConfigured,
    };
    let box_ = ServiceContainer::try_from(observation('a', 'b', "box", running())).unwrap();
    let web = ServiceContainer::try_from(observation('c', 'b', "web", running())).unwrap();
    let containers = [ContainerRef::Service(&box_), ContainerRef::Service(&web)];
    let name = MachineName::parse("machine-1").unwrap();
    let names = HashMap::from([(MachineId::parse("b".repeat(32)).unwrap(), &name)]);
    let mut out = Vec::new();
    process_table(
        &containers,
        Some(&scoped(&[("box2", "box"), ("web", "web")])),
        &names,
        &HashSet::new(),
    )
    .write(&mut out, false)
    .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "SERVICE\tKIND\tSERVER\tSTATE\n\
         box2\tservice\tmachine-1\trunning\n\
         web\tservice\tmachine-1\trunning\n"
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
        process_table(&containers, Some(&scoped(&[])), &names, retiring)
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

/// The `app` Namespace, with each Service's name now and its runtime name.
fn scoped(services: &[(&str, &str)]) -> crate::handlers::operator::Scoped {
    crate::handlers::operator::Scoped {
        environment: ployz_store::EnvironmentName::parse("production").unwrap(),
        namespace: ployz_core::Namespace::parse("app").unwrap(),
        services: services
            .iter()
            .map(|(name, runtime)| (name.parse().unwrap(), runtime.parse().unwrap()))
            .collect(),
        authored: None,
    }
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
        rotation: None,
        r#gen: None,
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
        stopped: false,
        run: None,
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
        interrupted: false,
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

#[test]
fn out_running_copy_is_draining() {
    let running = || ContainerRuntimeObservation::Running {
        health: HealthObservation::Healthy,
    };
    let copy = |id, rotation, runtime| {
        let mut copy = observation(id, 'f', "api", runtime);
        copy.try_update(|parts| parts.rotation = rotation).unwrap();
        ServiceContainer::try_from(copy).unwrap()
    };
    let copies = [
        copy('a', Some(CopyRotation::In), running()),
        copy('b', Some(CopyRotation::Out), running()),
        copy(
            'c',
            Some(CopyRotation::Out),
            ContainerRuntimeObservation::Exited { code: 0 },
        ),
        copy('d', None, running()),
    ];
    let live = derive_live_services(PartialResult::<_, RpcError> {
        successes: vec![ployz_core::MachineSuccess {
            machine_id: MachineId::parse("f".repeat(32)).unwrap(),
            value: copies
                .iter()
                .map(ServiceContainer::as_observation)
                .cloned()
                .collect(),
        }],
        failures: Vec::new(),
        omissions: Vec::new(),
    });
    let services = live.services();
    let services = services.iter().collect::<Vec<_>>();
    let retiring = retiring(&services);
    let out = |id: char| ContainerId::parse(id.to_string().repeat(64)).unwrap();
    assert_eq!(retiring, HashSet::from([out('b'), out('c')]));
    let containers = copies.iter().map(ContainerRef::Service).collect::<Vec<_>>();
    let draining = process_rows(&containers, &retiring)
        .iter()
        .filter(|row| row.draining)
        .map(|row| row.container.container_id)
        .collect::<Vec<_>>();
    assert_eq!(
        draining,
        [out('b')],
        "only an Out copy that still runs drains"
    );
}

fn changed(partial: bool) -> ServiceActionOutcome {
    ServiceActionOutcome {
        containers: Vec::new(),
        container_failures: Vec::new(),
        partial,
        interrupted: false,
    }
}

#[tokio::test]
async fn a_lifecycle_cancelled_before_its_first_step_exits_cancelled() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let outcome = run_batches(
        &[()],
        &[ContainerAction::Stop],
        &cancellation,
        |(), _| async { Ok(changed(false)) },
    )
    .await
    .unwrap();
    assert!(outcome.partial);
    assert_eq!(cancelled_exit(&outcome), 130);
}

#[tokio::test]
async fn a_lifecycle_cancelled_between_batches_exits_cancelled() {
    let cancellation = CancellationToken::new();
    let mut steps = 0;
    let outcome = run_batches(
        &[(), ()],
        &[ContainerAction::Restart],
        &cancellation,
        |(), _| {
            steps += 1;
            cancellation.cancel();
            async { Ok(changed(false)) }
        },
    )
    .await
    .unwrap();
    assert_eq!(steps, 1);
    assert!(outcome.partial);
    assert_eq!(cancelled_exit(&outcome), 130);
}

/// Ctrl-C during the last step ends no later step, but it still ends the command as cancelled.
#[tokio::test]
async fn a_lifecycle_cancelled_during_its_last_step_exits_cancelled() {
    let cancellation = CancellationToken::new();
    let outcome = run_batches(
        &[()],
        &[ContainerAction::Restart],
        &cancellation,
        |(), _| {
            cancellation.cancel();
            async { Ok(changed(true)) }
        },
    )
    .await
    .unwrap();
    assert_eq!(cancelled_exit(&outcome), 130);
}

fn cancelled_exit(outcome: &ServiceActionOutcome) -> u8 {
    crate::ui::after_result(&outcome.finish().unwrap_err())
}

/// `apply_service_action` marks a step partial for each copy it could not restart,
/// or left Held or unrouted. One such step among later successes still exits partial.
#[tokio::test]
async fn a_restart_that_fails_a_copy_exits_partial() {
    let failed = || ServiceActionOutcome {
        container_failures: vec![ContainerFailure {
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            container_id: ContainerId::parse("c".repeat(64)).unwrap(),
            error: RpcError {
                code: RpcErrorCode::Conflict,
                message: "Container c is left Held and not routed".into(),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            },
        }],
        ..changed(true)
    };
    let mut steps = vec![failed(), changed(false), changed(false)].into_iter();
    let outcome = run_batches(
        &[(), (), ()],
        &[ContainerAction::Restart],
        &CancellationToken::new(),
        |(), _| {
            let step = steps.next().unwrap();
            async { Ok(step) }
        },
    )
    .await
    .unwrap();
    let error = outcome.finish().unwrap_err();
    assert_eq!(error.printed_exit(), Some(crate::ui::PARTIAL_EXIT));
}

#[test]
fn a_stop_signal_docker_would_refuse_is_refused_before_any_server() {
    for command in ["stop", "restart"] {
        let signal = |signal: &str| {
            crate::cli::command()
                .try_get_matches_from(["ployz", "service", command, "api", "--signal", signal])
        };
        for refused in [
            "NOPE",
            "SIGNOPE",
            "SIGSIGTERM",
            "0",
            "65",
            "TERM9",
            "RTMAX-15",
            "RTMIN+16",
            "RTMIN+0",
            "RTMIN+01",
            "RTMIN++1",
        ] {
            let error = signal(refused).unwrap_err();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{refused}"
            );
            assert!(
                error
                    .to_string()
                    .contains("a signal name like SIGTERM or SIGINT, or a number"),
                "{error}"
            );
        }
        for taken in [
            "SIGTERM",
            "SIGINT",
            "term",
            "KILL",
            "sigusr1",
            "9",
            "15",
            "SIGRTMIN+3",
            "RTMAX-1",
        ] {
            let matches = signal(taken).unwrap();
            assert_eq!(
                leaf_matches(&matches)
                    .get_one::<String>("signal")
                    .map(String::as_str),
                Some(taken),
                "{taken} reaches Docker as typed"
            );
        }
    }
}
