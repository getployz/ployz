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
    Actor, Admit, Cancel, Change, Command, ConfigStore, CreateProject, CreateService, Deploy,
    DeploymentId, DeploymentStatus, DeploymentSummary, DeploymentsQuery, DiffQuery, DiffView,
    Discard, Edit, EnvironmentId, EnvironmentRef, NamespaceQuery, NodeStatus, OrganizationId,
    PlanQuery, Principal, ProjectId, ProjectName, Query, RemoveService, RenameService, Retry,
    Revision, RunEvidence, RunnerId, ServiceLineageId, ServiceQuery, ServicesQuery, SettingPath,
    Start, Trusted, UploadBase, UploadedSource, View, Written,
};
use serde_json::{Value, json};

mod backend;

const PROJECT: &str = "00000000-0000-4000-8000-000000000001";
const ENVIRONMENT: &str = "00000000-0000-4000-8000-000000000002";

/// A store with Project `shop` and new Services `web` and `api`.
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
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
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-00000000000{n}"))
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
    store.write_trusted(
        who,
        &Admit::Deploy(Deploy {
            id: id(n),
            environment: EnvironmentRef::default(),
            services: services
                .iter()
                .map(|name| ServiceName::parse(*name).unwrap())
                .collect(),
            version,
            upload: None,
            accept_volume_loss: Vec::new(),
            message: None,
        }),
        &Trusted::default(),
    )
}

fn diff(store: &ConfigStore, who: &Actor) -> DiffView {
    store.read(who, &DiffQuery::default()).unwrap()
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
        .write(
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
    RunEvidence::Executed {
        outcome: Box::new(outcome(json!({
            "type": "success",
            "completed": services.iter().map(|service| operation(service)).collect::<Vec<_>>()
        }))),
        removed: Vec::new(),
    }
}

fn outcome(value: Value) -> DeployOutcome<ExecutionError> {
    serde_json::from_value(value).unwrap()
}

fn code(result: Result<impl std::fmt::Debug, RpcError>) -> RpcErrorCode {
    result.unwrap_err().code
}

fn nodes(store: &ConfigStore, who: &Actor, n: u8) -> Vec<(String, NodeStatus)> {
    store
        .read(who, &ployz_store::DeploymentQuery { id: id(n) })
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| (node.node.name().to_owned(), node.outcome))
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
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Deployed)
        ]
    );
    assert!(
        store
            .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
            .unwrap()
            .preview
            .is_some()
    );
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
    let partial = RunEvidence::Executed {
        outcome: Box::new(outcome(json!({
            "type": "failed", "completed": [operation("web")],
            "failed": {"type": "operation", "operation": operation("api"), "error": {
                "type": "machine", "action": "RemoveContainer",
                "error": {"code": "internal", "message": "the daemon is busy", "details": {}}
            }},
            "unexecuted": []
        }))),
        removed: Vec::new(),
    };
    store.record(&id(1), &a, partial).unwrap();
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
    assert_eq!(view.deployment.status, DeploymentStatus::Failed);
    // The Deployment says why, in words users read.
    let Some(ployz_store::Outcome::Executed { reason, .. }) = view.outcome else {
        panic!("an executed outcome");
    };
    assert_eq!(
        reason.as_deref(),
        Some("remove Container failed: the daemon is busy")
    );
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Failed)
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
            ("web".to_owned(), NodeStatus::NotAttempted),
            ("api".to_owned(), NodeStatus::NotAttempted)
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
        store
            .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
            .unwrap()
            .deployment
            .status,
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
    let replaced = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
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
    store.write(who, &Cancel { deployment: id(n) })
}

fn status(store: &ConfigStore, who: &Actor, n: u8) -> DeploymentStatus {
    store
        .read(who, &ployz_store::DeploymentQuery { id: id(n) })
        .unwrap()
        .deployment
        .status
}

#[test]
fn a_cancelled_queued_deployment_never_runs() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let other = Actor::system(OrganizationId::parse("other").unwrap());
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
            ("web".to_owned(), NodeStatus::NotAttempted),
            ("api".to_owned(), NodeStatus::NotAttempted)
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
    let stopped = RunEvidence::Executed {
        outcome: Box::new(outcome(json!({
            "type": "failed", "completed": [operation("web")],
            "failed": {"type": "operation", "operation": operation("api"), "error": {"type": "cancelled"}},
            "unexecuted": []
        }))),
        removed: Vec::new(),
    };
    store.record(&id(1), &a, stopped).unwrap();
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Cancelled);
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Failed)
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
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
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

