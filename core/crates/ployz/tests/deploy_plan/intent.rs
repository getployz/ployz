use std::collections::BTreeMap;

use super::support::*;
use ployz::deploy::{SlotAction, SlotRepair, plan_ingress_repairs, preview_deploy};
use ployz_core::{
    ConfiguredHealthcheck, ContainerRuntimeObservation, DependencyCondition, DeployWarning,
    HealthObservation, HealthcheckCommand, HealthcheckSpec, MachineFailure, PreDeployHook,
    RpcError, RpcErrorCode, ServiceDependency, ServiceName,
};

#[test]
fn empty_selected_plans_every_target_service_in_dependency_order() {
    let (db, web, worker, dependencies) = web_db_worker();
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&db, &web, &worker],
        PlanOptions::default(),
    )
    .with_dependencies(dependencies);
    let plan = preview_deploy(&intent, &snapshot()).unwrap();
    assert_eq!(run_names(&plan), ["db", "web", "worker"]);
}

#[test]
fn apply_web_plans_web_and_db_not_worker() {
    let (db, web, mut worker, dependencies) = web_db_worker();
    worker.container.image = "ghcr.io/getployz/worker:old".into();
    let intent = DeployIntent::new(
        Namespace::parse("app").unwrap(),
        vec![db, web, spec("worker")],
        PlanOptions {
            selected: vec![attempt("web")],
            ..PlanOptions::default()
        },
    )
    .with_dependencies(dependencies);
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![container('c', '1', &worker, &service_id('a'))],
        ..Default::default()
    };
    let plan = preview_deploy(&intent, &snapshot).unwrap();
    assert_eq!(run_names(&plan), ["db", "web"]);
    assert!(!plan.operations.iter().any(|row| {
        matches!(
            &row.operation,
            DeployOperation::ReplaceContainer(replacement)
                if replacement.old_container_id == container_id('c')
        )
    }));
}

#[test]
fn one_spec_intent_plans_that_name() {
    let plan = preview_deploy(
        &DeployIntent::apply_one(
            Namespace::parse("app").unwrap(),
            spec("caddy"),
            PlanOptions::default(),
        ),
        &snapshot(),
    )
    .unwrap();
    assert_eq!(run_names(&plan), ["caddy"]);
}

#[test]
fn cyclic_apply_dependencies_are_a_plan_error() {
    let db = spec("db");
    let web = spec("web");
    let dependencies = BTreeMap::from([
        (
            web.name.clone(),
            vec![ServiceDependency {
                service: db.name.clone(),
                condition: DependencyCondition::ServiceStarted,
            }],
        ),
        (
            db.name.clone(),
            vec![ServiceDependency {
                service: web.name.clone(),
                condition: DependencyCondition::ServiceStarted,
            }],
        ),
    ]);
    let intent = DeployIntent::new(
        Namespace::parse("app").unwrap(),
        vec![db, web],
        PlanOptions {
            selected: vec![attempt("web")],
            ..PlanOptions::default()
        },
    )
    .with_dependencies(dependencies);
    assert!(matches!(
        preview_deploy(&intent, &snapshot()),
        Err(PlanError::DependencyCycle { service }) if service == "db"
    ));
}

#[test]
fn skip_health_on_options_is_set_on_planned_operations() {
    let plan = preview_deploy(
        &DeployIntent::apply_one(
            Namespace::parse("app").unwrap(),
            spec("api"),
            PlanOptions {
                skip_health_monitor: true,
                ..Default::default()
            },
        ),
        &snapshot(),
    )
    .unwrap();
    assert!(matches!(
        operations(&plan).as_slice(),
        [DeployOperation::RunContainer {
            skip_health_monitor: true,
            ..
        }]
    ));
}

