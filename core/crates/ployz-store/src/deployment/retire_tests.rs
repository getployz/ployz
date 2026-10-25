//! Deployments that tear down Services Saved State dropped, admitted through the
//! Store's own entry point: no caller-facing command names a teardown yet.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    DeployOutcome, DeployPreview, ExecutionError, PruneScope, QualifiedService, ServiceName,
};
use serde_json::{Value, json};

use super::{Retire, Scope, TargetNode};
use crate::{
    Actor, Admit, Change, Command, ConfigStore, CreateProject, CreateService, CreateVolume, Deploy,
    DeploymentId, DeploymentStatus, DeploymentSummary, Edit, EnvironmentId, EnvironmentRef, Mount,
    NodeStatus, OrganizationId, ProjectId, ProjectName, Publish, RemoveService, Retry, RunEvidence,
    RunnerId, ServiceLineageId, SettingPath, Trusted, VolumeId, VolumeName,
};

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn name(name: &str) -> ServiceName {
    ServiceName::parse(name).unwrap()
}

fn deployment(n: u8) -> DeploymentId {
    DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap()
}

fn runner() -> RunnerId {
    RunnerId::parse("runner-a").unwrap()
}

fn api() -> ServiceLineageId {
    ServiceLineageId::parse(uuid(4)).unwrap()
}

/// Production runs `web` and `api`, which mounts Volume `data`; then `api` leaves
/// Saved State.
fn retiring_api() -> (ConfigStore, Actor) {
    let store =
        ConfigStore::open("sqlite::memory:", crate::SealingKey::new(b"test").unwrap()).unwrap();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
            },
        )
        .unwrap();
    for (n, service) in [(3, "web"), (4, "api")] {
        add_service(&store, &who, n, service);
    }
    store
        .write(
            &who,
            &CreateVolume {
                id: VolumeId::parse(uuid(5)).unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("data").unwrap(),
                storage: ployz_core::config::VolumeKind::Docker {},
                mounts: vec![Mount {
                    service: name("api"),
                    path: "/data".into(),
                }],
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
            },
        )
        .unwrap();
    deploy(&store, &who, 1, &[]).unwrap();
    store.claim(&deployment(1), &runner()).unwrap();
    store
        .record(
            &deployment(1),
            &runner(),
            RunEvidence::Prepared(preview(&["web", "api"], true)),
        )
        .unwrap();
    store
        .record(&deployment(1), &runner(), succeeded(&["web", "api"]))
        .unwrap();
    remove_service(&store, &who, "api");
    publish(&store, &who);
    (store, who)
}