fn retry(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    source: u8,
) -> Result<DeploymentSummary, RpcError> {
    store.write_trusted(
        who,
        &Admit::Retry(Retry {
            id: id(n),
            deployment: id(source),
        }),
        &ployz_store::Trusted::default(),
    )
}

fn start(store: &ConfigStore, who: &Actor, n: u8) -> Result<DeploymentSummary, RpcError> {
    store.write(who, &Start { deployment: id(n) })
}

/// Deployment `n` claimed by `runner-a`, which fails `api` after applying `web`.
fn fail_api(store: &ConfigStore, n: u8) -> ployz_core::DeployIntent {
    let a = runner("runner-a");
    let intent = store.claim(&id(n), &a).unwrap().intent;
    store
        .record(&id(n), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    let failed = RunEvidence::Executed {
        outcome: Box::new(outcome(json!({
            "type": "failed", "completed": [operation("web")],
            "failed": {"type": "operation", "operation": operation("api"), "error": {"type": "cancelled"}},
            "unexecuted": []
        }))),
        removed: Vec::new(),
    };
    store.record(&id(n), &a, failed).unwrap();
    intent
}

#[test]
fn a_retry_ships_exactly_what_the_failed_deployment_froze() {
    let (store, who) = shop();
    let first = admit(&store, &who, 1, &[], None).unwrap();
    let frozen = fail_api(&store, 1);
    // A newer Saved revision, admitted and cancelled before it ran.
    set_replicas(&store, &who, "web", 3);
    admit(&store, &who, 2, &[], None).unwrap();
    cancel(&store, &who, 2).unwrap();

    let retried = retry(&store, &who, 3, 1).unwrap();
    assert_eq!(
        (
            retried.number,
            retried.status,
            retried.saved,
            retried.services
        ),
        (3, DeploymentStatus::Queued, first.saved, first.services)
    );
    // Replaying it is idempotent; its ID with another source is not.
    assert_eq!(retry(&store, &who, 3, 1).unwrap().number, 3);
    assert_eq!(code(retry(&store, &who, 3, 2)), RpcErrorCode::Conflict);
    // The failed one keeps its Node Outcomes; the retry targets every node again.
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Failed)
        ]
    );
    assert!(
        nodes(&store, &who, 3)
            .iter()
            .all(|(_, outcome)| *outcome == NodeStatus::Pending)
    );
    let claimed = store.claim(&id(3), &runner("runner-b")).unwrap();
    assert_eq!(claimed.intent, frozen);
    assert_eq!(
        store
            .read(&who, &ployz_store::DeploymentQuery { id: id(3) })
            .unwrap()
            .namespace
            .as_str(),
        "shop-production"
    );
    // A cancelled Deployment can be retried too, with its own Saved revision.
    let newer = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(2) })
        .unwrap()
        .deployment
        .saved;
    assert_ne!(newer, first.saved);
    assert_eq!(retry(&store, &who, 4, 2).unwrap().saved, newer);
}

#[test]
fn a_retry_is_refused_unless_its_deployment_ended_without_applying() {
    let (store, who) = shop();
    let other = Actor::system(OrganizationId::parse("other").unwrap());
    admit(&store, &who, 1, &[], None).unwrap();
    assert_eq!(code(retry(&store, &other, 9, 1)), RpcErrorCode::NotFound);
    assert_eq!(code(retry(&store, &who, 9, 8)), RpcErrorCode::NotFound);
    // Queued, running or cancelling: not ended yet.
    assert_eq!(code(retry(&store, &who, 9, 1)), RpcErrorCode::Conflict);
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    assert_eq!(code(retry(&store, &who, 9, 1)), RpcErrorCode::Conflict);
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    cancel(&store, &who, 1).unwrap();
    assert_eq!(code(retry(&store, &who, 9, 1)), RpcErrorCode::Conflict);
    // Its runner finished before it saw the cancel: it applied, so nothing to retry.
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    assert_eq!(status(&store, &who, 1), DeploymentStatus::Applied);
    assert_eq!(code(retry(&store, &who, 9, 1)), RpcErrorCode::Conflict);
    // A superseded one never ran: deploy again instead.
    admit(&store, &who, 2, &[], None).unwrap();
    admit(&store, &who, 3, &[], None).unwrap();
    assert_eq!(status(&store, &who, 2), DeploymentStatus::Superseded);
    assert_eq!(code(retry(&store, &who, 9, 2)), RpcErrorCode::Conflict);
    // A refused retry queued nothing, and an accepted one supersedes what's queued.
    fail_api(&store, 3);
    assert_eq!(retry(&store, &who, 9, 3).unwrap().number, 4);
    assert_eq!(status(&store, &who, 3), DeploymentStatus::Failed);
    admit(&store, &who, 5, &[], None).unwrap();
    retry(&store, &who, 6, 3).unwrap();
    assert_eq!(status(&store, &who, 5), DeploymentStatus::Superseded);
    assert_eq!(status(&store, &who, 9), DeploymentStatus::Superseded);
}