#[test]
fn selected_service_healthy_wait_precedes_the_dependent_hook() {
    let mut db = spec("db");
    db.container.healthcheck = Some(configured_healthcheck());
    let mut web = spec("web");
    web.pre_deploy = Some(PreDeployHook {
        command: vec!["migrate".into()].try_into().unwrap(),
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    let intent = DeployIntent::new(
        Namespace::parse("app").unwrap(),
        vec![db.clone(), web.clone()],
        PlanOptions {
            selected: vec![attempt("web")],
            ..PlanOptions::default()
        },
    )
    .with_dependencies(BTreeMap::from([(
        web.name,
        vec![ServiceDependency {
            service: db.name,
            condition: DependencyCondition::ServiceHealthy,
        }],
    )]));

    let plan = preview_deploy(&intent, &snapshot()).unwrap();
    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::RunContainer { spec: db, .. },
            DeployOperation::WaitHealthy {
                dependent,
                dependency,
                ..
            },
            DeployOperation::RunHook { spec: hook, .. },
            DeployOperation::RunContainer { spec: web, .. },
        ] if db.name.as_str() == "db"
            && dependent.to_string() == "app/web"
            && dependency.to_string() == "app/db"
            && hook.name.as_str() == "web"
            && web.name.as_str() == "web"
    ));
}

#[test]
fn healthy_dependency_does_not_gate_scale_down() {
    let mut db = spec("db");
    db.container.healthcheck = Some(configured_healthcheck());
    let web = spec("web");
    let web_service_id = service_id('a');
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&db, &web],
        PlanOptions::default(),
    )
    .with_dependencies(BTreeMap::from([(
        web.name.clone(),
        vec![ServiceDependency {
            service: db.name,
            condition: DependencyCondition::ServiceHealthy,
        }],
    )]));
    let plan = preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![
                container('b', '1', &web, &web_service_id),
                container('c', '1', &web, &web_service_id),
            ],
            ..Default::default()
        },
    )
    .unwrap();

    assert!(
        !operations(&plan)
            .iter()
            .any(|operation| matches!(operation, DeployOperation::WaitHealthy { .. }))
    );
    assert!(plan.warnings.is_empty());
}

#[test]
fn skip_health_omits_wait_and_warns_for_each_weakened_edge() {
    let mut db = spec("db");
    db.container.healthcheck = Some(configured_healthcheck());
    let web = spec("web");
    let intent = DeployIntent::new(
        Namespace::parse("app").unwrap(),
        vec![db.clone(), web.clone()],
        PlanOptions {
            skip_health_monitor: true,
            ..PlanOptions::default()
        },
    )
    .with_dependencies(BTreeMap::from([(
        web.name,
        vec![ServiceDependency {
            service: db.name,
            condition: DependencyCondition::ServiceHealthy,
        }],
    )]));

    let plan = preview_deploy(&intent, &snapshot()).unwrap();
    assert!(
        !operations(&plan)
            .iter()
            .any(|operation| matches!(operation, DeployOperation::WaitHealthy { .. }))
    );
    assert!(matches!(
        plan.warnings.as_slice(),
        [DeployWarning::SkippedDependencyHealth {
            dependent,
            dependency,
        }] if dependent.to_string() == "app/web" && dependency.to_string() == "app/db"
    ));
}

fn configured_healthcheck() -> HealthcheckSpec {
    HealthcheckSpec::Configured(ConfiguredHealthcheck {
        test: HealthcheckCommand::parse(["CMD", "true"]).unwrap(),
        interval_millis: Some(1_000),
        timeout_millis: Some(1_000),
        start_period_millis: None,
        start_interval_millis: None,
        retries: Some(1),
        deadline_millis: None,
    })
}

fn web_db_worker() -> (
    RequestedServiceSpec,
    RequestedServiceSpec,
    RequestedServiceSpec,
    BTreeMap<ServiceName, Vec<ServiceDependency>>,
) {
    let db = spec("db");
    let web = spec("web");
    let worker = spec("worker");
    let dependencies = BTreeMap::from([(
        web.name.clone(),
        vec![ServiceDependency {
            service: db.name.clone(),
            condition: DependencyCondition::ServiceStarted,
        }],
    )]);
    (db, web, worker, dependencies)
}

fn attempt(name: &str) -> ServiceAttempt {
    ServiceAttempt {
        name: ServiceName::parse(name).unwrap(),
    }
}

fn snapshot() -> DeploySnapshot {
    DeploySnapshot {
        machines: vec![machine('1', "first")],
        ..Default::default()
    }
}

