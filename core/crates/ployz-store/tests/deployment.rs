#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Admission, runner ownership, replay and partial Node Outcomes, through the Store's
//! interface only, on SQLite and on Postgres (see `backend`).

use ployz_core::config::ReviewLifecycleKind;
use ployz_core::{
    DeployOutcome, DeployPreview, ExecutionError, RpcError, RpcErrorCode, ServiceName,
};
use ployz_store::{
    Actor, Admit, Cancel, Change, Command, ConfigStore, CreateProject, CreateService, DeploymentId,
    DeploymentStatus, DeploymentSummary, DeploymentsQuery, DiffQuery, DiffView, Discard, Edit,
    EnvironmentId, EnvironmentRef, NodeStatus, OrganizationId, PlanQuery, ProjectId, ProjectName,
    RemoveService, RenameService, Revision, RunEvidence, RunnerId, ServiceId, ServiceQuery,
    ServicesQuery, SettingPath,
};
use serde_json::{Value, json};

mod backend;

const PROJECT: &str = "00000000-0000-4000-8000-000000000001";
const ENVIRONMENT: &str = "00000000-0000-4000-8000-000000000002";

/// A store with Project `shop` and new Services `web` and `api`.
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor {
        organization: OrganizationId::parse("org").unwrap(),
    };
    store
        .create_project(
            &who,
            &CreateProject {
                id: ProjectId::parse(PROJECT).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(ENVIRONMENT).unwrap(),
            },
        )
        .unwrap();
    for (n, name) in [(3, "web"), (4, "api")] {
        store
            .create_service(
                &who,
                &CreateService {
                    id: ServiceId::parse(format!("00000000-0000-4000-8000-00000000000{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some("nginx:1".into()),
                },
            )
            .unwrap();
    }
    (store, who)
}

fn id(n: u8) -> DeploymentId {
    DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap()
}

fn runner(name: &str) -> RunnerId {
    RunnerId::parse(name).unwrap()
}

fn admit(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    services: &[&str],
    version: Option<String>,
) -> Result<DeploymentSummary, RpcError> {
    store.admit(
        who,
        &Admit {
            id: id(n),
            environment: EnvironmentRef::default(),
            services: services
                .iter()
                .map(|name| ServiceName::parse(*name).unwrap())
                .collect(),
            version,
        },
    )
}

fn diff(store: &ConfigStore, who: &Actor) -> DiffView {
    store.diff(who, &DiffQuery::default()).unwrap()
}

fn changed(store: &ConfigStore, who: &Actor) -> Vec<String> {
    diff(store, who)
        .changes
        .into_iter()
        .map(|change| change.name)
        .collect()
}

fn set_replicas(store: &ConfigStore, who: &Actor, service: &str, replicas: u8) {
    store
        .edit(
            who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse(&format!("{service}.replicas")).unwrap(),
                    value: json!(replicas),
                }],
            },
        )
        .unwrap();
}

/// A distinct operation per Service, attributed to it.
fn operation(service: &str) -> Value {
    let container = if service == "web" { "a" } else { "b" };
    json!({"type": "remove_container", "machine_id": "a".repeat(32), "container_id": container.repeat(64)})
}

fn preview(services: &[&str]) -> DeployPreview {
    serde_json::from_value(json!({
        "namespace": "shop-production",
        "operations": services.iter().enumerate().map(|(index, service)| json!({
            "index": index, "machine_id": "a".repeat(32), "service_name": service,
            "operation": operation(service), "status": {"type": "pending"}
        })).collect::<Vec<_>>(),
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap()
}

fn succeeded(services: &[&str]) -> RunEvidence {
    RunEvidence::Executed(Box::new(outcome(json!({
        "type": "success",
        "completed": services.iter().map(|service| operation(service)).collect::<Vec<_>>()
    }))))
}

fn outcome(value: Value) -> DeployOutcome<ExecutionError> {
    serde_json::from_value(value).unwrap()
}

fn code(result: Result<impl std::fmt::Debug, RpcError>) -> RpcErrorCode {
    result.unwrap_err().code
}

fn nodes(store: &ConfigStore, who: &Actor, n: u8) -> Vec<(String, NodeStatus)> {
    store
        .deployment(who, &id(n))
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| (node.name, node.outcome))
        .collect()
}

#[test]
fn a_deploy_publishes_then_its_runner_records_it_into_applied_state() {
    let (store, who) = shop();
    let admitted = admit(&store, &who, 1, &[], None).unwrap();
    assert_eq!(
        (admitted.status, admitted.saved, admitted.number),
        (DeploymentStatus::Queued, Revision(1), 1)
    );
    // The submitted Deployment is Head now, so nothing is staged.
    assert!(changed(&store, &who).is_empty());

    let a = runner("runner-a");
    let claimed = store.claim(&id(1), &a).unwrap();
    assert_eq!(claimed.deployment.status, DeploymentStatus::Running);
    assert_eq!(claimed.intent.namespace.as_str(), "shop-production");
    assert_eq!(claimed.intent.target.len(), 2);
    assert!(claimed.intent.options.selected.is_empty());
    assert_eq!(store.claim(&id(1), &a).unwrap(), claimed);
    assert_eq!(
        code(store.claim(&id(1), &runner("runner-b"))),
        RpcErrorCode::Conflict
    );

    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    // Recording the same evidence twice changes nothing; another runner never may.
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    assert_eq!(
        code(store.record(&id(1), &runner("runner-b"), succeeded(&["web", "api"]))),
        RpcErrorCode::Conflict
    );
    let recorded = store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    assert_eq!(
        recorded,
        store
            .record(&id(1), &a, succeeded(&["web", "api"]))
            .unwrap()
    );
    assert_eq!(recorded.status, DeploymentStatus::Applied);
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Applied),
            ("api".to_owned(), NodeStatus::Applied)
        ]
    );
    assert!(store.deployment(&who, &id(1)).unwrap().preview.is_some());
    assert!(changed(&store, &who).is_empty());
    assert_eq!(code(store.claim(&id(1), &a)), RpcErrorCode::Conflict);

    // Applied State is the new Head: a later edit shows against it.
    set_replicas(&store, &who, "web", 3);
    assert_eq!(changed(&store, &who), ["web"]);
}

