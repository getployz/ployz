use std::collections::BTreeMap;

use super::support::*;
use ployz::deploy::{IngressContext, preview_deploy};
use ployz_core::{
    ConfiguredHealthcheck, DependencyCondition, DeployWarning, HealthcheckCommand, HealthcheckSpec,
    PreDeployHook, ServiceDependency, ServiceName,
};

#[test]
fn empty_selected_plans_every_target_service_in_dependency_order() {
    let (db, web, worker, dependencies) = web_db_worker();
    let intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&db, &web, &worker],
        PlanOptions::default(),
    )
    .with_dependencies(dependencies);
    let plan = preview_deploy(&intent, &snapshot(), IngressContext::default()).unwrap();
    assert_eq!(run_names(&plan), ["db", "web", "worker"]);
}

#[test]
fn apply_web_plans_web_and_db_not_worker() {
    let (db, web, mut worker, dependencies) = web_db_worker();
    worker.container.image = "ghcr.io/getployz/worker:old".into();
    let intent = DeployIntent::new(
        ProjectName::parse("app").unwrap(),
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
    let plan = preview_deploy(&intent, &snapshot, IngressContext::default()).unwrap();
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
            ProjectName::parse("app").unwrap(),
            spec("caddy"),
            PlanOptions::default(),
        ),
        &snapshot(),
        IngressContext::default(),
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
        ProjectName::parse("app").unwrap(),
        vec![db, web],
        PlanOptions {
            selected: vec![attempt("web")],
            ..PlanOptions::default()
        },
    )
    .with_dependencies(dependencies);
    assert!(matches!(
        preview_deploy(&intent, &snapshot(), IngressContext::default()),
        Err(PlanError::DependencyCycle { service }) if service == "db"
    ));
}