fn run_names(plan: &ployz::deploy::DeployPreview) -> Vec<&str> {
    plan.operations
        .iter()
        .filter_map(|row| match &row.operation {
            DeployOperation::RunContainer { spec, .. } => Some(spec.name.as_str()),
            DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::ReplaceContainer(_)
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::PrepareVolumes { .. }
            | DeployOperation::RemoveVolume { .. } => None,
        })
        .collect()
}

fn targets_container(plan: &ployz::deploy::DeployPreview, id: &ContainerId) -> bool {
    plan.operations.iter().any(|row| match &row.operation {
        DeployOperation::StopContainer { container_id, .. }
        | DeployOperation::RemoveContainer { container_id, .. }
        | DeployOperation::StopHook { container_id, .. } => container_id == id,
        DeployOperation::ReplaceContainer(replacement) => &replacement.old_container_id == id,
        DeployOperation::WaitHealthy { .. }
        | DeployOperation::RunContainer { .. }
        | DeployOperation::RunHook { .. }
        | DeployOperation::PrepareVolumes { .. }
        | DeployOperation::RemoveVolume { .. } => false,
    })
}

#[test]
fn user_namespace_deploy_does_not_replace_or_remove_system_ingress() {
    let mut system_ingress = spec("ingress");
    system_ingress.mode = ServiceMode::Global;
    system_ingress.container.image = "caddy:2.9.1".into();
    let mut shop_caddy = spec("caddy");
    shop_caddy.mode = ServiceMode::Global;
    shop_caddy.container.image = "caddy:2.10.2".into();
    let mut system_container = container('c', '1', &system_ingress, &service_id('a'));
    system_container
        .try_update(|parts| parts.namespace = Namespace::system())
        .unwrap();

    let shop = preview_deploy(
        &DeployIntent::apply_one(
            Namespace::parse("shop").unwrap(),
            shop_caddy,
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![system_container],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!targets_container(&shop, &container_id('c')));
    assert_eq!(run_names(&shop), ["caddy"]);

    let web = spec("web");
    let mut leftover = container('c', '1', &system_ingress, &service_id('a'));
    leftover
        .try_update(|parts| parts.namespace = Namespace::system())
        .unwrap();
    let full = preview_deploy(
        &DeployIntent::apply_all(
            Namespace::parse("shop").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![leftover],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!targets_container(&full, &container_id('c')));
    assert_eq!(run_names(&full), ["web"]);
}

fn ingress_container(
    hex: char,
    machine_hex: char,
    image: &str,
    runtime: ployz_core::ContainerRuntimeObservation,
) -> ployz_core::ContainerObservation {
    let ingress = ployz_core::caddy_service_spec(image.into(), Default::default());
    let mut observed = container(hex, machine_hex, &ingress, &service_id('a'));
    observed
        .try_update(|parts| {
            parts.namespace = Namespace::system();
            parts.runtime = runtime;
            parts
                .labels
                .insert("ployz.creation.key".into(), hex.to_string());
        })
        .unwrap();
    observed
}

fn planned_ingress(snapshot: &DeploySnapshot) -> Vec<SlotRepair> {
    let ingress = ployz_core::caddy_service_spec("caddy:test".into(), Default::default());
    let intent = DeployIntent::apply_all(Namespace::system(), [&ingress], PlanOptions::default());
    plan_ingress_repairs(&intent, snapshot)
}

#[derive(Debug, PartialEq)]
enum Repair {
    Create,
    Start(ContainerId),
    Dead(ContainerId),
    Restarting(ContainerId),
    Unchecked,
}

fn ingress_repairs(snapshot: &DeploySnapshot) -> Vec<(MachineId, Repair)> {
    planned_ingress(snapshot)
        .into_iter()
        .map(|repair| {
            let shown = match repair.action {
                SlotAction::Create(plan) => {
                    assert!(operations(plan.unwrap().preview()).iter().all(|operation| {
                        operation.machine_id() == repair.machine_id
                            && matches!(operation, DeployOperation::RunContainer { .. })
                    }));
                    Repair::Create
                }
                SlotAction::Start(container_id) => Repair::Start(container_id),
                SlotAction::Dead(container_id) => Repair::Dead(container_id),
                SlotAction::Restarting(container_id) => Repair::Restarting(container_id),
                SlotAction::Unchecked(_) => Repair::Unchecked,
            };
            (repair.machine_id, shown)
        })
        .collect()
}

fn running() -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running {
        health: HealthObservation::Healthy,
    }
}

#[test]
fn ingress_repair_creates_a_missing_slot_and_starts_a_stopped_one() {
    let exited = ContainerRuntimeObservation::Exited { code: 0 };
    let snapshot = DeploySnapshot {
        machines: vec![
            machine('1', "serving"),
            machine('2', "missing"),
            machine('3', "stopped"),
        ],
        containers: vec![
            ingress_container('a', '1', "caddy:test", running()),
            ingress_container('c', '3', "caddy:test", exited),
        ],
        ..Default::default()
    };
    assert_eq!(
        ingress_repairs(&snapshot),
        [
            (machine_id('2'), Repair::Create),
            (machine_id('3'), Repair::Start(container_id('c')))
        ]
    );
}

#[test]
fn ingress_created_on_every_server_at_once_is_one_service() {
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "missing"), machine('2', "also-missing")],
        ..Default::default()
    };
    let service_ids: Vec<_> = planned_ingress(&snapshot)
        .into_iter()
        .map(|repair| {
            let SlotAction::Create(Ok(plan)) = repair.action else {
                panic!("expected a create, got {:?}", repair.action);
            };
            let operations = operations(plan.preview());
            let [DeployOperation::RunContainer { spec, .. }] = operations.as_slice() else {
                panic!("expected one run");
            };
            spec.service_id
        })
        .collect();
    assert!(
        matches!(service_ids.as_slice(), [first, second] if first == second),
        "{service_ids:?}"
    );
}

