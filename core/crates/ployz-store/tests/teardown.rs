#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Environment lifecycle through the Store's interface only, on SQLite and on
//! Postgres (see `backend`): listing, the Default Environment, and the one removal
//! path, which takes what still runs off the Servers before deleting anything.

use ployz_core::{
    DeployOutcome, DeployPreview, DockerVolumeId, DockerVolumeName, MachineId, RpcError,
    RpcErrorCode, ServiceName, VolumeRemoval, VolumeRemovalOutcome,
};
use ployz_store::{
    Actor, Admit, Cancel, ConfigStore, CreateBranch, CreateEnvironment, CreateProject,
    CreateService, CreateVolume, DeploymentId, DeploymentStatus, EnvironmentId, EnvironmentName,
    EnvironmentRef, EnvironmentsQuery, Mount, OrganizationId, ProjectId, ProjectName,
    RemovalsQuery, RemoveEnvironment, RunEvidence, RunnerId, ServiceId, SetDefaultEnvironment,
    Trusted, VolumeId, VolumeName, VolumeObservation,
};
use serde_json::json;

mod backend;

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
    let who = Actor {
        organization: OrganizationId::parse("org").unwrap(),
    };
    store
        .create_project(
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
            .create_service(
                &who,
                &CreateService {
                    id: ServiceId::parse(uuid(n)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some("postgres:17".into()),
                },
            )
            .unwrap();
    }
    store
        .create_volume(
            &who,
            &CreateVolume {
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
        .create_environment(
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

fn admit(
    store: &ConfigStore,
    who: &Actor,
    environment: &str,
    n: u8,
    remove: bool,
    accept: &[&str],
    volumes: Option<VolumeObservation>,
) -> Result<ployz_store::DeploymentSummary, RpcError> {
    store.admit(
        who,
        &Admit {
            id: id(n),
            environment: at(environment),
            services: Vec::new(),
            version: None,
            upload: None,
            retry: None,
            remove,
            accept_volume_loss: accept
                .iter()
                .map(|name| VolumeName::parse(*name).unwrap())
                .collect(),
        },
        &Trusted {
            volumes,
            ..Trusted::default()
        },
    )
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

fn remove(store: &ConfigStore, who: &Actor, environment: &str) -> Result<(), RpcError> {
    store
        .remove_environment(
            who,
            &RemoveEnvironment {
                environment: at(environment),
            },
        )
        .map(|_| ())
}

fn set_default(store: &ConfigStore, who: &Actor, environment: &str) {
    store
        .set_default_environment(
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
        .environments(who, &EnvironmentsQuery::default())
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
    assert_eq!(
        refusal(admit(&store, &who, "production", 1, true, &[], None)).code,
        RpcErrorCode::Conflict
    );

    set_default(&store, &who, "staging");
    assert_eq!(listed(&store, &who), ["production", "staging*"]);
    // Nothing of production ever ran, so it goes at once, with no Servers.
    remove(&store, &who, "production").unwrap();
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

    let deployed = refusal(remove(&store, &who, "production"));
    assert_eq!(deployed.code, RpcErrorCode::Conflict);
    assert_eq!(deployed.details["deployed"], json!(true));

    // The removal deletes the deployed Volume, under the same review as any Deploy.
    let removals = store
        .removals(
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
    let branching = store.create_branch(
        &who,
        &CreateBranch {
            id: EnvironmentId::parse(uuid(7)).unwrap(),
            from: at("production"),
            name: EnvironmentName::parse("fix").unwrap(),
            copy: vec!["web".into()],
            live: Vec::new(),
            setup: Vec::new(),
            keep: false,
            fix: None,
        },
    );
    assert_eq!(refusal(branching).code, RpcErrorCode::Conflict);
    assert_eq!(
        refusal(remove(&store, &who, "production")).code,
        RpcErrorCode::Conflict
    );

    // Cancelled, it stays deployed; retried, it runs as admitted.
    store.cancel(&who, &Cancel { deployment: id(2) }).unwrap();
    assert_eq!(
        refusal(remove(&store, &who, "production")).details["deployed"],
        json!(true)
    );
    let retried = store
        .admit(
            &who,
            &Admit {
                id: id(3),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                retry: Some(id(2)),
                remove: false,
                accept_volume_loss: Vec::new(),
            },
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
    let listing = store
        .environments(&who, &EnvironmentsQuery::default())
        .unwrap();
    let removal = listing.environments[0].removal.as_ref().unwrap();
    assert_eq!(removal.status, DeploymentStatus::Applied);

    remove(&store, &who, "production").unwrap();
    assert_eq!(listed(&store, &who), ["staging*"]);
    // Its name is free again.
    store
        .create_environment(
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
        .create_branch(
            &who,
            &CreateBranch {
                id: EnvironmentId::parse(uuid(7)).unwrap(),
                from: at("production"),
                name: EnvironmentName::parse("fix").unwrap(),
                copy: vec!["web".into()],
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
    let unknown = store
        .environments(&who, &EnvironmentsQuery::default())
        .unwrap();
    assert_eq!(
        unknown.environments[0].removal.as_ref().unwrap().status,
        DeploymentStatus::Unknown
    );
    assert_eq!(
        refusal(remove(&store, &who, "fix")).details["deployed"],
        json!(true)
    );

    admit(&store, &who, "fix", 4, true, &[], None).unwrap();
    prepare(&store, 4, &["web"]);
    succeed(&store, 4, &["web"], Vec::new());
    remove(&store, &who, "fix").unwrap();
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