#[test]
fn only_a_queued_deployment_starts() {
    let (store, who) = shop();
    let other = Actor::system(OrganizationId::parse("other").unwrap());
    admit(&store, &who, 1, &[], None).unwrap();
    assert_eq!(code(start(&store, &other, 1)), RpcErrorCode::NotFound);
    // Starting changes nothing: a runner still claims it.
    assert_eq!(
        start(&store, &who, 1).unwrap().status,
        DeploymentStatus::Queued
    );
    assert_eq!(
        start(&store, &who, 1).unwrap().status,
        DeploymentStatus::Queued
    );
    let a = runner("runner-a");
    store.claim(&id(1), &a).unwrap();
    assert_eq!(code(start(&store, &who, 1)), RpcErrorCode::Conflict);
    store
        .record(&id(1), &a, RunEvidence::Prepared(preview(&["web", "api"])))
        .unwrap();
    store
        .record(&id(1), &a, succeeded(&["web", "api"]))
        .unwrap();
    assert_eq!(code(start(&store, &who, 1)), RpcErrorCode::Conflict);
    admit(&store, &who, 2, &[], None).unwrap();
    admit(&store, &who, 3, &[], None).unwrap();
    assert_eq!(code(start(&store, &who, 2)), RpcErrorCode::Conflict);
    cancel(&store, &who, 3).unwrap();
    assert_eq!(code(start(&store, &who, 3)), RpcErrorCode::Conflict);
}

