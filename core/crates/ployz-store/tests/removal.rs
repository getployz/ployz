#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Environment lifecycle through the Store's interface only, on SQLite and on
//! Postgres (see `backend`): listing, the Default Environment, and `remove`, which
//! marks an Environment or a Project, takes what still runs off the Servers and
//! deletes it from the Store once nothing of it runs.

use std::collections::BTreeMap;

use ployz_core::{
    DeployOutcome, DeployPreview, DockerVolumeId, DockerVolumeName, MachineId, RpcError,
    RpcErrorCode, ServiceName, VolumeRemoval, VolumeRemovalOutcome,
};
use ployz_store::{
    Actor, Admit, AdmitRemoval, Approval, Automated, Cancel, CancelRemoval, ConfigStore,
    CreateBranch, CreateEnvironment, CreateProject, CreateService, CreateVolume, Deploy,
    DeploymentId, DeploymentStatus, DeploymentSummary, DueQuery, EnvironmentId, EnvironmentName,
    EnvironmentRef, EnvironmentsQuery, KeepBranch, LeftStatus, Mount, OrganizationId, ProjectId,
    ProjectName, Publish, Removal, RemovalsQuery, Remove, RunEvidence, RunnerId, ServiceLineageId,
    SetDefaultEnvironment, ShutDown, Sweep, SyncChanges, SystemEvent, Trusted, VolumeId,
    VolumeName, VolumeObservation, Written,
};
use serde_json::json;

mod backend;

/// A node by its name: `SERVICE`, or `volumes.VOLUME`.
fn node(name: &str) -> ployz_store::NodeName {
    ployz_store::NodeName::parse(name).unwrap()
}

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

/// Project `shop`: `production` runs `web` and `db`, which mounts Volume `data`.
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
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
    for (n, name) in [(3, "web"), (4, "db")] {
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(uuid(n)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some("postgres:17".into()),
                    template: None,
                },
            )
            .unwrap();
    }
    volume(&store, &who, 5, "data");
    store
        .write(
            &who,
            &CreateEnvironment {
                id: EnvironmentId::parse(uuid(6)).unwrap(),
                project: None,
                name: EnvironmentName::parse("staging").unwrap(),
            },
        )
        .unwrap();
    (store, who)
}

/// Give `production`'s `db` Volume `name`, with ID `uuid(n)`.
fn volume(store: &ConfigStore, who: &Actor, n: u8, name: &str) {
    store
        .write(
            who,
            &CreateVolume {
                storage_locked: false,
                shared_writes: false,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse(uuid(n)).unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse(name).unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("db").unwrap(),
                    path: format!("/{name}"),
                }],
            },
        )
        .unwrap();
}

/// [`shop`] with `production` deployed and its Branch `fix`, running `web`, deployed.
fn branched() -> (ConfigStore, Actor) {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, &["web", "db"]);
    branch(&store, &who, "fix");
    deploy(&store, &who, "fix", 2, &["web"]);
    (store, who)
}

fn branch(store: &ConfigStore, who: &Actor, name: &str) {
    store
        .write(
            who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(7)).unwrap(),
                from: at("production"),
                name: EnvironmentName::parse(name).unwrap(),
                copy: vec![node("web")],
                live: Vec::new(),
                setup: Vec::new(),
                keep: false,
                fix: None,
            },
        )
        .unwrap();
}

fn id(n: u8) -> DeploymentId {
    DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap()
}

fn runner() -> RunnerId {
    RunnerId::parse("runner").unwrap()
}

/// Claim Deployment `id` and record a Deploy Preview touching `services`.
fn prepare(store: &ConfigStore, id: &DeploymentId, services: &[&str]) -> ployz_store::Claimed {
    let claimed = store.claim(id, &runner()).unwrap();
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace,
        "operations": services.iter().enumerate().map(|(index, name)| json!({
            "index": index, "machine_id": "a".repeat(32), "service_name": name,
            "operation": operation(index), "status": {"type": "pending"}
        })).collect::<Vec<_>>(),
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(id, &runner(), RunEvidence::Prepared(preview))
        .unwrap();
    claimed
}

fn operation(index: usize) -> serde_json::Value {
    json!({"type": "remove_container", "machine_id": "a".repeat(32),
           "container_id": format!("{index:x}").repeat(64)})
}

fn executed(
    store: &ConfigStore,
    id: &DeploymentId,
    (outcome, progress): (serde_json::Value, Vec<ployz_store::ServerProgress>),
    removed: Vec<VolumeRemoval>,
) {
    let outcome: DeployOutcome<ployz_core::ExecutionError> =
        serde_json::from_value(outcome).unwrap();
    store
        .record(
            id,
            &runner(),
            RunEvidence::Executed {
                progress,
                outcome: Box::new(outcome),
                removed,
            },
        )
        .unwrap();
}

/// Record that Deployment `id` succeeded for `services` and deleted `removed`.
fn succeed(store: &ConfigStore, id: &DeploymentId, services: &[&str], removed: Vec<VolumeRemoval>) {
    executed(
        store,
        id,
        (
            json!({
                "type": "success",
                "completed": (0..services.len()).map(operation).collect::<Vec<_>>()
            }),
            Vec::new(),
        ),
        removed,
    );
}

