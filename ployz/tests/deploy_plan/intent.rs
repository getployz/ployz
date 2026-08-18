use std::collections::BTreeMap;

use super::support::*;
use ployz::deploy::plan_deploy;
use ployz_core::ServiceName;

#[test]
fn empty_apply_with_nonempty_target_produces_an_empty_plan() {
    let web = spec("web");
    let plan = plan_deploy(
        &DeployIntent::new(
            ProjectName::parse("app").unwrap(),
            vec![web],
            Vec::new(),
            PlanOptions::default(),
        ),
        &snapshot(),
    )
    .unwrap();
    assert!(plan.operations.is_empty());
}

#[test]
fn apply_all_names_plans_every_service_in_dependency_order() {
    let (db, web, worker, dependencies) = web_db_worker();
    let intent = DeployIntent::new(
        ProjectName::parse("app").unwrap(),
        vec![db, web, worker],
        vec![attempt("db"), attempt("web"), attempt("worker")],
        PlanOptions::default(),
    )
    .with_dependencies(dependencies);
    let plan = plan_deploy(&intent, &snapshot()).unwrap();
    assert_eq!(run_names(&plan), ["db", "web", "worker"]);
}

#[test]
fn apply_web_plans_web_and_db_not_worker() {
    let (db, web, mut worker, dependencies) = web_db_worker();
    worker.container.image = "ghcr.io/getployz/worker:old".into();
    let intent = DeployIntent::new(
        ProjectName::parse("app").unwrap(),
        vec![db, web, spec("worker")],
        vec![attempt("web")],
        PlanOptions::default(),
    )
    .with_dependencies(dependencies);
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![container('c', '1', &worker, &service_id('a'))],
        ..Default::default()
    };
    let plan = plan_deploy(&intent, &snapshot).unwrap();
    assert_eq!(run_names(&plan), ["db", "web"]);
    assert!(!plan.operations.iter().any(|operation| {
        matches!(
            operation,
            DeployOperation::ReplaceContainer(replacement)
                if replacement.old_container_id == container_id('c')
        )
    }));
}

#[test]
fn one_spec_intent_plans_that_name() {
    let plan = plan_deploy(
        &DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
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
        (web.name.clone(), vec![db.name.clone()]),
        (db.name.clone(), vec![web.name.clone()]),
    ]);
    let intent = DeployIntent::new(
        ProjectName::parse("app").unwrap(),
        vec![db, web],
        vec![attempt("web")],
        PlanOptions::default(),
    )
    .with_dependencies(dependencies);
    assert!(matches!(
        plan_deploy(&intent, &snapshot()),
        Err(PlanError::DependencyCycle { service }) if service == "db"
    ));
}

#[test]
fn skip_health_on_options_is_set_on_planned_operations() {
    let plan = plan_deploy(
        &DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
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
        plan.operations.as_slice(),
        [DeployOperation::RunContainer {
            skip_health_monitor: true,
            ..
        }]
    ));
}

fn web_db_worker() -> (
    RequestedServiceSpec,
    RequestedServiceSpec,
    RequestedServiceSpec,
    BTreeMap<ServiceName, Vec<ServiceName>>,
) {
    let db = spec("db");
    let web = spec("web");
    let worker = spec("worker");
    let dependencies = BTreeMap::from([(web.name.clone(), vec![db.name.clone()])]);
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

fn run_names(plan: &ployz::deploy::DeployPlan) -> Vec<&str> {
    plan.operations
        .iter()
        .filter_map(|operation| match operation {
            DeployOperation::RunContainer { spec, .. } => Some(spec.name.as_str()),
            DeployOperation::CreateVolume { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::ReplaceContainer(_)
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. } => None,
        })
        .collect()
}

fn targets_container(plan: &ployz::deploy::DeployPlan, id: &ContainerId) -> bool {
    plan.operations.iter().any(|operation| match operation {
        DeployOperation::StopContainer { container_id, .. }
        | DeployOperation::RemoveContainer { container_id, .. }
        | DeployOperation::StopHook { container_id, .. } => container_id == id,
        DeployOperation::ReplaceContainer(replacement) => &replacement.old_container_id == id,
        DeployOperation::CreateVolume { .. }
        | DeployOperation::RunContainer { .. }
        | DeployOperation::RunHook { .. } => false,
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

    let shop = plan_deploy(
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
    )
    .unwrap();
    assert!(!targets_container(&shop, &container_id('c')));
    assert_eq!(run_names(&shop), ["caddy"]);

    let web = spec("web");
    let mut leftover = container('c', '1', &system_caddy, &service_id('a'));
    leftover.project_name = ProjectName::system();
    let full = plan_deploy(
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

    let plan = plan_deploy(
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
    )
    .unwrap();
    match plan.operations.as_slice() {
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

    let plan = plan_deploy(
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
        .filter_map(|service| service.service_name().map(|name| name.as_str().to_owned()))
        .collect();
    owned.sort();
    assert_eq!(owned, ["debug", "web"]);

    let plan = plan_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("shop").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &snapshot,
    )
    .unwrap();
    assert!(!targets_container(&plan, &container_id('d')));
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

    let plan = plan_deploy(
        &DeployIntent::apply_one(ProjectName::system(), requested, PlanOptions::default()),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![system_container],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(targets_container(&plan, &container_id('c')));
}
