#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Environment lifecycle through the Store's interface only, on SQLite and on
//! Postgres (see `backend`): listing, the Default Environment, and the one removal
//! path, which takes what still runs off the Servers before deleting anything,
//! for one Environment or a whole Project.

use ployz_core::{
    DeployOutcome, DeployPreview, DockerVolumeId, DockerVolumeName, MachineId, RpcError,
    RpcErrorCode, ServiceName, VolumeRemoval, VolumeRemovalOutcome,
};
use ployz_store::{
    Actor, Admit, Cancel, ConfigStore, CreateBranch, CreateEnvironment, CreateProject,
    CreateService, CreateVolume, Deploy, DeploymentId, DeploymentStatus, EnvironmentId,
    EnvironmentName, EnvironmentRef, EnvironmentRemoved, EnvironmentsQuery, Mount, OrganizationId,
    ProjectId, ProjectName, Removal, RemovalsQuery, RemoveEnvironment, RemoveProject, Retry,
    RunEvidence, RunnerId, ServiceLineageId, SetDefaultEnvironment, Sweep, SystemEvent, Teardown,
    Trusted, VolumeId, VolumeName, VolumeObservation,
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
    store
        .write(
            &who,
            &CreateVolume {
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse(uuid(5)).unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("data").unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("db").unwrap(),
                    path: "/data".into(),
                }],
            },
        )
        .unwrap();
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

fn id(n: u8) -> DeploymentId {
    DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap()
}

/// Admit Deployment `n`. Accepting losses confirms them as a person would: with
/// the version the refusal that listed them handed back.
fn admit(
    store: &ConfigStore,
    who: &Actor,
    environment: &str,
    n: u8,
    remove: bool,
    accept: &[&str],
    volumes: Option<VolumeObservation>,
) -> Result<ployz_store::DeploymentSummary, RpcError> {
    let accept_volume_loss: Vec<VolumeName> = accept
        .iter()
        .map(|name| VolumeName::parse(*name).unwrap())
        .collect();
    let request = |version: Option<String>| match remove {
        true => Admit::Remove(Removal {
            id: id(n),
            environment: at(environment),
            version,
            accept_volume_loss: accept_volume_loss.clone(),
            close: false,
        }),
        false => Admit::Deploy(Deploy {
            id: id(n),
            environment: at(environment),
            services: Vec::new(),
            version,
            upload: None,
            accept_volume_loss: accept_volume_loss.clone(),
            message: None,
        }),
    };
    let trusted = Trusted {
        volumes,
        ..Trusted::default()
    };
    match store.write_trusted(who, &request(None), &trusted) {
        Err(refused)
            if !accept.is_empty() && refused.code == RpcErrorCode::ConfirmationRequired =>
        {
            let version = refused.details["version"].as_str().unwrap().to_owned();
            store.write_trusted(who, &request(Some(version)), &trusted)
        }
        admitted => admitted,
    }
}

fn runner() -> RunnerId {
    RunnerId::parse("runner").unwrap()
}

/// Claim Deployment `n` and record a Deploy Preview touching `services`.
fn prepare(store: &ConfigStore, n: u8, services: &[&str]) -> ployz_store::Claimed {
    let claimed = store.claim(&id(n), &runner()).unwrap();
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
        .record(&id(n), &runner(), RunEvidence::Prepared(preview))
        .unwrap();
    claimed
}

fn operation(index: usize) -> serde_json::Value {
    json!({"type": "remove_container", "machine_id": "a".repeat(32),
           "container_id": format!("{index:x}").repeat(64)})
}

/// Record that Deployment `n` succeeded for `services` and deleted `removed`.
fn succeed(store: &ConfigStore, n: u8, services: &[&str], removed: Vec<VolumeRemoval>) {
    let outcome: DeployOutcome<ployz_core::ExecutionError> = serde_json::from_value(json!({
        "type": "success",
        "completed": (0..services.len()).map(operation).collect::<Vec<_>>()
    }))
    .unwrap();
    store
        .record(
            &id(n),
            &runner(),
            RunEvidence::Executed {
                outcome: Box::new(outcome),
                removed,
            },
        )
        .unwrap();
}

fn deploy(store: &ConfigStore, who: &Actor, environment: &str, n: u8, services: &[&str]) {
    admit(store, who, environment, n, false, &[], None).unwrap();
    prepare(store, n, services);
    succeed(store, n, services, Vec::new());
}

fn remove(
    store: &ConfigStore,
    who: &Actor,
    environment: &str,
) -> Result<Teardown<EnvironmentRemoved>, RpcError> {
    store.write(
        who,
        &RemoveEnvironment {
            environment: at(environment),
        },
    )
}