/// Run Deployment `id` for `services` to a failure on Server `a…`.
fn fail(store: &ConfigStore, id: &DeploymentId, services: &[&str]) {
    prepare(store, id, services);
    let error = json!({
        "type": "machine", "action": "RemoveContainer",
        "error": {"code": "internal", "message": "busy", "details": {}}
    });
    let rows: Vec<ployz_core::OperationRow> = serde_json::from_value(json!([{
        "index": 0, "machine_id": "a".repeat(32), "service_name": services[0],
        "operation": operation(0), "status": {"type": "failed", "error": error}
    }]))
    .unwrap();
    executed(
        store,
        id,
        (
            json!({
                "type": "failed", "completed": [],
                "failed": {"type": "operation", "operation": operation(0), "error": error},
                "unexecuted": []
            }),
            ployz_store::RowTracker::default().changes(&rows),
        ),
        Vec::new(),
    );
}

/// Run Deployment `id` for `services` to `applied`, deleting `removed`.
fn run(store: &ConfigStore, id: &DeploymentId, services: &[&str], removed: Vec<VolumeRemoval>) {
    prepare(store, id, services);
    succeed(store, id, services, removed);
}

fn deploy(store: &ConfigStore, who: &Actor, environment: &str, n: u8, services: &[&str]) {
    store
        .write(
            who,
            &Admit::Deploy(Deploy {
                id: id(n),
                environment: at(environment),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
        )
        .unwrap();
    run(store, &id(n), services, Vec::new());
}

fn volumes(given: &[(&str, &[&str])]) -> BTreeMap<EnvironmentName, Vec<VolumeName>> {
    given
        .iter()
        .map(|(environment, volumes)| {
            (
                EnvironmentName::parse(*environment).unwrap(),
                volumes
                    .iter()
                    .map(|volume| VolumeName::parse(*volume).unwrap())
                    .collect(),
            )
        })
        .collect()
}

/// Remove `environment` of `shop`, or all of it.
fn request(environment: Option<&str>) -> Remove {
    Remove {
        project: ProjectName::parse("shop").unwrap(),
        environment: environment.map(|name| EnvironmentName::parse(name).unwrap()),
        version: None,
        accept_volume_loss: BTreeMap::new(),
        every_copy: BTreeMap::new(),
    }
}

/// Send `remove` as a person does: asked to confirm, again with the version the
/// refusal handed back.
fn confirmed(
    store: &ConfigStore,
    who: &Actor,
    remove: Remove,
    trusted: &Trusted,
) -> Result<Removal, RpcError> {
    match store.write_trusted(who, &remove, trusted) {
        Err(refused) if refused.code == RpcErrorCode::ConfirmationRequired => {
            let version = refused.details["version"].as_str().unwrap().to_owned();
            store.write_trusted(
                who,
                &Remove {
                    version: Some(version),
                    ..remove
                },
                trusted,
            )
        }
        sent => sent,
    }
}

fn observing(observation: VolumeObservation) -> Trusted {
    Trusted {
        volumes: Some(observation),
        ..Trusted::default()
    }
}

/// Each Environment the removal hasn't taken off yet, and where it stands.
fn left(removal: Removal) -> Vec<(String, LeftStatus)> {
    match removal {
        Removal::Removing { left, .. } => left
            .into_iter()
            .map(|left| (left.environment.name.to_string(), left.status))
            .collect(),
        Removal::Removed { .. } => panic!("removed"),
    }
}

/// The Deployment taking `environment` off, which `removal` admitted.
fn taking_off(removal: &Removal, environment: &str) -> DeploymentId {
    let Removal::Removing { left, .. } = removal else {
        panic!("removed");
    };
    left.iter()
        .find(|left| left.environment.name.as_str() == environment)
        .and_then(|left| left.deployment.as_ref())
        .map(|deployment| deployment.id.clone())
        .unwrap()
}

/// The Environments that came off only in configuration, and their Services.
fn residuals(removal: Removal) -> Vec<(String, Vec<String>)> {
    match removal {
        Removal::Removed { residuals } => residuals
            .into_iter()
            .map(|residual| {
                let mut services: Vec<String> =
                    residual.services.iter().map(ToString::to_string).collect();
                services.sort();
                (residual.environment.to_string(), services)
            })
            .collect(),
        Removal::Removing { left, .. } => panic!("still removing: {left:?}"),
    }
}

/// Cloud's admission of the removal Deployment `n` of `environment`.
fn admit_removal(
    store: &ConfigStore,
    who: &Actor,
    environment: &str,
    n: u8,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    store.write_trusted(
        who,
        &Admit::Remove(AdmitRemoval {
            id: id(n),
            environment: at(environment),
        }),
        trusted,
    )
}

/// Shut `environment` down with Deployment `n`, accepting `accept` as a person
/// would: with the version the refusal that listed them handed back.
fn shut_down(
    store: &ConfigStore,
    who: &Actor,
    (environment, n): (&str, u8),
    accept: &[&str],
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    let request = |version: Option<String>| {
        Admit::ShutDown(ShutDown {
            id: id(n),
            environment: at(environment),
            version,
            accept_volume_loss: accept
                .iter()
                .map(|name| VolumeName::parse(*name).unwrap())
                .collect(),
        })
    };
    match store.write_trusted(who, &request(None), trusted) {
        Err(refused)
            if !accept.is_empty() && refused.code == RpcErrorCode::ConfirmationRequired =>
        {
            let version = refused.details["version"].as_str().unwrap().to_owned();
            store.write_trusted(who, &request(Some(version)), trusted)
        }
        admitted => admitted,
    }
}

fn sweep(store: &ConfigStore, who: &Actor) -> Automated {
    let Written::Automated(automated) = store
        .system(
            &who.organization,
            &SystemEvent::Sweep(Sweep { now: 0 }),
            &Trusted::default(),
        )
        .unwrap()
    else {
        panic!("a sweep writes Automated");
    };
    automated
}

fn names<'a>(
    environments: impl IntoIterator<Item = &'a ployz_store::EnvironmentSummary>,
) -> Vec<String> {
    let mut names: Vec<String> = environments
        .into_iter()
        .map(|environment| environment.name.to_string())
        .collect();
    names.sort();
    names
}

/// What comes off next, as Cloud reads it: each Environment and where it failed.
fn due(store: &ConfigStore, who: &Actor) -> Vec<(String, Vec<String>)> {
    store
        .read(who, &DueQuery {})
        .unwrap()
        .into_iter()
        .map(|due| (due.environment.name.to_string(), due.failed_on))
        .collect()
}

fn set_default(store: &ConfigStore, who: &Actor, environment: &str) {
    store
        .write(
            who,
            &SetDefaultEnvironment {
                environment: at(environment),
            },
        )
        .unwrap();
}

/// Each Environment's name, `*` marking the default and `<` naming its Parent.
fn listed(store: &ConfigStore, who: &Actor) -> Vec<String> {
    store
        .read(who, &EnvironmentsQuery::default())
        .unwrap()
        .environments
        .into_iter()
        .map(|listing| {
            let mut name = listing.name.to_string();
            if listing.default {
                name.push('*');
            }
            if let Some(parent) = listing.parent {
                name.push_str(&format!("<{parent}"));
            }
            name
        })
        .collect()
}

fn server(letter: char) -> MachineId {
    MachineId::parse(letter.to_string().repeat(32)).unwrap()
}

/// `production`'s Docker Volume `uuid(n)` on Server `letter…`.
fn docker(letter: char, n: u8) -> DockerVolumeId {
    DockerVolumeId {
        machine_id: server(letter),
        name: DockerVolumeName::parse(format!("shop-production_vol-{}", uuid(n))).unwrap(),
    }
}

fn held() -> DockerVolumeId {
    docker('a', 5)
}

fn observed() -> VolumeObservation {
    VolumeObservation {
        sought: vec![held().name],
        held: vec![held()],
        unanswered: Vec::new(),
    }
}

fn deleted(volumes: &[DockerVolumeId]) -> Vec<VolumeRemoval> {
    volumes
        .iter()
        .map(|volume| VolumeRemoval {
            id: volume.clone(),
            outcome: VolumeRemovalOutcome::Removed,
        })
        .collect()
}

/// `production`'s removal, accepting its Volume `data` as held on Server `a…`.
fn accepting_data(environment: Option<&str>) -> Remove {
    Remove {
        accept_volume_loss: volumes(&[("production", &["data"])]),
        ..request(environment)
    }
}

fn refusal(result: Result<impl std::fmt::Debug, RpcError>) -> RpcError {
    result.unwrap_err()
}

#[test]
fn environments_list_and_an_undeployed_one_is_removed_without_servers() {
    let (store, who) = shop();
    assert_eq!(listed(&store, &who), ["production*", "staging"]);

    let default = refusal(store.write(&who, &request(Some("production"))));
    assert_eq!(default.code, RpcErrorCode::Unsupported);
    assert!(default.message.contains("Default Environment"));
    assert_eq!(default.details["next"]["tool"], "environment_show");

    set_default(&store, &who, "staging");
    assert_eq!(listed(&store, &who), ["production", "staging*"]);
    // Nothing of production ever ran, so it goes at once, with no Servers.
    let removal = store.write(&who, &request(Some("production"))).unwrap();
    assert!(residuals(removal).is_empty());
    assert_eq!(listed(&store, &who), ["staging*"]);
    assert_eq!(
        refusal(store.write(&who, &request(Some("production")))).code,
        RpcErrorCode::NotFound
    );
}

#[test]
fn a_deployed_root_leaves_the_servers_before_the_store() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, &["web", "db"]);
    set_default(&store, &who, "staging");

    // The removal deletes the deployed Volume, under the same review as any Deploy.
    let removals = store
        .read(
            &who,
            &RemovalsQuery {
                environment: at("production"),
                remove: true,
            },
        )
        .unwrap();
    assert_eq!(removals.volumes[0].docker_volume, held().name);
    let unobserved = refusal(store.write(&who, &accepting_data(Some("production"))));
    assert_eq!(unobserved.code, RpcErrorCode::Unavailable);
    let unaccepted =
        refusal(store.write_trusted(&who, &request(Some("production")), &observing(observed())));
    assert_eq!(unaccepted.code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(
        unaccepted.details["accept"],
        json!({"production": ["data"]})
    );
    assert!(due(&store, &who).is_empty(), "a refusal marks nothing");

    let removing = confirmed(
        &store,
        &who,
        accepting_data(Some("production")),
        &observing(observed()),
    )
    .unwrap();
    let first = taking_off(&removing, "production");
    assert_eq!(
        left(removing),
        [("production".into(), LeftStatus::Removing)]
    );

    // Nothing builds on it, and it isn't gone until the removal applies.
    let branching = refusal(store.write(
        &who,
        &CreateBranch {
            id: EnvironmentId::parse(uuid(7)).unwrap(),
            from: at("production"),
            name: EnvironmentName::parse("fix").unwrap(),
            copy: vec![node("web")],
            live: Vec::new(),
            setup: Vec::new(),
            keep: false,
            fix: None,
        },
    ));
    assert_eq!(branching.code, RpcErrorCode::Conflict);
    assert!(branching.message.contains("being removed"));
    assert!(sweep(&store, &who).removed.is_empty());

    // Cancelled, the removal stops and it stays deployed.
    store.write(&who, &Cancel { deployment: first }).unwrap();
    let stopped = refusal(admit_removal(
        &store,
        &who,
        "production",
        3,
        &observing(observed()),
    ));
    assert!(stopped.message.contains("isn't being removed"));

    // Removed again, it goes as admitted.
    let removing = confirmed(
        &store,
        &who,
        accepting_data(Some("production")),
        &observing(observed()),
    )
    .unwrap();
    let again = taking_off(&removing, "production");
    let claimed = prepare(&store, &again, &["web", "db"]);
    assert!(claimed.intent.target.is_empty(), "a removal ships nothing");
    assert_eq!(claimed.deletes, [held()]);
    succeed(&store, &again, &["web", "db"], deleted(&[held()]));
    assert_eq!(names(&sweep(&store, &who).removed), ["production"]);
    assert_eq!(listed(&store, &who), ["staging*"]);
    // Its name is free again.
    store
        .write(
            &who,
            &CreateEnvironment {
                id: EnvironmentId::parse(uuid(8)).unwrap(),
                project: None,
                name: EnvironmentName::parse("production").unwrap(),
            },
        )
        .unwrap();
}

