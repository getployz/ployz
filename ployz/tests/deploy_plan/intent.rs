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