/// What a removal deleted; any other answer fails the test.
fn removed<T: std::fmt::Debug>(result: Result<Teardown<T>, RpcError>) -> T {
    match result.unwrap() {
        Teardown::Removed(removed) => removed,
        other @ (Teardown::Waiting { .. } | Teardown::NeedsRemoval { .. }) => {
            panic!("not removed: {other:?}")
        }
    }
}

/// The Environment still on the Servers that stops a removal, and whether it needs
/// a removal Deployment next (else a Deployment of it hasn't ended).
fn on_servers<T: std::fmt::Debug>(result: Result<Teardown<T>, RpcError>) -> (String, bool) {
    match result.unwrap() {
        Teardown::NeedsRemoval { environment, .. } => (environment.to_string(), true),
        Teardown::Waiting { environment, .. } => (environment.to_string(), false),
        Teardown::Removed(removed) => panic!("removed: {removed:?}"),
    }
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

fn held() -> DockerVolumeId {
    DockerVolumeId {
        machine_id: MachineId::parse("a".repeat(32)).unwrap(),
        name: DockerVolumeName::parse(format!("shop-production_vol-{}", uuid(5))).unwrap(),
    }
}

fn observed() -> VolumeObservation {
    VolumeObservation {
        sought: vec![held().name],
        held: vec![held()],
        unanswered: Vec::new(),
    }
}

fn refusal(result: Result<impl std::fmt::Debug, RpcError>) -> RpcError {
    result.unwrap_err()
}

#[test]
fn environments_list_and_an_undeployed_one_is_removed_without_servers() {
    let (store, who) = shop();
    assert_eq!(listed(&store, &who), ["production*", "staging"]);

    let default = refusal(remove(&store, &who, "production"));
    assert_eq!(default.code, RpcErrorCode::Conflict);
    assert!(default.message.contains("Default Environment"));

    set_default(&store, &who, "staging");
    assert_eq!(listed(&store, &who), ["production", "staging*"]);
    // Nothing of production ever ran, so it goes at once, with no Servers.
    removed(remove(&store, &who, "production"));
    assert_eq!(listed(&store, &who), ["staging*"]);
    assert_eq!(
        refusal(remove(&store, &who, "production")).code,
        RpcErrorCode::NotFound
    );
}

#[test]
fn a_deployed_root_leaves_the_servers_before_the_store() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, &["web", "db"]);
    set_default(&store, &who, "staging");

    assert_eq!(
        on_servers(remove(&store, &who, "production")),
        ("production".into(), true)
    );

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
    let unobserved = refusal(admit(&store, &who, "production", 2, true, &[], None));
    assert_eq!(unobserved.code, RpcErrorCode::Unavailable);
    let unaccepted = refusal(admit(
        &store,
        &who,
        "production",
        2,
        true,
        &[],
        Some(observed()),
    ));
    assert_eq!(unaccepted.code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(unaccepted.details["accept"], json!(["data"]));
    let queued = admit(
        &store,
        &who,
        "production",
        2,
        true,
        &["data"],
        Some(observed()),
    )
    .unwrap();
    assert!(queued.remove);

    // Nothing branches from it, and it isn't gone until the removal applies.
    let branching = store.write(
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
    );
    assert_eq!(refusal(branching).code, RpcErrorCode::Conflict);
    assert_eq!(
        on_servers(remove(&store, &who, "production")),
        ("production".into(), false)
    );

    // Cancelled, it stays deployed; retried, it runs as admitted.
    store.write(&who, &Cancel { deployment: id(2) }).unwrap();
    assert_eq!(
        on_servers(remove(&store, &who, "production")),
        ("production".into(), true)
    );
    let retried = store
        .write_trusted(
            &who,
            &Admit::Retry(Retry {
                id: id(3),
                deployment: id(2),
            }),
            &Trusted::default(),
        )
        .unwrap();
    assert!(retried.remove);
    let claimed = prepare(&store, 3, &["web", "db"]);
    assert!(claimed.intent.target.is_empty(), "a removal ships nothing");
    assert_eq!(claimed.deletes, [held()]);
    succeed(
        &store,
        3,
        &["web", "db"],
        vec![VolumeRemoval {
            id: held(),
            outcome: VolumeRemovalOutcome::Removed,
        }],
    );
    let listing = store.read(&who, &EnvironmentsQuery::default()).unwrap();
    let removal = listing.environments[0].removal.as_ref().unwrap();
    assert_eq!(removal.status, DeploymentStatus::Applied);

    removed(remove(&store, &who, "production"));
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
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, &["web", "db"]);
    store
        .write(
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
        )
        .unwrap();
    deploy(&store, &who, "fix", 2, &["web"]);
    set_default(&store, &who, "staging");
    assert_eq!(
        listed(&store, &who),
        ["fix<production", "production", "staging*"]
    );

    for refused in [
        refusal(remove(&store, &who, "production")),
        refusal(admit(&store, &who, "production", 3, true, &[], None)),
    ] {
        assert_eq!(refused.code, RpcErrorCode::Conflict);
        assert_eq!(refused.details["branches"], json!(["fix"]));
    }

    // Its runner lost track after preparing: the outcome is unknown, so it stays.
    admit(&store, &who, "fix", 3, true, &[], None).unwrap();
    prepare(&store, 3, &["web"]);
    store
        .record(&id(3), &runner(), RunEvidence::Abandoned)
        .unwrap();
    let unknown = store.read(&who, &EnvironmentsQuery::default()).unwrap();
    assert_eq!(
        unknown.environments[0].removal.as_ref().unwrap().status,
        DeploymentStatus::Unknown
    );
    assert_eq!(
        on_servers(remove(&store, &who, "fix")),
        ("fix".into(), true)
    );

    admit(&store, &who, "fix", 4, true, &[], None).unwrap();
    prepare(&store, 4, &["web"]);
    succeed(&store, 4, &["web"], Vec::new());
    removed(remove(&store, &who, "fix"));
    assert_eq!(listed(&store, &who), ["production", "staging*"]);
    // With its Branch gone, the Parent can go too.
    admit(
        &store,
        &who,
        "production",
        5,
        true,
        &["data"],
        Some(observed()),
    )
    .unwrap();
}