#[test]
fn a_branch_goes_before_its_parent_and_an_unknown_removal_keeps_it() {
    let (store, who) = branched();
    set_default(&store, &who, "staging");
    assert_eq!(
        listed(&store, &who),
        ["fix<production", "production", "staging*"]
    );

    let parent = refusal(store.write(&who, &accepting_data(Some("production"))));
    assert_eq!(parent.code, RpcErrorCode::Unsupported);
    assert_eq!(parent.details["branches"], json!(["fix"]));
    let unmarked = refusal(admit_removal(
        &store,
        &who,
        "production",
        3,
        &Trusted::default(),
    ));
    assert_eq!(unmarked.code, RpcErrorCode::Conflict);
    assert_eq!(unmarked.details["next"]["tool"], "remove");

    // Its runner lost track after preparing: the outcome is unknown, so it stays.
    let removing = confirmed(&store, &who, request(Some("fix")), &Trusted::default()).unwrap();
    let lost = taking_off(&removing, "fix");
    prepare(&store, &lost, &["web"]);
    store
        .record(&lost, &runner(), RunEvidence::Abandoned)
        .unwrap();
    let stalled = confirmed(&store, &who, request(Some("fix")), &Trusted::default()).unwrap();
    let Removal::Removing { left: fix, .. } = stalled else {
        panic!("removed");
    };
    assert_eq!(fix[0].status, LeftStatus::Stalled);
    assert!(fix[0].why.as_ref().unwrap().contains("remove it by hand"));
    let swept = sweep(&store, &who);
    assert!(swept.closing.is_empty() && swept.removed.is_empty());
    assert_eq!(swept.skipped.len(), 1);
    assert!(
        refusal(admit_removal(&store, &who, "fix", 4, &Trusted::default()))
            .message
            .contains("remove it by hand")
    );

    // Taken off by hand, the sweep deletes it.
    shut_down(&store, &who, ("fix", 5), &[], &Trusted::default()).unwrap();
    run(&store, &id(5), &["web"], Vec::new());
    assert_eq!(names(&sweep(&store, &who).removed), ["fix"]);
    assert_eq!(listed(&store, &who), ["production", "staging*"]);
    // With its Branch gone, the Parent can go too.
    let parent = confirmed(
        &store,
        &who,
        accepting_data(Some("production")),
        &observing(observed()),
    )
    .unwrap();
    assert_eq!(left(parent), [("production".into(), LeftStatus::Removing)]);
}

