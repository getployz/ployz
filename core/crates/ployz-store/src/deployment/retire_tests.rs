//! Deployments that tear down Services Saved State dropped, admitted through the
//! Store's own entry point: no caller-facing command names a teardown yet.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{DeployOutcome, DeployPreview, ExecutionError, ServiceName};
use serde_json::{Value, json};

use super::{Retire, Scope, TargetNode};
use crate::{
    Actor, Admit, Command, ConfigStore, CreateProject, CreateService, CreateVolume, Deploy,
    DeploymentId, DeploymentStatus, DeploymentSummary, EnvironmentId, EnvironmentRef, Mount,
    NodeStatus, OrganizationId, ProjectId, ProjectName, Publish, RemoveService, Retry, RunEvidence,
    RunnerId, ServiceLineageId, Trusted, VolumeId, VolumeName,
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
        store
            .write(
                &who,
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
    store
        .write(
            &who,
            &RemoveService {
                environment: EnvironmentRef::default(),
                service: name("api"),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &Command::Publish(Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            }),
        )
        .unwrap();
    (store, who)
}

fn deploy(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    services: &[&str],
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
                &Trusted::default(),
            )
        })
        .unwrap()
}

fn operation(service: &str) -> Value {
    let container = if service == "web" { "a" } else { "b" };
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
                    &saved.intent,
                    &saved.intent,
                    Scope::Narrow {
                        services: &[],
                        retire: &[retire],
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