#[test]
fn a_targeted_deploy_and_its_plan_cover_only_the_named_services() {
    let (store, who) = shop();
    let plan = store
        .read(
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
            .read(
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
    let bad_limit = store.read(
        &who,
        &DeploymentsQuery {
            limit: Some(0),
            ..DeploymentsQuery::default()
        },
    );
    assert_eq!(code(bad_limit), RpcErrorCode::InvalidArgument);
    let stranger = Actor::system(OrganizationId::parse("other").unwrap());
    assert_eq!(
        code(store.read(&stranger, &ployz_store::DeploymentQuery { id: id(1) })),
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
    store.write(&who, &remove).unwrap();
    // Nothing ran: `web` is still applied, and lists as removed by the next Deploy.
    assert_eq!(
        nodes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Deployed),
            ("api".to_owned(), NodeStatus::Deployed)
        ]
    );
    let listed = store.read(&who, &ServicesQuery::default()).unwrap();
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
        .read(
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
    let listed = store.read(&who, &ServicesQuery::default()).unwrap();
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
        .write(
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

#[test]
fn an_environments_namespace_is_the_one_its_deployments_use() {
    let (store, who) = shop();
    let query: Query = serde_json::from_value(json!({"query": "namespace"})).unwrap();
    let View::Namespace(before) = store.read(&who, &query).unwrap() else {
        panic!("a namespace query answers a namespace view");
    };
    assert_eq!(before.namespace.as_str(), "shop-production");
    admit(&store, &who, 1, &[], None).unwrap();
    let after = store.read(&who, &NamespaceQuery::default()).unwrap();
    assert_eq!(after, before);
    let stranger = Actor::system(OrganizationId::parse("other").unwrap());
    assert_eq!(
        code(store.read(&stranger, &NamespaceQuery::default())),
        RpcErrorCode::NotFound
    );
}

#[test]
fn an_upload_is_recorded_kept_for_later_deployments_and_its_receipts_come_back() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("app").unwrap(),
                image: None,
            },
        )
        .unwrap();
    let upload = UploadedSource {
        digest: "d".repeat(64),
        base: Some(UploadBase {
            commit: backend::sha(&"c".repeat(40)),
            changed: true,
        }),
        uploader: None,
    };
    let with = |n: u8, upload: Option<UploadedSource>| {
        store.write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: id(n),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &ployz_store::Trusted::default(),
        )
    };
    let mut bad = upload.clone();
    bad.digest = "D".repeat(64);
    assert_eq!(code(with(1, Some(bad))), RpcErrorCode::InvalidArgument);
    let first = with(1, Some(upload.clone())).unwrap();
    assert_eq!(first.upload.as_ref(), Some(&upload));
    let a = runner("cli-a");
    let claimed = store.claim(&id(1), &a).unwrap();
    // The runner prepares the Service without a source from the lowering input.
    assert!(
        claimed
            .intent
            .target
            .iter()
            .all(|spec| spec.name.as_str() != "app")
    );
    assert_eq!(
        claimed.input["snapshots"][2]["config"]["source"]["type"],
        json!("empty")
    );
    assert!(claimed.receipts.is_empty());
    let receipt = json!({"fingerprint": "f".repeat(64)});
    let built =
        |receipt: Value| RunEvidence::Built([(ServiceName::parse("app").unwrap(), receipt)].into());
    assert_eq!(
        code(store.record(&id(1), &runner("cli-b"), built(receipt.clone()))),
        RpcErrorCode::Conflict
    );
    assert_eq!(
        code(store.record(&id(1), &a, built(json!("image")))),
        RpcErrorCode::InvalidArgument
    );
    store.record(&id(1), &a, built(receipt.clone())).unwrap();
    // Built from an upload, it lists as uploaded, not empty.
    let listed = store
        .read(&who, &ployz_store::ServicesQuery::default())
        .unwrap();
    let app = listed
        .services
        .iter()
        .find(|listing| listing.service.name.as_str() == "app")
        .unwrap();
    assert_eq!(app.source, ployz_store::SourceKind::Uploaded);
    store
        .record(&id(1), &a, RunEvidence::NotExecuted("stopped".into()))
        .unwrap();
    // A later Deployment without a new upload keeps building from the latest one.
    let second = with(2, None).unwrap();
    assert_eq!(second.upload.as_ref(), Some(&upload));
    let claimed = store.claim(&id(2), &a).unwrap();
    assert_eq!(
        claimed.receipts.get(&ServiceName::parse("app").unwrap()),
        Some(&receipt)
    );
    assert_eq!(
        store
            .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
            .unwrap()
            .deployment
            .upload,
        Some(upload.clone())
    );
    // A retry ships the failed one's upload, not the Environment's latest.
    let mut newer = upload.clone();
    newer.digest = "e".repeat(64);
    with(3, Some(newer)).unwrap();
    assert_eq!(
        retry(&store, &who, 4, 1).unwrap().upload,
        Some(upload.clone())
    );
    // Another Environment of the Project without its own receipt borrows this one;
    // preparation reuses it only if its fingerprint matches.
    let staging = EnvironmentRef {
        project: None,
        environment: Some(ployz_store::EnvironmentName::parse("staging").unwrap()),
    };
    store
        .write(
            &who,
            &ployz_store::CreateEnvironment {
                id: EnvironmentId::parse("00000000-0000-4000-8000-000000000099").unwrap(),
                project: None,
                name: ployz_store::EnvironmentName::parse("staging").unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000098").unwrap(),
                environment: staging.clone(),
                name: ServiceName::parse("app").unwrap(),
                image: None,
            },
        )
        .unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: id(5),
                environment: staging,
                services: Vec::new(),
                version: None,
                upload: Some(upload),
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    assert_eq!(
        store.claim(&id(5), &a).unwrap().receipts[&ServiceName::parse("app").unwrap()],
        receipt
    );
}