#[test]
fn a_dead_ingress_proxy_is_never_started_and_never_returns() {
    let dead = || ContainerRuntimeObservation::Dead;
    let mut only_dead = machine('1', "only-dead");
    only_dead.machine.public_ip = Some(std::net::IpAddr::from([192, 0, 2, 1]));
    let mut stopped_beside = machine('2', "stopped-beside-dead");
    stopped_beside.machine.public_ip = Some(std::net::IpAddr::from([192, 0, 2, 2]));
    let snapshot = DeploySnapshot {
        machines: vec![only_dead, stopped_beside],
        containers: vec![
            ingress_container('a', '1', "caddy:test", dead()),
            ingress_container('b', '1', "caddy:test", dead()),
            ingress_container(
                'c',
                '2',
                "caddy:test",
                ContainerRuntimeObservation::Exited { code: 0 },
            ),
            ingress_container('d', '2', "caddy:test", dead()),
        ],
        ..Default::default()
    };
    assert_eq!(
        ingress_repairs(&snapshot),
        [
            (machine_id('1'), Repair::Dead(container_id('b'))),
            (machine_id('2'), Repair::Start(container_id('c'))),
        ]
    );
    assert_eq!(
        snapshot.ingress_returning(),
        [Some(std::net::IpAddr::from([192, 0, 2, 2]))]
    );
}

#[test]
fn a_crash_looping_ingress_proxy_is_reported_not_repaired_and_never_returns() {
    let restarting = || ContainerRuntimeObservation::Restarting;
    let mut looping = machine('1', "looping");
    looping.machine.public_ip = Some(std::net::IpAddr::from([192, 0, 2, 1]));
    let snapshot = DeploySnapshot {
        machines: vec![looping, machine('2', "running-beside-looping")],
        containers: vec![
            ingress_container('a', '1', "caddy:test", restarting()),
            ingress_container(
                'b',
                '1',
                "caddy:test",
                ContainerRuntimeObservation::Exited { code: 0 },
            ),
            ingress_container('c', '2', "caddy:test", restarting()),
            ingress_container('d', '2', "caddy:test", running()),
        ],
        ..Default::default()
    };
    assert_eq!(
        ingress_repairs(&snapshot),
        [(machine_id('1'), Repair::Restarting(container_id('a')))]
    );
    assert!(snapshot.ingress_returning().is_empty());
}