#[test]
fn a_project_leaves_the_servers_branches_first_and_its_default_last() {
    let (store, who) = branched();
    deploy(&store, &who, "staging", 3, &[]);
    let listed_projects = store
        .read(&who, &ployz_store::ProjectsQuery {})
        .unwrap()
        .projects;
    assert_eq!(
        listed_projects[0].default_environment.as_str(),
        "production"
    );
    let kept = refusal(store.remove_organization(&who));
    assert_eq!(kept.code, RpcErrorCode::Conflict);
    assert_eq!(
        kept.details["next"],
        json!({"tool": "remove", "input": {"project": "shop"}})
    );

    // Branches come off first; the Default Environment only after the rest.
    let removing = confirmed(&store, &who, accepting_data(None), &observing(observed())).unwrap();
    let fix = taking_off(&removing, "fix");
    assert_eq!(
        left(removing),
        [
            ("fix".into(), LeftStatus::Removing),
            ("staging".into(), LeftStatus::Removing),
            ("production".into(), LeftStatus::WaitingTurn),
        ]
    );
    let parent = refusal(admit_removal(
        &store,
        &who,
        "production",
        4,
        &observing(observed()),
    ));
    assert_eq!(parent.details["branches"], json!(["fix"]));

    // Cancelling one of its Deployments stops the whole removal, and leaves the Project whole.
    store.write(&who, &Cancel { deployment: fix }).unwrap();
    assert!(due(&store, &who).is_empty());
    let again = confirmed(&store, &who, accepting_data(None), &observing(observed())).unwrap();
    let Written::RemovalCancelled(cancelled) = store
        .write(
            &who,
            &ployz_store::Command::CancelRemoval(CancelRemoval {
                project: ProjectName::parse("shop").unwrap(),
                environment: None,
            }),
        )
        .unwrap()
    else {
        panic!("not cancelled");
    };
    assert_eq!(
        cancelled.cancelled,
        [taking_off(&again, "fix"), taking_off(&again, "staging")]
    );
    assert_eq!(listed(&store, &who).len(), 3);

    let removing = confirmed(&store, &who, accepting_data(None), &observing(observed())).unwrap();
    run(&store, &taking_off(&removing, "fix"), &["web"], Vec::new());
    run(&store, &taking_off(&removing, "staging"), &[], Vec::new());

    // Last, the Default Environment, under what the removal accepted.
    let swept = sweep(&store, &who);
    assert_eq!(names(&swept.closing), ["production"]);
    let default = admit_removal(&store, &who, "production", 9, &observing(observed())).unwrap();
    assert!(default.remove);
    let claimed = prepare(&store, &id(9), &["web", "db"]);
    assert_eq!(claimed.deletes, [held()]);
    succeed(&store, &id(9), &["web", "db"], deleted(&[held()]));
    assert_eq!(
        names(&sweep(&store, &who).removed),
        ["fix", "production", "staging"]
    );
    assert!(
        store
            .read(&who, &ployz_store::ProjectsQuery {})
            .unwrap()
            .projects
            .is_empty()
    );
    store.remove_organization(&who).unwrap();
    // Its name is free again.
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(10)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid(11)).unwrap(),
            },
        )
        .unwrap();
}