#[test]
fn cloud_names_the_uploader_and_uploaded_builds_report_like_git_ones() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("app").unwrap(),
                image: None,
            },
        )
        .unwrap();
    let app = ServiceName::parse("app").unwrap();
    let upload = |uploader: Option<&str>| UploadedSource {
        digest: "d".repeat(64),
        base: None,
        uploader: uploader.map(|name| Principal::parse(name).unwrap()),
    };
    let admit = |n: u8, who: &Actor| {
        let command = Command::Admit(Admit::Deploy(Deploy {
            id: id(n),
            environment: EnvironmentRef::default(),
            services: Vec::new(),
            version: None,
            // A caller can't name the uploader: only Cloud's authentication does.
            upload: Some(upload(Some("mallory"))),
            accept_volume_loss: Vec::new(),
            message: None,
        }));
        let Written::Deployment(summary) = store.write(who, &command).unwrap() else {
            panic!("an admit writes a Deployment");
        };
        summary
    };
    // Cloud names who acts; the Deployment records them as its admitter and uploader.
    let nick = Actor {
        principal: Some(Principal::parse("nick").unwrap()),
        ..who.clone()
    };
    let admitted = admit(1, &nick);
    assert_eq!(admitted.upload, Some(upload(Some("nick"))));
    assert_eq!(admitted.admitted_by, nick.principal);
    assert!(admitted.admitted_at > 0);
    assert_eq!((admitted.started_at, admitted.ended_at), (None, None));
    assert_eq!(admit(2, &who).upload, Some(upload(None)));
    let a = runner("cloud-a");
    let claimed = store.claim(&id(2), &a).unwrap();
    assert_eq!(claimed.uploads, vec![app.clone()]);
    assert!(claimed.sources.is_empty());
    let report = |status| {
        RunEvidence::Build(ployz_store::BuildReport {
            service: app.clone(),
            status,
            message: None,
            log: "GitHub can't build uploaded source\n".into(),
        })
    };
    store
        .record(&id(2), &a, report(ployz_store::BuildStatus::Building))
        .unwrap();
    store
        .record(&id(2), &a, report(ployz_store::BuildStatus::Failed))
        .unwrap();
    store
        .record(&id(2), &a, RunEvidence::UploadNeeded(vec![app.clone()]))
        .unwrap();
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(2) })
        .unwrap();
    assert_eq!(view.deployment.status, DeploymentStatus::Failed);
    assert_eq!(view.builds.len(), 1);
    assert_eq!(view.builds[0].commit, None);
    assert_eq!(view.builds[0].status, ployz_store::BuildStatus::Failed);
    let Some(ployz_store::Outcome::NotExecuted { needs_upload, .. }) = view.outcome else {
        panic!("nothing executed");
    };
    assert_eq!(needs_upload, vec![app]);
    // A retry keeps the upload's provenance.
    assert_eq!(
        retry(&store, &who, 3, 2).unwrap().upload,
        Some(upload(None))
    );
}

#[test]
fn a_deploy_message_shows_on_the_deployment_and_its_retry() {
    let (store, who) = shop();
    let admit = |n: u8, message: String| {
        store.write(
            &who,
            &Admit::Deploy(Deploy {
                id: id(n),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: Some(message),
            }),
        )
    };
    let too_long = admit(1, "x".repeat(501)).unwrap_err();
    assert_eq!(too_long.code, RpcErrorCode::InvalidArgument);
    let admitted = admit(1, "  Ship the new header  ".into()).unwrap();
    assert_eq!(admitted.message.as_deref(), Some("Ship the new header"));
    store
        .write(&who, &ployz_store::Cancel { deployment: id(1) })
        .unwrap();
    let retried = store
        .write(
            &who,
            &Admit::Retry(ployz_store::Retry {
                id: id(2),
                deployment: id(1),
            }),
        )
        .unwrap();
    assert_eq!(retried.message.as_deref(), Some("Ship the new header"));
    let shown = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(2) })
        .unwrap();
    assert_eq!(
        shown.deployment.message.as_deref(),
        Some("Ship the new header")
    );
}

#[test]
fn deploying_a_service_with_nothing_to_run_is_refused_naming_it() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("blank").unwrap(),
                image: None,
            },
        )
        .unwrap();
    let refused = admit(&store, &who, 1, &[], None).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        refused.message,
        "blank has nothing to run yet: add an image or connect a repository"
    );
    assert_eq!(refused.details["service"], "blank");
    // Deploying other Services leaves it out.
    admit(&store, &who, 1, &["web"], None).unwrap();
}