#[test]
fn a_created_ingress_proxy_joins_the_service_its_siblings_run() {
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "serving"), machine('2', "missing")],
        containers: vec![ingress_container('a', '1', "caddy:test", running())],
        ..Default::default()
    };
    let repairs = planned_ingress(&snapshot);
    let [
        SlotRepair {
            action: SlotAction::Create(Ok(plan)),
            ..
        },
    ] = repairs.as_slice()
    else {
        panic!("expected one create, got {repairs:?}");
    };
    assert!(matches!(
        operations(plan.preview()).as_slice(),
        [DeployOperation::RunContainer { spec, .. }] if spec.service_id == service_id('a')
    ));
}

#[test]
fn a_stopped_replacement_starts_as_it_is_whatever_its_creation_key() {
    let mut replacement = ingress_container(
        'b',
        '1',
        "caddy:test",
        ContainerRuntimeObservation::Exited { code: 0 },
    );
    replacement
        .try_update(|parts| {
            parts.labels.insert(
                "ployz.creation.key".into(),
                format!(
                    "global:{}:shape:replace:{}",
                    service_id('a'),
                    container_id('a')
                ),
            );
        })
        .unwrap();
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "stopped")],
        containers: vec![replacement],
        ..Default::default()
    };
    assert_eq!(
        ingress_repairs(&snapshot),
        [(machine_id('1'), Repair::Start(container_id('b')))]
    );
}

#[test]
fn ingress_repair_starts_a_stopped_older_proxy_and_leaves_active_or_unwanted_ones() {
    let exited = ContainerRuntimeObservation::Exited { code: 0 };
    let mut no_ingress = machine('3', "no-ingress");
    no_ingress.machine.accepts_ingress = false;
    let snapshot = DeploySnapshot {
        machines: vec![
            machine('1', "older"),
            machine('2', "older-stopped"),
            no_ingress,
            machine('5', "paused"),
        ],
        containers: vec![
            ingress_container('a', '1', "caddy:old", running()),
            ingress_container('b', '2', "caddy:old", exited.clone()),
            ingress_container('c', '3', "caddy:test", running()),
            ingress_container('e', '5', "caddy:test", ContainerRuntimeObservation::Paused),
            ingress_container('f', '5', "caddy:test", exited),
        ],
        ..Default::default()
    };
    assert_eq!(
        ingress_repairs(&snapshot),
        [(machine_id('2'), Repair::Start(container_id('b')))]
    );
}

#[test]
fn ingress_repair_that_cannot_admit_on_one_server_still_plans_the_other() {
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "full"), machine('2', "free")],
        capacity: capacity([('1', 0), ('2', 5)]),
        ..Default::default()
    };
    let repairs = planned_ingress(&snapshot);
    let plans: Vec<_> = repairs
        .iter()
        .map(|repair| match &repair.action {
            SlotAction::Create(plan) => (repair.machine_id, plan.as_ref().ok()),
            other @ (SlotAction::Start(_)
            | SlotAction::Dead(_)
            | SlotAction::Restarting(_)
            | SlotAction::Unchecked(_)) => {
                panic!("expected a create, got {other:?}")
            }
        })
        .collect();
    assert!(matches!(
        plans.as_slice(),
        [(full, None), (free, Some(plan))]
            if *full == machine_id('1') && *free == machine_id('2')
                && matches!(
                    operations(plan.preview()).as_slice(),
                    [DeployOperation::RunContainer { machine_id: on, .. }, ..] if *on == machine_id('2')
                )
    ));
}

#[test]
fn a_server_whose_containers_went_unobserved_is_unchecked_not_repaired() {
    let mut unlisted = machine('1', "unlisted");
    unlisted.machine.public_ip = Some(std::net::IpAddr::from([192, 0, 2, 1]));
    let snapshot = DeploySnapshot {
        machines: vec![unlisted, machine('2', "omitted"), machine('3', "missing")],
        container_failures: vec![MachineFailure {
            machine_id: machine_id('1'),
            error: RpcError {
                code: RpcErrorCode::Unavailable,
                message: "container listing failed".into(),
                details: Default::default(),
                cause: Vec::new(),
            },
        }],
        container_omissions: vec![machine_id('2')],
        ..Default::default()
    };
    assert_eq!(
        ingress_repairs(&snapshot),
        [
            (machine_id('1'), Repair::Unchecked),
            (machine_id('2'), Repair::Unchecked),
            (machine_id('3'), Repair::Create)
        ]
    );
    assert!(matches!(
        &planned_ingress(&snapshot).first().unwrap().action,
        SlotAction::Unchecked(cause) if cause.contains("container listing failed")
    ));
    assert_eq!(snapshot.ingress_returning(), [None]);
}