#[test]
fn a_partial_outcome_applies_only_confirmed_nodes() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    // Recording an outcome before its preview is refused.
    assert_eq!(
        code(store.record(&id(1), &a, succeeded(&["web"]))),
        RpcErrorCode::Conflict
    );
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    // Evidence that doesn't match the preview is refused.
    assert_eq!(
        code(store.record(&id(1), &a, succeeded(&["web"]))),
        RpcErrorCode::InvalidArgument
    );
    let partial = RunEvidence::Executed(Box::new(outcome(json!({
        "type": "failed", "completed": [operation("web")],
        "failed": {"type": "operation", "operation": operation("api"), "error": {"type": "cancelled"}},
        "unexecuted": []
    }))));
    store.record(&id(1), &a, partial).unwrap();
    assert_eq!(
        store.deployment(&who, &id(1)).unwrap().deployment.status,
        DeploymentStatus::Failed
    );
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Applied),
            ("api".to_owned(), NodeStatus::NotApplied)
        ]
    );
    // Only `api` is still to deploy; a different outcome can't overwrite this one.
    assert_eq!(changed(&store, &who), ["api"]);
    assert_eq!(
        code(store.record(&id(1), &a, succeeded(&["web", "api"]))),
        RpcErrorCode::Conflict
    );
}

#[test]
fn nothing_executed_applies_nothing() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(
            &id(1),
            &a,
            RunEvidence::NotExecuted("No Server answered".into()),
        )
        .unwrap();
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::NotApplied),
            ("api".to_owned(), NodeStatus::NotApplied)
        ]
    );
    assert_eq!(changed(&store, &who), ["web", "api"]);
}

#[test]
fn admission_replays_supersedes_and_checks_the_reviewed_version() {
    let (store, who) = shop();
    let first = admit(&store, &who, 1, &[], None).unwrap();
    assert_eq!(admit(&store, &who, 1, &[], None).unwrap(), first);
    assert_eq!(
        code(admit(&store, &who, 1, &["web"], None)),
        RpcErrorCode::Conflict
    );

    // The newest admission replaces the pending one.
    let reviewed = diff(&store, &who).version;
    set_replicas(&store, &who, "web", 2);
    let stale = admit(&store, &who, 2, &[], Some(reviewed)).unwrap_err();
    assert_eq!(stale.code, RpcErrorCode::Conflict);
    assert!(stale.details.get("diff").is_some());
    let second = admit(&store, &who, 2, &[], Some(diff(&store, &who).version)).unwrap();
    assert_eq!((second.number, second.saved), (2, Revision(2)));
    assert_eq!(
        store.deployment(&who, &id(1)).unwrap().deployment.status,
        DeploymentStatus::Superseded
    );
    assert_eq!(
        code(store.claim(&id(1), &runner("runner-a"))),
        RpcErrorCode::Conflict
    );
}

#[test]
fn a_new_runner_leaves_the_replaced_deployment_unknown() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    admit(&store, &who, 2, &[], None).unwrap();
    store.claim(&id(2), &runner("runner-b")).unwrap();
    let replaced = store.deployment(&who, &id(1)).unwrap();
    assert_eq!(replaced.deployment.status, DeploymentStatus::Unknown);
    assert!(
        replaced
            .nodes
            .iter()
            .all(|node| node.outcome == NodeStatus::Unknown)
    );
    assert_eq!(
        code(store.record(&id(1), &a, RunEvidence::Prepared(preview(&["web"])))),
        RpcErrorCode::Conflict
    );
}