/// A new Service lineage `n` named `service`, staged.
fn add_service(store: &ConfigStore, who: &Actor, n: u8, service: &str) {
    store
        .write(
            who,
            &CreateService {
                id: ServiceLineageId::parse(uuid(n)).unwrap(),
                environment: EnvironmentRef::default(),
                name: name(service),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
}

fn remove_service(store: &ConfigStore, who: &Actor, service: &str) {
    store
        .write(
            who,
            &RemoveService {
                environment: EnvironmentRef::default(),
                service: name(service),
            },
        )
        .unwrap();
}

fn publish(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &Command::Publish(Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
}

fn deploy(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    services: &[&str],
) -> Result<DeploymentSummary, ployz_core::RpcError> {
    admit(store, who, n, services, false)
}

/// A Deploy of `services`, pruning the whole Namespace when `prune`.
fn admit(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    services: &[&str],
    prune: bool,
) -> Result<DeploymentSummary, ployz_core::RpcError> {
    store.write(
        who,
        &Admit::Deploy(Deploy {
            id: deployment(n),
            environment: EnvironmentRef::default(),
            services: services.iter().map(|service| name(service)).collect(),
            version: None,
            upload: None,
            accept_volume_loss: Vec::new(),
            message: None,
            prune,
        }),
    )
}

/// The Store's own Deployment of `services` that tears down `api`, resolved as
/// the merge will resolve it.
fn retire_api(store: &ConfigStore, who: &Actor, services: &[&str]) -> DeploymentSummary {
    let services: Vec<ServiceName> = services.iter().map(|service| name(service)).collect();
    store
        .storage
        .write(|tx| {
            let environment = crate::scope::lock(tx, who, &EnvironmentRef::default())?;
            let saved = crate::review::latest_saved(tx, &environment.summary.id)?.unwrap();
            let retire = super::retirements(
                tx,
                &environment.summary.id,
                &saved.intent,
                &BTreeSet::from([api()]),
            )?;
            crate::automation::auto_admit(
                tx,
                who,
                (&environment, &saved),
                (&services, &retire),
                (None, &BTreeMap::new()),
                super::Admission::New {
                    id: &crate::id::DeploymentId::parse(uuid::Uuid::new_v4().to_string())?,
                    push: None,
                },
                &Trusted::default(),
            )
        })
        .unwrap()
}

fn operation(service: &str) -> Value {
    let container = match service {
        "web" => "a",
        "api" => "b",
        _ => "c",
    };
    json!({"type": "remove_container", "machine_id": "a".repeat(32), "container_id": container.repeat(64)})
}

/// What the runner plans for `services`; `complete` when it saw every Machine.
fn preview(services: &[&str], complete: bool) -> DeployPreview {
    serde_json::from_value(json!({
        "namespace": "shop-production",
        "operations": services.iter().enumerate().map(|(index, service)| json!({
            "index": index, "machine_id": "a".repeat(32), "service_name": service,
            "operation": operation(service), "status": {"type": "pending"}
        })).collect::<Vec<_>>(),
        "warnings": [], "would_remove": [], "preserved_volumes": [],
        "prune_refusal": if complete { "selected_services" } else { "incomplete_snapshot" },
    }))
    .unwrap()
}

fn succeeded(services: &[&str]) -> RunEvidence {
    let outcome: DeployOutcome<ExecutionError> = serde_json::from_value(json!({
        "type": "success",
        "completed": services.iter().map(|service| operation(service)).collect::<Vec<_>>()
    }))
    .unwrap();
    RunEvidence::Executed {
        progress: Vec::new(),
        outcome: Box::new(outcome),
        removed: Vec::new(),
    }
}

/// The run executed `completed`, then failed at `failed`.
fn failed_at(completed: &[&str], failed: &str) -> RunEvidence {
    let outcome: DeployOutcome<ExecutionError> = serde_json::from_value(json!({
        "type": "failed",
        "completed": completed.iter().map(|service| operation(service)).collect::<Vec<_>>(),
        "failed": {"type": "operation", "operation": operation(failed), "error": {
            "type": "machine", "action": "RemoveContainer",
            "error": {"code": "internal", "message": "busy", "details": {}}
        }},
        "unexecuted": []
    }))
    .unwrap();
    RunEvidence::Executed {
        progress: Vec::new(),
        outcome: Box::new(outcome),
        removed: Vec::new(),
    }
}

fn redeploy(store: &ConfigStore, who: &Actor, n: u8, source: &DeploymentId) {
    store
        .write(
            who,
            &Admit::Retry(Retry {
                id: deployment(n),
                deployment: source.clone(),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
}

fn status(store: &ConfigStore, who: &Actor, id: &DeploymentId) -> DeploymentStatus {
    store
        .read(who, &crate::DeploymentQuery { id: id.clone() })
        .unwrap()
        .deployment
        .status
}

/// Run Deployment `id` to its end on `preview`, executing `ran`.
fn run(
    store: &ConfigStore,
    id: &DeploymentId,
    preview: DeployPreview,
    ran: &[&str],
) -> DeploymentStatus {
    store.claim(id, &runner()).unwrap();
    store
        .record(id, &runner(), RunEvidence::Prepared(preview))
        .unwrap();
    store.record(id, &runner(), succeeded(ran)).unwrap().status
}

fn nodes(store: &ConfigStore, id: &DeploymentId) -> Vec<TargetNode> {
    store
        .storage
        .read(|tx| super::locked(tx, id))
        .unwrap()
        .nodes
}

fn outcomes(store: &ConfigStore, who: &Actor, id: &DeploymentId) -> Vec<(String, NodeStatus)> {
    store
        .read(who, &crate::DeploymentQuery { id: id.clone() })
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| (node.node.name().to_owned(), node.outcome))
        .collect()
}

fn applied(store: &ConfigStore, who: &Actor) -> Vec<String> {
    store
        .storage
        .read(|tx| {
            let environment = crate::scope::lock(tx, who, &EnvironmentRef::default())?;
            super::applied_state(tx, &environment.summary.id, &environment.working)
        })
        .unwrap()
        .services
        .into_iter()
        .map(|service| service.slug)
        .collect()
}

#[test]
fn a_full_deploy_tears_down_what_saved_state_dropped_and_keeps_the_rest() {
    let (store, who) = retiring_api();
    let admitted = deploy(&store, &who, 3, &[]).unwrap();
    assert_eq!(admitted.services, []);
    assert!(!admitted.prune);
    let api_node = nodes(&store, &admitted.id)
        .into_iter()
        .find(|node| node.id() == api().as_str())
        .expect("the dropped Service is a node");
    assert_eq!(
        api_node,
        TargetNode::Service {
            id: api(),
            name: name("api"),
            runtime: name("api"),
            removals: ployz_core::ServiceRemovals::from_names([name("api")]),
        }
    );
    let claimed = store.claim(&admitted.id, &runner()).unwrap();
    assert!(claimed.intent.options.selected.is_empty());
    assert_eq!(
        claimed.intent.options.removals,
        ployz_core::ServiceRemovals::from_names([name("api")])
    );
    assert_eq!(claimed.intent.options.prune, PruneScope::Removals);
    assert!(claimed.input.to_string().contains("\"removals\":[\"api\"]"));
    assert!(claimed.input.to_string().contains("\"prune\":\"removals\""));
    assert_eq!(
        run(
            &store,
            &admitted.id,
            preview(&["web", "api"], true),
            &["web", "api"]
        ),
        DeploymentStatus::Applied
    );
    let outcomes = outcomes(&store, &who, &admitted.id);
    assert!(
        outcomes.contains(&("api".to_owned(), NodeStatus::Removed)),
        "{outcomes:?}"
    );
    assert!(
        outcomes.contains(&("web".to_owned(), NodeStatus::Deployed)),
        "{outcomes:?}"
    );
    assert_eq!(applied(&store, &who), ["web"]);
}

#[test]
fn a_full_deploy_with_prune_removes_the_whole_namespace() {
    let (store, who) = retiring_api();
    let admitted = admit(&store, &who, 3, &[], true).unwrap();
    assert_eq!(admitted.services, []);
    assert!(admitted.prune);
    let claimed = store.claim(&admitted.id, &runner()).unwrap();
    assert!(claimed.intent.options.selected.is_empty());
    assert_eq!(
        claimed.intent.options.removals,
        ployz_core::ServiceRemovals::from_names([name("api")])
    );
    assert_eq!(claimed.intent.options.prune, PruneScope::Namespace);
    assert!(!claimed.input.to_string().contains("\"prune\""));
    // The choice outlives a read of the row.
    assert!(
        store
            .read(
                &who,
                &crate::DeploymentQuery {
                    id: admitted.id.clone()
                }
            )
            .unwrap()
            .deployment
            .prune
    );
}

#[test]
fn a_full_deploy_replaces_a_dropped_service_whose_name_a_new_one_took() {
    let (store, who) = retiring_api();
    add_service(&store, &who, 6, "api");
    publish(&store, &who);
    let admitted = deploy(&store, &who, 3, &[]).unwrap();
    let claimed = store.claim(&admitted.id, &runner()).unwrap();
    assert!(
        claimed
            .intent
            .target
            .iter()
            .any(|spec| spec.name == name("api"))
    );
    assert!(
        claimed.intent.options.removals.is_empty(),
        "{:?}",
        claimed.intent.options.removals
    );
}

#[test]
fn a_full_deploy_tears_down_the_runtime_a_renamed_service_ran_as() {
    let (store, who) = retiring_api();
    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("web.privateDns").unwrap(),
                    value: json!("front"),
                }],
            },
        )
        .unwrap();
    publish(&store, &who);
    let admitted = deploy(&store, &who, 3, &[]).unwrap();
    let web = ServiceLineageId::parse(uuid(3)).unwrap();
    let web_node = nodes(&store, &admitted.id)
        .into_iter()
        .find(|node| node.id() == web.as_str())
        .expect("the renamed Service is a node");
    assert_eq!(
        web_node,
        TargetNode::Service {
            id: web,
            name: name("web"),
            runtime: name("front"),
            removals: ployz_core::ServiceRemovals::from_names([name("web")]),
        }
    );
    let claimed = store.claim(&admitted.id, &runner()).unwrap();
    assert!(
        claimed
            .intent
            .target
            .iter()
            .any(|spec| spec.name == name("front"))
    );
    assert_eq!(
        claimed.intent.options.removals,
        ployz_core::ServiceRemovals::from_names([name("api"), name("web")])
    );
    assert_eq!(
        run(
            &store,
            &admitted.id,
            preview(&["front", "api", "web"], true),
            &["front", "api", "web"]
        ),
        DeploymentStatus::Applied
    );
    let outcomes = outcomes(&store, &who, &admitted.id);
    assert!(
        outcomes.contains(&("web".to_owned(), NodeStatus::Deployed)),
        "{outcomes:?}"
    );
    assert!(
        outcomes.contains(&("api".to_owned(), NodeStatus::Removed)),
        "{outcomes:?}"
    );
    assert_eq!(applied(&store, &who), ["web"]);
    // Torn down once, the old runtime is no later Deployment's to remove.
    let again = deploy(&store, &who, 4, &[]).unwrap();
    let claimed = store.claim(&again.id, &runner()).unwrap();
    assert!(
        claimed.intent.options.removals.is_empty(),
        "{:?}",
        claimed.intent.options.removals
    );
}

#[test]
fn a_full_deploy_keeps_a_refused_runtime_removal_for_the_next_complete_one() {
    let (store, who) = retiring_api();
    let gone = deploy(&store, &who, 3, &[]).unwrap();
    assert_eq!(
        run(&store, &gone.id, preview(&["api"], true), &["api"]),
        DeploymentStatus::Applied
    );
    assert_eq!(applied(&store, &who), ["web"]);
    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("web.privateDns").unwrap(),
                    value: json!("front"),
                }],
            },
        )
        .unwrap();
    publish(&store, &who);
    // A snapshot missing a Machine refuses the removal of `web`, and the Deployment
    // still lands Applied on `front` alone.
    let refused = deploy(&store, &who, 4, &[]).unwrap();
    let claimed = store.claim(&refused.id, &runner()).unwrap();
    assert_eq!(
        claimed.intent.options.removals,
        ployz_core::ServiceRemovals::from_names([name("web")])
    );
    store
        .record(
            &refused.id,
            &runner(),
            RunEvidence::Prepared(preview(&["front"], false)),
        )
        .unwrap();
    assert_eq!(
        store
            .record(&refused.id, &runner(), succeeded(&["front"]))
            .unwrap()
            .status,
        DeploymentStatus::Applied
    );
    let outcomes = outcomes(&store, &who, &refused.id);
    assert!(
        outcomes.contains(&("web".to_owned(), NodeStatus::Deployed)),
        "{outcomes:?}"
    );
    // Nothing removed `web`, so the next full Deployment carries it again.
    let again = deploy(&store, &who, 5, &[]).unwrap();
    let claimed = store.claim(&again.id, &runner()).unwrap();
    assert_eq!(
        claimed.intent.options.removals,
        ployz_core::ServiceRemovals::from_names([name("web")])
    );
    assert_eq!(
        run(&store, &again.id, preview(&["web"], true), &["web"]),
        DeploymentStatus::Applied
    );
    let done = deploy(&store, &who, 6, &[]).unwrap();
    let claimed = store.claim(&done.id, &runner()).unwrap();
    assert!(
        claimed.intent.options.removals.is_empty(),
        "{:?}",
        claimed.intent.options.removals
    );
}