fn remove_shop(store: &ConfigStore, who: &Actor) -> Result<Teardown<Vec<String>>, RpcError> {
    store
        .write(
            who,
            &RemoveProject {
                project: ProjectName::parse("shop").unwrap(),
            },
        )
        .map(|teardown| match teardown {
            Teardown::Removed(removed) => Teardown::Removed(
                removed
                    .environments
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            ),
            Teardown::Waiting {
                environment,
                deployment,
            } => Teardown::Waiting {
                environment,
                deployment,
            },
            Teardown::NeedsRemoval {
                environment,
                deployment,
            } => Teardown::NeedsRemoval {
                environment,
                deployment,
            },
        })
}

/// Take `environment` off the Servers with Deployment `n`, which applies.
fn take_off(store: &ConfigStore, who: &Actor, environment: &str, n: u8, services: &[&str]) {
    admit(store, who, environment, n, true, &[], None).unwrap();
    prepare(store, n, services);
    succeed(store, n, services, Vec::new());
}

#[test]
fn a_project_leaves_the_servers_branches_first_and_its_default_last() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, &["web", "db"]);
    store
        .write(
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
        )
        .unwrap();
    deploy(&store, &who, "fix", 2, &["web"]);
    deploy(&store, &who, "staging", 3, &[]);
    let listed = store
        .read(&who, &ployz_store::ProjectsQuery {})
        .unwrap()
        .projects;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].default_environment.as_str(), "production");
    assert_eq!(
        listed[0]
            .environments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["fix", "production", "staging"]
    );
    let kept = refusal(store.remove_organization(&who));
    assert_eq!(kept.code, RpcErrorCode::Conflict);
    assert_eq!(
        kept.details["next"],
        json!("ployz project rm shop --confirm shop")
    );

    // Branches come off first; the Default Environment only after the rest.
    assert_eq!(on_servers(remove_shop(&store, &who)), ("fix".into(), true));
    let parent = refusal(admit(&store, &who, "production", 4, true, &[], None));
    assert_eq!(parent.details["branches"], json!(["fix"]));

    // A removal in flight, then cancelled, leaves the Project whole.
    admit(&store, &who, "fix", 5, true, &[], None).unwrap();
    assert_eq!(on_servers(remove_shop(&store, &who)), ("fix".into(), false));
    store.write(&who, &Cancel { deployment: id(5) }).unwrap();
    assert_eq!(on_servers(remove_shop(&store, &who)), ("fix".into(), true));

    take_off(&store, &who, "fix", 6, &["web"]);
    assert_eq!(
        on_servers(remove_shop(&store, &who)),
        ("staging".into(), true)
    );
    let default = refusal(admit(&store, &who, "production", 7, true, &[], None));
    assert_eq!(default.details["environments"], json!(["staging"]));
    take_off(&store, &who, "staging", 8, &[]);

    // Last, the Default Environment, under the same destructive review.
    let unaccepted = refusal(admit(
        &store,
        &who,
        "production",
        9,
        true,
        &[],
        Some(observed()),
    ));
    assert_eq!(unaccepted.code, RpcErrorCode::ConfirmationRequired);
    admit(
        &store,
        &who,
        "production",
        9,
        true,
        &["data"],
        Some(observed()),
    )
    .unwrap();
    prepare(&store, 9, &["web", "db"]);
    succeed(
        &store,
        9,
        &["web", "db"],
        vec![VolumeRemoval {
            id: held(),
            outcome: VolumeRemovalOutcome::Removed,
        }],
    );

    assert_eq!(
        removed(remove_shop(&store, &who)),
        ["fix", "staging", "production"]
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
    assert_eq!(
        removed(remove_shop(&store, &who)),
        ["staging", "production"]
    );

    // The Default Environment may be a Branch: it comes off before its Parent.
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, &["web", "db"]);
    store
        .write(
            &who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(7)).unwrap(),
                from: at("production"),
                name: EnvironmentName::parse("next").unwrap(),
                copy: vec![node("web")],
                live: Vec::new(),
                setup: Vec::new(),
                keep: false,
                fix: None,
            },
        )
        .unwrap();
    deploy(&store, &who, "next", 2, &["web"]);
    set_default(&store, &who, "next");
    assert_eq!(on_servers(remove_shop(&store, &who)), ("next".into(), true));
    take_off(&store, &who, "next", 3, &["web"]);
    admit(
        &store,
        &who,
        "production",
        4,
        true,
        &["data"],
        Some(observed()),
    )
    .unwrap();
    prepare(&store, 4, &["web", "db"]);
    succeed(
        &store,
        4,
        &["web", "db"],
        vec![VolumeRemoval {
            id: held(),
            outcome: VolumeRemovalOutcome::Removed,
        }],
    );
    assert_eq!(
        removed(remove_shop(&store, &who)),
        ["staging", "next", "production"]
    );
}