#[test]
fn an_undeployed_project_goes_at_once_and_a_default_branch_comes_off_before_its_parent() {
    let (store, who) = shop();
    assert!(residuals(store.write(&who, &request(None)).unwrap()).is_empty());
    assert!(
        store
            .read(&who, &ployz_store::ProjectsQuery {})
            .unwrap()
            .projects
            .is_empty()
    );

    // The Default Environment may be a Branch: it comes off before its Parent.
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, &["web", "db"]);
    branch(&store, &who, "next");
    deploy(&store, &who, "next", 2, &["web"]);
    set_default(&store, &who, "next");
    let removing = confirmed(&store, &who, accepting_data(None), &observing(observed())).unwrap();
    let next = taking_off(&removing, "next");
    assert_eq!(
        left(removing),
        [
            ("next".into(), LeftStatus::Removing),
            ("production".into(), LeftStatus::WaitingTurn),
        ]
    );
    run(&store, &next, &["web"], Vec::new());
    assert_eq!(names(&sweep(&store, &who).closing), ["production"]);
    admit_removal(&store, &who, "production", 4, &observing(observed())).unwrap();
    run(&store, &id(4), &["web", "db"], deleted(&[held()]));
    assert_eq!(
        names(&sweep(&store, &who).removed),
        ["next", "production", "staging"]
    );
}

#[test]
fn with_no_server_left_a_removal_applies_at_once() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, &["web", "db"]);
    set_default(&store, &who, "staging");
    let none_left = Trusted {
        servers: Some(0),
        ..Trusted::default()
    };
    // No runner and no review: a shutdown completes in configuration only.
    let shutdown = shut_down(&store, &who, ("production", 2), &[], &none_left).unwrap();
    assert_eq!(shutdown.status, DeploymentStatus::Applied);
    assert_eq!(shutdown.outcome, Some(ployz_store::Outcome::Forgotten));
    // Zero enrolled Servers isn't runtime absence: nothing claims it ran or removed anything.
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(2) })
        .unwrap();
    assert_eq!(view.deployment.started_at, None);
    assert!(!view.nodes.is_empty());
    assert!(
        view.nodes
            .iter()
            .all(|node| node.outcome == ployz_store::NodeStatus::Unknown)
    );
    // Removed, it says what may still run where it ran.
    let removal = store
        .write_trusted(&who, &request(Some("production")), &none_left)
        .unwrap();
    assert_eq!(
        residuals(removal),
        [("production".into(), vec!["db".into(), "web".into()])]
    );

    // A Deploy still needs a Server to run it.
    let refused = refusal(store.write_trusted(
        &who,
        &Admit::Deploy(Deploy {
            id: id(3),
            environment: at("staging"),
            services: Vec::new(),
            version: None,
            upload: None,
            accept_volume_loss: Vec::new(),
            message: None,
        }),
        &none_left,
    ));
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(refused.details["next"], "ployz server add");
}