#[test]
fn redeploying_a_full_deployment_runs_the_service_a_queued_full_deployment_removes() {
    let (store, who) = retiring_api();
    let queued = deploy(&store, &who, 3, &[]).unwrap();
    assert!(nodes(&store, &queued.id).iter().any(TargetNode::retires));
    redeploy(&store, &who, 4, &deployment(1));
    assert_eq!(
        status(&store, &who, &queued.id),
        DeploymentStatus::Superseded
    );
    let claimed = store.claim(&deployment(4), &runner()).unwrap();
    assert!(
        claimed
            .intent
            .target
            .iter()
            .any(|spec| spec.name == name("api"))
    );
    assert!(
        claimed.intent.options.removals.is_empty(),
        "{:?}",
        claimed.intent.options.removals
    );
}

#[test]
fn a_full_deploy_tears_down_a_service_that_failed_to_start_once_saved_state_drops_it() {
    let (store, who) = retiring_api();
    add_service(&store, &who, 6, "jobs");
    publish(&store, &who);
    let attempt = deploy(&store, &who, 3, &[]).unwrap();
    store.claim(&attempt.id, &runner()).unwrap();
    store
        .record(
            &attempt.id,
            &runner(),
            RunEvidence::Prepared(preview(&["web", "jobs", "api"], true)),
        )
        .unwrap();
    assert_eq!(
        store
            .record(&attempt.id, &runner(), failed_at(&["web", "api"], "jobs"))
            .unwrap()
            .status,
        DeploymentStatus::Failed
    );
    assert_eq!(applied(&store, &who), ["web"]);
    remove_service(&store, &who, "jobs");
    publish(&store, &who);
    let admitted = deploy(&store, &who, 4, &[]).unwrap();
    let claimed = store.claim(&admitted.id, &runner()).unwrap();
    assert_eq!(
        claimed.intent.options.removals,
        ployz_core::ServiceRemovals::from_names([name("jobs")])
    );
}