#[test]
fn run_in_a_named_namespace_replaces_that_namespaces_matching_service() {
    let mut current = spec("web");
    current.container.image = "nginx:1".into();
    let mut requested = spec("web");
    requested.container.image = "nginx:2".into();
    let mut owned = container('c', '1', &current, &service_id('a'));
    owned
        .try_update(|parts| parts.namespace = Namespace::parse("shop").unwrap())
        .unwrap();

    let plan = preview_deploy(
        &DeployIntent::apply_one(
            Namespace::parse("shop").unwrap(),
            requested,
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![owned],
            ..Default::default()
        },
    )
    .unwrap();
    match operations(&plan).as_slice() {
        [DeployOperation::ReplaceContainer(replacement)] => {
            assert_eq!(replacement.old_container_id, container_id('c'));
            assert_eq!(replacement.spec.service_id, service_id('a'));
        }
        other => panic!("expected replace of shop/web, got {other:?}"),
    }
}

#[test]
fn run_in_a_named_namespace_does_not_take_over_another_namespaces_service() {
    let mut current = spec("web");
    current.container.image = "nginx:1".into();
    let mut requested = spec("web");
    requested.container.image = "nginx:2".into();
    let mut other = container('c', '1', &current, &service_id('a'));
    other
        .try_update(|parts| parts.namespace = Namespace::parse("default").unwrap())
        .unwrap();

    let plan = preview_deploy(
        &DeployIntent::apply_one(
            Namespace::parse("shop").unwrap(),
            requested,
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![other],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!targets_container(&plan, &container_id('c')));
    assert_eq!(run_names(&plan), ["web"]);
}

#[test]
fn imperative_service_in_a_namespace_is_visible_to_a_later_full_deploy() {
    let web = spec("web");
    let debug = spec("debug");
    let mut web_container = container('c', '1', &web, &service_id('a'));
    web_container
        .try_update(|parts| parts.namespace = Namespace::parse("shop").unwrap())
        .unwrap();
    let mut debug_container = container('d', '1', &debug, &service_id('b'));
    debug_container
        .try_update(|parts| parts.namespace = Namespace::parse("shop").unwrap())
        .unwrap();
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![web_container, debug_container],
        ..Default::default()
    };

    let mut owned: Vec<_> = snapshot
        .services_in(&Namespace::parse("shop").unwrap())
        .iter()
        .map(|service| service.identity.name.as_str().to_owned())
        .collect();
    owned.sort();
    assert_eq!(owned, ["debug", "web"]);

    let plan = preview_deploy(
        &DeployIntent::apply_all(
            Namespace::parse("shop").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &snapshot,
    )
    .unwrap();
    assert!(targets_container(&plan, &container_id('d')));
    assert_eq!(
        plan.would_remove,
        [ployz_core::QualifiedService::parse("shop/debug").unwrap()]
    );
    assert_eq!(plan.prune_refusal, None);
}

#[test]
fn system_namespace_deploy_still_replaces_its_own_ingress() {
    let mut current = spec("ingress");
    current.mode = ServiceMode::Global;
    current.container.image = "caddy:2.9.1".into();
    let mut requested = spec("ingress");
    requested.mode = ServiceMode::Global;
    requested.container.image = "caddy:2.10.2".into();
    let mut system_container = container('c', '1', &current, &service_id('a'));
    system_container
        .try_update(|parts| parts.namespace = Namespace::system())
        .unwrap();

    let plan = preview_deploy(
        &DeployIntent::apply_one(Namespace::system(), requested, PlanOptions::default()),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![system_container],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(targets_container(&plan, &container_id('c')));
}

#[test]
fn unchanged_ingress_follows_a_role_change_on_only_that_server() {
    let ingress = ployz_core::caddy_service_spec("caddy:2.10.2".into(), Default::default());
    let system = |hex, machine_hex| {
        let mut container = container(hex, machine_hex, &ingress, &service_id('a'));
        container
            .try_update(|parts| parts.namespace = Namespace::system())
            .unwrap();
        container
    };
    let mut dropped = machine('2', "dropped");
    dropped.machine.accepts_ingress = false;

    let plan = preview_deploy(
        &DeployIntent::apply_one(Namespace::system(), ingress.clone(), PlanOptions::default()),
        &DeploySnapshot {
            machines: vec![machine('1', "kept"), dropped, machine('3', "gained")],
            containers: vec![system('c', '1'), system('d', '2')],
            ..Default::default()
        },
    )
    .unwrap();

    assert!(!targets_container(&plan, &container_id('c')));
    assert!(targets_container(&plan, &container_id('d')));
    let runs = plan
        .operations
        .iter()
        .filter_map(|row| match &row.operation {
            DeployOperation::RunContainer { machine_id, .. } => Some(*machine_id),
            DeployOperation::PrepareVolumes { .. }
            | DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::ReplaceContainer(_)
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::RemoveVolume { .. } => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(runs, [machine_id('3')]);
}

#[test]
fn cloud_lowering_orders_dependency_before_migration_and_container() {
    let snapshots: Vec<_> = ["web", "db"].into_iter().map(|name| serde_json::json!({
        "serviceId": name,
        "config": {"version": 2, "privateDns": name,
            "env": if name == "web" { serde_json::json!({"DB": {"kind": "literal", "value": "", "parts": [
                {"kind": "ref", "owner": {"scope": "service", "lineageId": "db-lineage"}, "key": "PORT"}]}}) } else { serde_json::json!({}) },
                "healthcheck": {"type":"none"}, "restartPolicy":"on-failure",
            "source": {"type":"image", "version":1, "image":"nginx:latest", "credentials":{"type":"none"}},
            "preDeployCommand": if name == "web" { Some("migrate") } else { None }
        }
    })).collect();
    let intent = ployz_core::config::lower_deployment(
        serde_json::from_value(serde_json::json!({
            "namespace": "app", "snapshots": snapshots,
            "lineages": {"db-lineage": "db"}
        }))
        .unwrap(),
    )
    .unwrap();
    let plan = preview_deploy(&intent, &snapshot()).unwrap();
    assert!(matches!(operations(&plan).as_slice(), [
        DeployOperation::RunContainer { spec: db, .. },
        DeployOperation::RunHook { spec: hook, .. },
        DeployOperation::RunContainer { spec: web, .. },
    ] if db.name.as_str() == "db" && hook.name.as_str() == "web" && web.name.as_str() == "web"));
}

#[test]
fn an_ingress_server_without_a_running_proxy_is_one_whose_ingress_returns() {
    let running = ployz_core::ContainerRuntimeObservation::Running {
        health: ployz_core::HealthObservation::Healthy,
    };
    let exited = ployz_core::ContainerRuntimeObservation::Exited { code: 0 };
    let at = |hex: char, name: &str, last: u8| {
        let mut observed = machine(hex, name);
        observed.machine.public_ip = Some(std::net::IpAddr::from([192, 0, 2, last]));
        observed
    };
    let mut no_web = at('4', "no-web", 4);
    no_web.machine.accepts_ingress = false;
    let unrecorded = machine('5', "unrecorded");
    let snapshot = DeploySnapshot {
        machines: vec![
            at('1', "serving", 1),
            at('2', "missing", 2),
            at('3', "stopped", 3),
            no_web,
            unrecorded,
        ],
        containers: vec![
            ingress_container('a', '1', "caddy:test", running),
            ingress_container('c', '3', "caddy:test", exited),
        ],
        ..Default::default()
    };
    assert_eq!(
        snapshot.ingress_returning(),
        [
            Some(std::net::IpAddr::from([192, 0, 2, 2])),
            Some(std::net::IpAddr::from([192, 0, 2, 3])),
            None
        ]
    );
}