#[test]
fn a_forgotten_cluster_leaves_nothing_deployed_and_its_removal_forgotten() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, &["web", "db"]);
    store
        .write(
            &who,
            &Admit::Deploy(Deploy {
                id: id(3),
                environment: at("staging"),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
        )
        .unwrap();
    let data = || {
        let listing = store
            .read(
                &who,
                &ployz_store::VolumesQuery {
                    environment: at("production"),
                },
            )
            .unwrap()
            .volumes
            .remove(0);
        (listing.deployed, listing.storage_locked)
    };
    assert_eq!(data(), (true, true));
    let applied = store.applied_volumes(&who).unwrap();
    assert_eq!(
        applied
            .iter()
            .map(|lost| format!("{}/{}/{}", lost.project, lost.environment, lost.volume))
            .collect::<Vec<_>>(),
        ["shop/production/data"]
    );

    let forgotten = store
        .system(
            &who.organization,
            &SystemEvent::ClusterForgotten,
            &Trusted::default(),
        )
        .unwrap();
    let Written::Automated(forgotten) = forgotten else {
        panic!("not automated: {forgotten:?}");
    };
    // What might still have run is cancelled; history stays.
    assert_eq!(forgotten.cancelled, vec![id(3)]);
    let status = |n| {
        store
            .read(&who, &ployz_store::DeploymentQuery { id: id(n) })
            .unwrap()
            .deployment
            .status
    };
    assert_eq!(
        (status(1), status(3)),
        (DeploymentStatus::Applied, DeploymentStatus::Cancelled)
    );
    // Nothing reads Deployed, no Namespace is held and storage may be chosen again; the configuration stays.
    assert_eq!(data(), (false, false));
    assert!(store.applied_volumes(&who).unwrap().is_empty());
    assert!(
        store
            .read(&who, &ployz_store::NamespacesQuery {})
            .unwrap()
            .namespaces
            .is_empty()
    );
    assert_eq!(listed(&store, &who), ["production*", "staging"]);
    // Nothing is left for a Deploy to ask before deleting: the data went with the Servers.
    let removals = store
        .read(
            &who,
            &RemovalsQuery {
                environment: at("production"),
                remove: true,
            },
        )
        .unwrap();
    assert!(removals.volumes.is_empty());

    // What ran is still counted as ran, so its shutdown says it was left where it ran.
    set_default(&store, &who, "staging");
    let none_left = Trusted {
        servers: Some(0),
        ..Trusted::default()
    };
    let shutdown = shut_down(&store, &who, ("production", 2), &[], &none_left).unwrap();
    assert_eq!(shutdown.outcome, Some(ployz_store::Outcome::Forgotten));
}

#[test]
fn shutting_down_what_never_ran_applies_at_once() {
    let (store, who) = shop();
    let shutdown = shut_down(&store, &who, ("staging", 1), &[], &Trusted::default()).unwrap();
    assert_eq!(shutdown.status, DeploymentStatus::Applied);
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
    assert_eq!(
        view.deployment.outcome,
        Some(ployz_store::Outcome::NeverRan)
    );
}

#[test]
fn namespaces_name_the_environment_that_owns_each() {
    let (store, who) = shop();
    assert!(
        store
            .read(&who, &ployz_store::NamespacesQuery {})
            .unwrap()
            .namespaces
            .is_empty()
    );
    deploy(&store, &who, "production", 1, &["web"]);
    let owned = store
        .read(&who, &ployz_store::NamespacesQuery {})
        .unwrap()
        .namespaces;
    assert_eq!(owned.len(), 1);
    assert_eq!(
        (owned[0].project.as_str(), owned[0].environment.as_str()),
        ("shop", "production")
    );
    // Another Organization sees none of them.
    let other = Actor::system(OrganizationId::parse("other").unwrap());
    assert!(
        store
            .read(&other, &ployz_store::NamespacesQuery {})
            .unwrap()
            .namespaces
            .is_empty()
    );
}

#[test]
fn a_failed_removal_goes_again_only_on_new_evidence_or_a_retry() {
    let (store, who) = branched();
    let two = Trusted {
        servers: Some(2),
        ..Trusted::default()
    };
    let removing = confirmed(&store, &who, request(Some("fix")), &two).unwrap();
    fail(&store, &taking_off(&removing, "fix"), &["web"]);

    // Cloud sees it due, with where it failed, but nothing changed: it stalls.
    assert_eq!(names(&sweep(&store, &who).closing), ["fix"]);
    assert_eq!(due(&store, &who), [("fix".into(), vec!["a".repeat(32)])]);
    let stalled = refusal(admit_removal(&store, &who, "fix", 3, &two));
    assert_eq!(stalled.code, RpcErrorCode::Conflict);
    assert!(stalled.message.contains("stalled"), "{}", stalled.message);
    assert_eq!(stalled.details["failed_on"], json!(["a".repeat(32)]));

    // A Server left: that may let it go.
    let one = Trusted {
        servers: Some(1),
        ..Trusted::default()
    };
    admit_removal(&store, &who, "fix", 3, &one).unwrap();
    fail(&store, &id(3), &["web"]);
    assert!(
        refusal(admit_removal(&store, &who, "fix", 4, &one))
            .message
            .contains("stalled")
    );

    // Its remover asks again: it goes again.
    let again = confirmed(&store, &who, request(Some("fix")), &one).unwrap();
    assert_eq!(left(again), [("fix".into(), LeftStatus::Removing)]);
}

#[test]
fn a_shutdown_asks_approval_only_when_it_accepts_volume_loss() {
    let (store, who) = branched();
    set_default(&store, &who, "staging");
    let required = Trusted {
        approval: Approval::Required,
        ..observing(observed())
    };
    // Its Services go, and no data: nobody is asked, and nothing is marked.
    let fix = shut_down(&store, &who, ("fix", 3), &[], &required).unwrap();
    run(&store, &fix.id, &["web"], Vec::new());
    assert!(due(&store, &who).is_empty());

    let asked = refusal(shut_down(
        &store,
        &who,
        ("production", 4),
        &["data"],
        &required,
    ));
    assert_eq!(asked.code, RpcErrorCode::from("approval_required"));
    let approved = Trusted {
        approval: Approval::Approved(
            serde_json::from_value(asked.details["approval"].clone()).unwrap(),
        ),
        ..observing(observed())
    };
    let shutdown = shut_down(&store, &who, ("production", 4), &["data"], &approved).unwrap();
    assert!(shutdown.remove);
    assert!(due(&store, &who).is_empty());
}

#[test]
fn a_default_waits_while_its_branch_removal_is_stalled() {
    let (store, who) = branched();
    let removing = confirmed(&store, &who, accepting_data(None), &observing(observed())).unwrap();
    fail(&store, &taking_off(&removing, "fix"), &["web"]);

    assert_eq!(due(&store, &who), [("fix".into(), vec!["a".repeat(32)])]);
    let first = refusal(admit_removal(
        &store,
        &who,
        "production",
        3,
        &observing(observed()),
    ));
    assert_eq!(first.code, RpcErrorCode::Conflict);
    assert_eq!(first.details["branches"], json!(["fix"]));
    assert_eq!(
        listed(&store, &who),
        ["fix<production", "production*", "staging"]
    );
}

#[test]
fn a_data_holding_environment_goes_with_the_version_its_first_refusal_handed_back() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, &["web", "db"]);
    set_default(&store, &who, "staging");
    let trusted = observing(observed());

    // The dashboard sends what the refusal asks for, as it asked.
    let asked = refusal(store.write_trusted(&who, &request(Some("production")), &trusted));
    assert_eq!(asked.code, RpcErrorCode::ConfirmationRequired);
    let removing = store
        .write_trusted(
            &who,
            &Remove {
                version: Some(asked.details["version"].as_str().unwrap().to_owned()),
                accept_volume_loss: serde_json::from_value(asked.details["accept"].clone())
                    .unwrap(),
                ..request(Some("production"))
            },
            &trusted,
        )
        .unwrap();
    run(
        &store,
        &taking_off(&removing, "production"),
        &["web", "db"],
        deleted(&[held()]),
    );
    assert_eq!(names(&sweep(&store, &who).removed), ["production"]);
    assert_eq!(listed(&store, &who), ["staging*"]);
}