fn cancel(store: &ConfigStore, who: &Actor, n: u8) -> Result<DeploymentSummary, RpcError> {
    store.cancel(who, &Cancel { deployment: id(n) })
}

fn status(store: &ConfigStore, who: &Actor, n: u8) -> DeploymentStatus {
    store.deployment(who, &id(n)).unwrap().deployment.status
}

#[test]
fn a_cancelled_queued_deployment_never_runs() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let other = Actor {
        organization: OrganizationId::parse("other").unwrap(),
    };
    assert_eq!(code(cancel(&store, &other, 1)), RpcErrorCode::NotFound);
    assert_eq!(
        cancel(&store, &who, 1).unwrap().status,
        DeploymentStatus::Cancelled
    );
    // Cancelling again changes nothing; its runner finds nothing to run.
    assert_eq!(
        cancel(&store, &who, 1).unwrap().status,
        DeploymentStatus::Cancelled
    );
    assert_eq!(
        code(store.claim(&id(1), &runner("runner-a"))),
        RpcErrorCode::Conflict
    );
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::NotApplied),
            ("api".to_owned(), NodeStatus::NotApplied)
        ]
    );
    assert_eq!(changed(&store, &who), ["web", "api"]);
}

#[test]
fn a_cancelled_running_deployment_keeps_its_confirmed_node_outcomes() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    assert_eq!(
        cancel(&store, &who, 1).unwrap().status,
        DeploymentStatus::Cancelling
    );
    // Its runner stops it partway and records what ran.
    let stopped = RunEvidence::Executed(Box::new(outcome(json!({
        "type": "failed", "completed": [operation("web")],
        "failed": {"type": "operation", "operation": operation("api"), "error": {"type": "cancelled"}},
        "unexecuted": []
    }))));
    store.record(&id(1), &a, stopped).unwrap();
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Cancelled);
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Applied),
            ("api".to_owned(), NodeStatus::NotApplied)
        ]
    );
    assert_eq!(changed(&store, &who), ["api"]);
}

#[test]
fn a_runner_that_loses_track_after_preparing_leaves_the_outcome_unknown() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    // Its step is retried: it may have executed, so nothing replays.
    let retried = store.claim(&id(1), &a).unwrap_err();
    assert_eq!(retried.code, RpcErrorCode::Conflict);
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Unknown);
    assert!(
        nodes(&store, &who, 1)
            .iter()
            .all(|(_, outcome)| *outcome == NodeStatus::Unknown)
    );
    assert_eq!(
        code(store.record(&id(1), &a, succeeded(&["web", "api"]))),
        RpcErrorCode::Conflict
    );
    assert_eq!(changed(&store, &who), ["web", "api"]);

    // Abandoned after preparing: unknown too.
    admit(&store, &who, 2, &[], None).unwrap();
    store.claim(&id(2), &a).unwrap();
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store.record(&id(2), &a, RunEvidence::Abandoned).unwrap();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Unknown);
    store.record(&id(2), &a, RunEvidence::Abandoned).unwrap();
}

#[test]
fn a_runner_that_stops_before_preparing_executed_nothing() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let a = runner("runner-a");
    // Only its owner may abandon it, once it claimed it.
    assert_eq!(
        code(store.record(&id(1), &a, RunEvidence::Abandoned)),
        RpcErrorCode::Conflict
    );
    store.claim(&id(1), &a).unwrap();
    store.record(&id(1), &a, RunEvidence::Abandoned).unwrap();
    let view = store.deployment(&who, &id(1)).unwrap();
    assert_eq!(view.deployment.status, DeploymentStatus::Failed);
    assert!(matches!(
        view.outcome,
        Some(ployz_store::Outcome::NotExecuted { .. })
    ));

    // Abandoning after an outcome changes nothing.
    admit(&store, &who, 2, &[], None).unwrap();
    store.claim(&id(2), &a).unwrap();
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(2), &a, succeeded(&["web", "api"]))
        .unwrap();
    store.record(&id(2), &a, RunEvidence::Abandoned).unwrap();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Applied);
    // An ended Deployment can't be cancelled.
    assert_eq!(code(cancel(&store, &who, 2)), RpcErrorCode::Conflict);
}