#[test]
fn a_closed_branch_goes_on_its_own_once_its_removal_applied() {
    let (store, who) = shop();
    deploy(&store, &who, "production", 1, &["web", "db"]);
    store
        .write(
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
        )
        .unwrap();
    deploy(&store, &who, "fix", 2, &["web"]);
    store
        .write(
            &who,
            &Admit::Remove(Removal {
                id: id(3),
                environment: at("fix"),
                version: None,
                accept_volume_loss: Vec::new(),
                close: true,
            }),
        )
        .unwrap();
    let sweep = || {
        store
            .system(
                &who.organization,
                &SystemEvent::Sweep(Sweep { now: 0 }),
                &Trusted::default(),
            )
            .unwrap();
    };
    // Not while its removal may still run.
    sweep();
    assert_eq!(
        listed(&store, &who),
        ["fix<production", "production*", "staging"]
    );
    prepare(&store, 3, &["web"]);
    succeed(&store, 3, &["web"], Vec::new());
    // Nobody comes back to delete it: the sweep does.
    sweep();
    assert_eq!(listed(&store, &who), ["production*", "staging"]);
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
    // No runner and no review: it completes in configuration only.
    let removal = store
        .write_trusted(
            &who,
            &Admit::Remove(Removal {
                id: id(2),
                environment: at("production"),
                version: None,
                accept_volume_loss: Vec::new(),
                close: false,
            }),
            &none_left,
        )
        .unwrap();
    assert_eq!(removal.status, DeploymentStatus::Applied);
    assert_eq!(removal.outcome, Some(ployz_store::Outcome::Forgotten));
    // Zero enrolled Servers isn't runtime absence: nothing claims it ran or removed anything.
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(2) })
        .unwrap();
    assert_eq!(
        view.deployment.outcome,
        Some(ployz_store::Outcome::Forgotten)
    );
    assert_eq!(view.deployment.started_at, None);
    assert!(!view.nodes.is_empty());
    assert!(
        view.nodes
            .iter()
            .all(|node| node.outcome == ployz_store::NodeStatus::Unknown)
    );
    removed(remove(&store, &who, "production"));

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
    assert_eq!(refused.code, RpcErrorCode::Unavailable);
}

#[test]
fn shutting_down_what_never_ran_applies_at_once() {
    let (store, who) = shop();
    let shutdown = admit(&store, &who, "staging", 1, true, &[], None).unwrap();
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