#[test]
fn a_redeploy_never_prunes() {
    let (store, who) = retiring_api();
    let pruned = admit(&store, &who, 3, &[], true).unwrap();
    assert_eq!(
        run(
            &store,
            &pruned.id,
            preview(&["web", "api"], true),
            &["web", "api"]
        ),
        DeploymentStatus::Applied
    );
    redeploy(&store, &who, 4, &pruned.id);
    assert!(
        !store
            .read(&who, &crate::DeploymentQuery { id: deployment(4) })
            .unwrap()
            .deployment
            .prune
    );
    let claimed = store.claim(&deployment(4), &runner()).unwrap();
    assert_eq!(claimed.intent.options.prune, PruneScope::Removals);
}

#[test]
fn a_teardown_alone_stays_targeted_and_removes_only_the_service() {
    let (store, who) = retiring_api();
    let admitted = retire_api(&store, &who, &[]);
    assert_eq!(admitted.services, [name("api")]);
    // Only the Service: never Volume `data` it mounted, so no Docker Volume is deleted.
    assert_eq!(
        nodes(&store, &admitted.id),
        [TargetNode::Service {
            id: api(),
            name: name("api"),
            runtime: name("api"),
            removals: ployz_core::ServiceRemovals::from_names([name("api")]),
        }]
    );
    let claimed = store.claim(&admitted.id, &runner()).unwrap();
    assert_eq!(
        claimed
            .intent
            .options
            .selected
            .iter()
            .map(|attempt| attempt.name.clone())
            .collect::<Vec<_>>(),
        [name("api")]
    );
    assert_eq!(
        claimed.intent.options.removals,
        ployz_core::ServiceRemovals::from_names([name("api")])
    );
    assert!(claimed.input.to_string().contains("\"removals\":[\"api\"]"));
    assert_eq!(
        run(&store, &admitted.id, preview(&["api"], true), &["api"]),
        DeploymentStatus::Applied
    );
    assert_eq!(
        outcomes(&store, &who, &admitted.id),
        [("api".to_owned(), NodeStatus::Removed)]
    );
    assert_eq!(applied(&store, &who), ["web"]);
}