#[test]
fn skip_health_on_options_is_set_on_planned_operations() {
    let plan = preview_deploy(
        &DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
            spec("api"),
            PlanOptions {
                skip_health_monitor: true,
                ..Default::default()
            },
        ),
        &snapshot(),
        IngressContext::default(),
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
        command: vec!["migrate".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    let intent = DeployIntent::new(
        ProjectName::parse("app").unwrap(),
        vec![db.clone(), web.clone()],
        PlanOptions {
            selected: vec![attempt("web")],
            ..PlanOptions::default()
        },
    )
    .with_dependencies(BTreeMap::from([(
        web.name.clone(),
        vec![ServiceDependency {
            service: db.name.clone(),
            condition: DependencyCondition::ServiceHealthy,
        }],
    )]));

    let plan = preview_deploy(&intent, &snapshot(), IngressContext::default()).unwrap();
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
        ProjectName::parse("app").unwrap(),
        [&db, &web],
        PlanOptions::default(),
    )
    .with_dependencies(BTreeMap::from([(
        web.name.clone(),
        vec![ServiceDependency {
            service: db.name.clone(),
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
        IngressContext::default(),
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
        ProjectName::parse("app").unwrap(),
        vec![db.clone(), web.clone()],
        PlanOptions {
            skip_health_monitor: true,
            ..PlanOptions::default()
        },
    )
    .with_dependencies(BTreeMap::from([(
        web.name.clone(),
        vec![ServiceDependency {
            service: db.name,
            condition: DependencyCondition::ServiceHealthy,
        }],
    )]));

    let plan = preview_deploy(&intent, &snapshot(), IngressContext::default()).unwrap();
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

fn spec(name: &str) -> RequestedServiceSpec {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.name = ServiceName::parse(name).unwrap();
    requested
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
            DeployOperation::CreateVolume { .. }
            | DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::ReplaceContainer(_)
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
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
        DeployOperation::CreateVolume { .. }
        | DeployOperation::WaitHealthy { .. }
        | DeployOperation::RunContainer { .. }
        | DeployOperation::RunHook { .. }
        | DeployOperation::RemoveVolume { .. } => false,
    })
}

#[test]
fn user_project_deploy_does_not_replace_or_remove_system_caddy() {
    let mut system_caddy = spec("caddy");
    system_caddy.mode = ServiceMode::Global;
    system_caddy.container.image = "caddy:2.9.1".into();
    let mut shop_caddy = spec("caddy");
    shop_caddy.mode = ServiceMode::Global;
    shop_caddy.container.image = "caddy:2.10.2".into();
    let mut system_container = container('c', '1', &system_caddy, &service_id('a'));
    system_container.project_name = ProjectName::system();

    let shop = preview_deploy(
        &DeployIntent::apply_one(
            ProjectName::parse("shop").unwrap(),
            shop_caddy,
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![system_container],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();
    assert!(!targets_container(&shop, &container_id('c')));
    assert_eq!(run_names(&shop), ["caddy"]);

    let web = spec("web");
    let mut leftover = container('c', '1', &system_caddy, &service_id('a'));
    leftover.project_name = ProjectName::system();
    let full = preview_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("shop").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![leftover],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();
    assert!(!targets_container(&full, &container_id('c')));
    assert_eq!(run_names(&full), ["web"]);
}

#[test]
fn run_in_a_named_project_replaces_that_projects_matching_service() {
    let mut current = spec("web");
    current.container.image = "nginx:1".into();
    let mut requested = spec("web");
    requested.container.image = "nginx:2".into();
    let mut owned = container('c', '1', &current, &service_id('a'));
    owned.project_name = ProjectName::parse("shop").unwrap();

    let plan = preview_deploy(
        &DeployIntent::apply_one(
            ProjectName::parse("shop").unwrap(),
            requested,
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![owned],
            ..Default::default()
        },
        IngressContext::default(),
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
fn run_in_a_named_project_does_not_take_over_another_projects_service() {
    let mut current = spec("web");
    current.container.image = "nginx:1".into();
    let mut requested = spec("web");
    requested.container.image = "nginx:2".into();
    let mut other = container('c', '1', &current, &service_id('a'));
    other.project_name = ProjectName::parse("default").unwrap();

    let plan = preview_deploy(
        &DeployIntent::apply_one(
            ProjectName::parse("shop").unwrap(),
            requested,
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![other],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();
    assert!(!targets_container(&plan, &container_id('c')));
    assert_eq!(run_names(&plan), ["web"]);
}

#[test]
fn imperative_service_in_a_project_is_visible_to_a_later_full_deploy() {
    let web = spec("web");
    let debug = spec("debug");
    let mut web_container = container('c', '1', &web, &service_id('a'));
    web_container.project_name = ProjectName::parse("shop").unwrap();
    let mut debug_container = container('d', '1', &debug, &service_id('b'));
    debug_container.project_name = ProjectName::parse("shop").unwrap();
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![web_container, debug_container],
        ..Default::default()
    };

    let mut owned: Vec<_> = snapshot
        .services_in(&ProjectName::parse("shop").unwrap())
        .iter()
        .map(|service| service.identity.name.as_str().to_owned())
        .collect();
    owned.sort();
    assert_eq!(owned, ["debug", "web"]);

    let plan = preview_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("shop").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &snapshot,
        IngressContext::default(),
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
fn system_project_deploy_still_replaces_its_own_caddy() {
    let mut current = spec("caddy");
    current.mode = ServiceMode::Global;
    current.container.image = "caddy:2.9.1".into();
    let mut requested = spec("caddy");
    requested.mode = ServiceMode::Global;
    requested.container.image = "caddy:2.10.2".into();
    let mut system_container = container('c', '1', &current, &service_id('a'));
    system_container.project_name = ProjectName::system();

    let plan = preview_deploy(
        &DeployIntent::apply_one(ProjectName::system(), requested, PlanOptions::default()),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![system_container],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();
    assert!(targets_container(&plan, &container_id('c')));
}