#[test]
fn a_targeted_deploy_and_its_plan_cover_only_the_named_services() {
    let (store, who) = shop();
    let plan = store
        .plan(
            &who,
            &PlanQuery {
                services: vec![ServiceName::parse("web").unwrap()],
                ..PlanQuery::default()
            },
        )
        .unwrap();
    assert_eq!(plan.version, diff(&store, &who).version);
    assert_eq!(
        plan.changes
            .iter()
            .map(|change| change.name.as_str())
            .collect::<Vec<_>>(),
        ["web"]
    );
    assert_eq!(plan.unresolved, ["operations"]);
    assert_eq!(plan.namespace.as_str(), "shop-production");
    assert_eq!(
        code(admit(&store, &who, 1, &["nope"], None)),
        RpcErrorCode::NotFound
    );

    admit(&store, &who, 1, &["web"], None).unwrap();
    let claimed = store.claim(&id(1), &runner("runner-a")).unwrap();
    assert_eq!(
        claimed
            .intent
            .options
            .selected
            .iter()
            .map(|attempt| attempt.name.as_str())
            .collect::<Vec<_>>(),
        ["web"]
    );
    assert_eq!(changed(&store, &who), ["api"]);
}

#[test]
fn deployments_page_newest_first_within_the_organization() {
    let (store, who) = shop();
    for n in 1..=3 {
        admit(&store, &who, n, &[], None).unwrap();
    }
    let page = |cursor: Option<String>| {
        store
            .deployments(
                &who,
                &DeploymentsQuery {
                    limit: Some(2),
                    cursor,
                    ..DeploymentsQuery::default()
                },
            )
            .unwrap()
    };
    let first = page(None);
    assert_eq!(
        first
            .deployments
            .iter()
            .map(|d| d.number)
            .collect::<Vec<_>>(),
        [3, 2]
    );
    let last = page(first.next_cursor.clone());
    assert_eq!(
        last.deployments
            .iter()
            .map(|d| d.number)
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(last.next_cursor, None);
    let bad_limit = store.deployments(
        &who,
        &DeploymentsQuery {
            limit: Some(0),
            ..DeploymentsQuery::default()
        },
    );
    assert_eq!(code(bad_limit), RpcErrorCode::InvalidArgument);
    let stranger = Actor {
        organization: OrganizationId::parse("other").unwrap(),
    };
    assert_eq!(
        code(store.deployment(&stranger, &id(1))),
        RpcErrorCode::NotFound
    );
}

#[test]
fn a_staged_removal_leaves_applied_state_until_its_deploy_confirms_it() {
    let (store, who) = shop();
    let a = runner("runner-a");
    admit(&store, &who, 1, &[], None).unwrap();
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();

    let remove = RemoveService {
        environment: EnvironmentRef::default(),
        service: ServiceName::parse("web").unwrap(),
    };
    store.remove_service(&who, &remove).unwrap();
    // Nothing ran: `web` is still applied, and lists as removed by the next Deploy.
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Applied),
            ("api".to_owned(), NodeStatus::Applied)
        ]
    );
    let listed = store.services(&who, &ServicesQuery::default()).unwrap();
    assert_eq!(
        listed
            .services
            .iter()
            .map(|listing| (listing.service.name.as_str(), listing.change))
            .collect::<Vec<_>>(),
        [("api", None), ("web", Some(ReviewLifecycleKind::Delete))]
    );
    assert_eq!(changed(&store, &who), ["web"]);
    let removed = store
        .service(
            &who,
            &ServiceQuery {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(removed.values["image"], json!("nginx:1"));

    // The Deploy removes it; once confirmed, it is gone.
    admit(&store, &who, 2, &[], None).unwrap();
    let claimed = store.claim(&id(2), &a).unwrap();
    assert_eq!(claimed.intent.target.len(), 1);
    store
        .record(&id(2), &a, RunEvidence::Prepared(preview(&["web"])))
        .unwrap();
    store.record(&id(2), &a, succeeded(&["web"])).unwrap();
    let listed = store.services(&who, &ServicesQuery::default()).unwrap();
    assert_eq!(listed.services.len(), 1);
    assert!(changed(&store, &who).is_empty());
}

#[test]
fn renaming_a_deployed_service_is_a_staged_change_discard_undoes() {
    let (store, who) = shop();
    let a = runner("runner-a");
    admit(&store, &who, 1, &[], None).unwrap();
    store.claim(&id(1), &a).unwrap();
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    store
        .write(
            &who,
            &Command::RenameService(RenameService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
                name: ServiceName::parse("front").unwrap(),
            }),
        )
        .unwrap();
    let change = diff(&store, &who).changes.remove(0);
    assert_eq!(
        (change.name.as_str(), change.settings[0].path.as_str()),
        ("front", "front.name")
    );
    assert_eq!(
        (&change.settings[0].before, &change.settings[0].after),
        (&json!("web"), &json!("front"))
    );

    store
        .discard(
            &who,
            &Discard {
                environment: EnvironmentRef::default(),
                path: Some(SettingPath::parse("front").unwrap()),
                version: None,
            },
        )
        .unwrap();
    assert!(changed(&store, &who).is_empty());
}