#[test]
fn a_teardown_rides_with_the_services_a_deploy_ships() {
    let (store, who) = retiring_api();
    let admitted = retire_api(&store, &who, &["web"]);
    assert_eq!(admitted.services, [name("web"), name("api")]);
    let claimed = store.claim(&admitted.id, &runner()).unwrap();
    assert_eq!(
        claimed
            .intent
            .options
            .selected
            .iter()
            .map(|attempt| attempt.name.clone())
            .collect::<Vec<_>>(),
        [name("web"), name("api")]
    );
    assert_eq!(
        claimed.intent.options.removals,
        ployz_core::ServiceRemovals::from_names([name("api")])
    );
}

#[test]
fn a_service_already_gone_is_removed_with_nothing_to_run() {
    let (store, who) = retiring_api();
    let admitted = retire_api(&store, &who, &[]);
    assert_eq!(
        run(&store, &admitted.id, preview(&[], true), &[]),
        DeploymentStatus::Applied
    );
    assert_eq!(
        outcomes(&store, &who, &admitted.id),
        [("api".to_owned(), NodeStatus::Removed)]
    );
    assert_eq!(applied(&store, &who), ["web"]);
}

#[test]
fn a_snapshot_missing_a_machine_keeps_the_service_in_applied_state() {
    let (store, who) = retiring_api();
    let admitted = retire_api(&store, &who, &[]);
    assert_eq!(
        run(&store, &admitted.id, preview(&[], false), &[]),
        DeploymentStatus::Failed
    );
    assert_eq!(
        outcomes(&store, &who, &admitted.id),
        [("api".to_owned(), NodeStatus::NotAttempted)]
    );
    assert_eq!(applied(&store, &who), ["web", "api"]);

    // A retry tears down exactly what the failed attempt froze.
    store
        .write(
            &who,
            &Admit::Retry(Retry {
                id: deployment(3),
                deployment: admitted.id.clone(),
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
    assert_eq!(nodes(&store, &deployment(3)), nodes(&store, &admitted.id));
    assert_eq!(
        run(&store, &deployment(3), preview(&["api"], true), &["api"]),
        DeploymentStatus::Applied
    );
    assert_eq!(applied(&store, &who), ["web"]);
}

#[test]
fn a_targeted_deploy_superseding_a_teardown_keeps_it() {
    let (store, who) = retiring_api();
    let admitted = retire_api(&store, &who, &[]);
    let replacement = deploy(&store, &who, 3, &["web"]).unwrap();
    assert_eq!(replacement.services, [name("web"), name("api")]);
    assert_eq!(
        store
            .read(
                &who,
                &crate::DeploymentQuery {
                    id: admitted.id.clone()
                }
            )
            .unwrap()
            .deployment
            .status,
        DeploymentStatus::Superseded
    );
    let claimed = store.claim(&deployment(3), &runner()).unwrap();
    assert_eq!(
        claimed.intent.options.removals,
        ployz_core::ServiceRemovals::from_names([name("api")])
    );
    assert_eq!(
        run(
            &store,
            &deployment(3),
            preview(&["web", "api"], true),
            &["web", "api"]
        ),
        DeploymentStatus::Applied
    );
    assert_eq!(applied(&store, &who), ["web"]);
}

#[test]
fn a_narrowed_deployment_never_tears_down_a_service_saved_state_runs() {
    let (store, who) = retiring_api();
    let refused = |retire: Retire| {
        store
            .storage
            .read(|tx| {
                let environment = crate::scope::lock(tx, &who, &EnvironmentRef::default())?;
                let saved = crate::review::latest_saved(tx, &environment.summary.id)?.unwrap();
                super::freeze(
                    &environment.summary.id,
                    &who.organization,
                    &saved.intent,
                    &super::Landing::applied(&saved.intent),
                    Scope::Narrow {
                        services: &[],
                        retire: &[retire],
                        losses: &[],
                        every_saved: false,
                    },
                    ployz_core::Namespace::parse("shop-production").unwrap(),
                    None,
                )
                .map(|_| ())
            })
            .unwrap_err()
            .code
    };
    let web = ServiceLineageId::parse(uuid(3)).unwrap();
    for retire in [
        // Still saved.
        Retire {
            id: web,
            name: name("web"),
            runtimes: ployz_core::ServiceRemovals::from_names([name("web")]),
        },
        // Gone, but its runtime Service is one `web` runs.
        Retire {
            id: api(),
            name: name("api"),
            runtimes: ployz_core::ServiceRemovals::from_names([name("web")]),
        },
        // Nothing to tear down.
        Retire {
            id: api(),
            name: name("api"),
            runtimes: ployz_core::ServiceRemovals::default(),
        },
    ] {
        assert_eq!(refused(retire), ployz_core::RpcErrorCode::InvalidArgument);
    }
    // Resolving never offers a Service Saved State still holds.
    let resolved = store
        .storage
        .read(|tx| {
            let environment = crate::scope::lock(tx, &who, &EnvironmentRef::default())?;
            let saved = crate::review::latest_saved(tx, &environment.summary.id)?.unwrap();
            super::retirements(
                tx,
                &environment.summary.id,
                &saved.intent,
                &BTreeSet::from([ServiceLineageId::parse(uuid(3)).unwrap()]),
            )
        })
        .unwrap();
    assert!(resolved.is_empty());
}

/// How many Saved revisions reading Deployment `id` loads.
fn saved_reads(store: &ConfigStore, who: &Actor, id: &DeploymentId) -> usize {
    let before = super::SAVED_READS.get();
    store
        .read(who, &crate::DeploymentQuery { id: id.clone() })
        .unwrap();
    super::SAVED_READS.get() - before
}

#[test]
fn reading_a_deployment_loads_saved_state_only_for_what_its_rows_mount() {
    let (store, who) = retiring_api();
    // A queued teardown of one Service: no rows, so no Saved read.
    let teardown = retire_api(&store, &who, &[]);
    assert_eq!(saved_reads(&store, &who, &teardown.id), 0);

    // A running Deployment with rows and a Volume: the mounted rows read Saved State once.
    let (store, who) = retiring_api();
    deploy(&store, &who, 2, &[]).unwrap();
    store.claim(&deployment(2), &runner()).unwrap();
    store
        .record(
            &deployment(2),
            &runner(),
            RunEvidence::Prepared(preview(&["web"], true)),
        )
        .unwrap();
    let snapshot: Vec<ployz_core::OperationRow> = serde_json::from_value(json!([{
        "index": 0, "machine_id": "a".repeat(32), "machine_name": "alpha", "service_name": "web",
        "operation": operation("web"), "status": {"type": "pending"}
    }]))
    .unwrap();
    store
        .record(
            &deployment(2),
            &runner(),
            RunEvidence::Progress(crate::RowTracker::default().changes(&snapshot)),
        )
        .unwrap();
    let read = store
        .read(&who, &crate::DeploymentQuery { id: deployment(2) })
        .unwrap();
    assert!(
        read.nodes.iter().any(|node| node.node.name() == "data"),
        "the Deployment ships the mounted Volume"
    );
    assert!(read.nodes.iter().any(|node| !node.rows.is_empty()));
    assert_eq!(saved_reads(&store, &who, &deployment(2)), 1);
}

/// Deployment 2 records a completed row for runtime `cache`, which no node targets,
/// under a preview that does or doesn't list `cache` as obsolete.
fn read_with_a_cache_row(obsolete: bool) -> super::DeploymentView {
    let (store, who) = retiring_api();
    deploy(&store, &who, 2, &[]).unwrap();
    store.claim(&deployment(2), &runner()).unwrap();
    let mut plan = preview(&["web", "cache"], true);
    if obsolete {
        plan.would_remove
            .push(QualifiedService::new(plan.namespace.clone(), name("cache")));
    }
    store
        .record(&deployment(2), &runner(), RunEvidence::Prepared(plan))
        .unwrap();
    let snapshot: Vec<ployz_core::OperationRow> = serde_json::from_value(json!([{
        "index": 1, "machine_id": "a".repeat(32), "machine_name": "alpha", "service_name": "cache",
        "operation": operation("cache"), "status": {"type": "completed"}
    }]))
    .unwrap();
    store
        .record(
            &deployment(2),
            &runner(),
            RunEvidence::Progress(crate::RowTracker::default().changes(&snapshot)),
        )
        .unwrap();
    store
        .read(&who, &crate::DeploymentQuery { id: deployment(2) })
        .unwrap()
}

#[test]
fn a_service_the_store_never_held_keeps_its_rows_outside_every_node() {
    let read = read_with_a_cache_row(true);
    assert!(!read.nodes.iter().any(|node| node.node.name() == "cache"));
    let rows: Vec<_> = read
        .untargeted
        .get(&name("cache"))
        .unwrap()
        .iter()
        .map(|row| (row.server.as_str(), &row.state))
        .collect();
    assert_eq!(rows, [("alpha", &crate::RowState::Completed)]);
}

#[test]
fn an_untargeted_row_the_plan_does_not_remove_is_not_shown_as_removed() {
    let read = read_with_a_cache_row(false);
    assert!(read.untargeted.is_empty());
}