#[test]
fn remove_refuses_a_destructive_target_without_its_version() {
    let (store, who) = branched();
    // Only Services go, and that still deletes what runs.
    let unversioned = refusal(store.write(&who, &request(Some("fix"))));
    assert_eq!(unversioned.code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(unversioned.details["effects"][0]["kind"], "removes_service");
    let stale = refusal(store.write(
        &who,
        &Remove {
            version: Some("stale".into()),
            ..request(Some("fix"))
        },
    ));
    assert_eq!(stale.code, RpcErrorCode::ConfirmationRequired);
    // Accepting the data isn't confirming it.
    let unconfirmed =
        refusal(store.write_trusted(&who, &accepting_data(None), &observing(observed())));
    assert_eq!(unconfirmed.code, RpcErrorCode::ConfirmationRequired);
    assert!(due(&store, &who).is_empty(), "a refusal marks nothing");
    deploy(&store, &who, "fix", 3, &["web"]);
}

#[test]
fn every_copy_refuses_a_lost_volume_it_did_not_name() {
    let (store, who) = shop();
    volume(&store, &who, 9, "logs");
    deploy(&store, &who, "production", 1, &["web", "db"]);
    branch(&store, &who, "fix");
    deploy(&store, &who, "fix", 2, &["web"]);
    let data_only = VolumeObservation {
        sought: vec![docker('a', 5).name, docker('a', 9).name],
        held: vec![docker('a', 5)],
        unanswered: Vec::new(),
    };
    let removing = confirmed(
        &store,
        &who,
        Remove {
            every_copy: volumes(&[("production", &["data"])]),
            ..accepting_data(None)
        },
        &observing(data_only),
    )
    .unwrap();
    run(&store, &taking_off(&removing, "fix"), &["web"], Vec::new());

    // `logs` turned up since: nobody gave it up.
    let both = VolumeObservation {
        sought: vec![docker('a', 5).name, docker('a', 9).name],
        held: vec![docker('a', 5), docker('a', 9)],
        unanswered: Vec::new(),
    };
    let refused = refusal(admit_removal(
        &store,
        &who,
        "production",
        3,
        &observing(both),
    ));
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(refused.details["volumes"], json!(["logs"]));
}

/// Remove all of `shop`, giving up `data` as held on `a…` and, under `every_copy`,
/// every copy of it; then admit `production`'s removal once `data` has a mirror on `b…`.
fn mirrored(every_copy: bool) -> (ConfigStore, Result<DeploymentSummary, RpcError>) {
    let (store, who) = branched();
    let consent = if every_copy {
        volumes(&[("production", &["data"])])
    } else {
        BTreeMap::new()
    };
    let removing = confirmed(
        &store,
        &who,
        Remove {
            every_copy: consent,
            ..accepting_data(None)
        },
        &observing(observed()),
    )
    .unwrap();
    run(&store, &taking_off(&removing, "fix"), &["web"], Vec::new());
    let mirror = VolumeObservation {
        sought: vec![held().name],
        held: vec![held(), docker('b', 5)],
        unanswered: Vec::new(),
    };
    let admitted = admit_removal(&store, &who, "production", 3, &observing(mirror));
    (store, admitted)
}

#[test]
fn every_copy_deletes_a_later_mirror_and_a_confirmed_removal_refuses_it() {
    let (store, admitted) = mirrored(true);
    admitted.unwrap();
    assert_eq!(
        prepare(&store, &id(3), &["web", "db"]).deletes,
        [held(), docker('b', 5)]
    );

    let (_, refused) = mirrored(false);
    let refused = refused.unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(refused.details["volumes"], json!(["data"]));
}

#[test]
fn a_remove_never_carries_everything() {
    for written in [
        json!({"project": "shop", "everything": true}),
        json!({"project": "shop", "every_copy": {}, "because": "idle"}),
        json!({"project": "shop", "accept_volume_loss": {"production": {"everything": {"because": "idle"}}}}),
    ] {
        assert!(serde_json::from_value::<Remove>(written).is_err());
    }
    let named: Remove = serde_json::from_value(json!({
        "project": "shop", "environment": "production",
        "accept_volume_loss": {"production": ["data"]}, "every_copy": {"production": ["data"]}
    }))
    .unwrap();
    assert_eq!(named.every_copy, volumes(&[("production", &["data"])]));
}

#[test]
fn nothing_builds_on_a_removal_whose_deployment_failed() {
    let (store, who) = branched();
    let removing = confirmed(&store, &who, accepting_data(None), &observing(observed())).unwrap();
    fail(&store, &taking_off(&removing, "fix"), &["web"]);

    let being_removed = |written: Result<serde_json::Value, RpcError>| {
        let refused = written.unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
        assert!(
            refused.message.contains("being removed"),
            "{}",
            refused.message
        );
        assert!(refused.details["next"]["tool"].is_string());
    };
    let write = |command: ployz_store::Command| {
        store
            .write(&who, &command)
            .map(|written| serde_json::to_value(written).unwrap())
    };
    being_removed(write(ployz_store::Command::Admit(Admit::Deploy(Deploy {
        id: id(3),
        environment: at("production"),
        services: Vec::new(),
        version: None,
        upload: None,
        accept_volume_loss: Vec::new(),
        message: None,
    }))));
    being_removed(write(ployz_store::Command::Publish(Publish {
        environment: at("production"),
        version: None,
        accept_volume_loss: Vec::new(),
    })));
    being_removed(write(ployz_store::Command::CreateBranch(CreateBranch {
        id: EnvironmentId::parse(uuid(8)).unwrap(),
        from: at("production"),
        name: EnvironmentName::parse("other").unwrap(),
        copy: vec![node("web")],
        live: Vec::new(),
        setup: Vec::new(),
        keep: false,
        fix: None,
    })));
    being_removed(write(ployz_store::Command::Sync(SyncChanges {
        from: at("fix"),
        into: Some(at("production")),
        when: None,
        version: "stale".into(),
        picks: None,
        skip: Vec::new(),
        values: BTreeMap::new(),
    })));
    being_removed(write(ployz_store::Command::SetDefaultEnvironment(
        SetDefaultEnvironment {
            environment: at("staging"),
        },
    )));
    being_removed(write(ployz_store::Command::KeepBranch(KeepBranch {
        environment: at("fix"),
        kept: true,
    })));
    being_removed(write(ployz_store::Command::CreateEnvironment(
        CreateEnvironment {
            id: EnvironmentId::parse(uuid(9)).unwrap(),
            project: None,
            name: EnvironmentName::parse("qa").unwrap(),
        },
    )));
}

#[test]
fn a_project_removal_deletes_no_member_alone() {
    let (store, who) = branched();
    let removing = confirmed(&store, &who, accepting_data(None), &observing(observed())).unwrap();
    // `staging` never ran and `fix` is off the Servers: both stay until the Project goes.
    run(&store, &taking_off(&removing, "fix"), &["web"], Vec::new());
    let swept = sweep(&store, &who);
    assert!(swept.removed.is_empty());
    assert_eq!(
        listed(&store, &who),
        ["fix<production", "production*", "staging"]
    );

    admit_removal(&store, &who, "production", 3, &observing(observed())).unwrap();
    run(&store, &id(3), &["web", "db"], deleted(&[held()]));
    assert_eq!(
        names(&sweep(&store, &who).removed),
        ["fix", "production", "staging"]
    );
    assert!(
        store
            .read(&who, &ployz_store::ProjectsQuery {})
            .unwrap()
            .projects
            .is_empty()
    );
}
