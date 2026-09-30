#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Volumes, mounts and the destructive review of a Deploy that deletes data, through
//! the Store's interface only, on SQLite and on Postgres (see `backend`).

use ployz_core::config::{ReviewLifecycleKind, VolumeKind};
use ployz_core::{
    DeployOutcome, DeployPreview, DockerVolumeId, DockerVolumeName, MachineId, RpcError,
    RpcErrorCode, ServiceName, VolumeRemoval, VolumeRemovalOutcome,
};
use ployz_store::{
    Actor, Admit, Change, ConfigStore, CreateProject, CreateService, CreateVolume, DataEffect,
    Deploy, DeploymentId, DeploymentStatus, DiffQuery, DiffView, Discard, Edit, EnvironmentId,
    EnvironmentQuery, EnvironmentRef, Mount, NodeStatus, OrganizationId, ProjectId, ProjectName,
    Publish, RemovalsQuery, RemoveVolume, RenameVolume, Retry, RunEvidence, RunnerId,
    ServiceLineageId, SetVolumeStorage, SettingPath, Trusted, VolumeId, VolumeListing, VolumeName,
    VolumeObservation, VolumeQuery, VolumesQuery,
};
use serde_json::{Value, json};

mod backend;

/// Items as their text, to compare with literals.
fn texts<T: ToString>(items: &[T]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

const VOLUME: &str = "00000000-0000-4000-8000-000000000005";
const DOCKER_VOLUME: &str = "shop-production_vol-00000000-0000-4000-8000-000000000005";

/// A store with Project `shop`, Service `web` and Volume `data` mounted at `/data`.
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("postgres:17".into()),
                template: None,
            },
        )
        .unwrap();
    let created = store
        .write(
            &who,
            &CreateVolume {
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse(VOLUME).unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("data").unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: "/data".into(),
                }],
            },
        )
        .unwrap();
    assert_eq!(texts(&created.staged), ["volumes.data", "web.mounts.data"]);
    (store, who)
}

fn edit(store: &ConfigStore, who: &Actor, change: Change) -> Result<(), RpcError> {
    store
        .write(
            who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![change],
            },
        )
        .map(|_| ())
}

fn path(path: &str) -> SettingPath {
    SettingPath::parse(path).unwrap()
}

fn listed(store: &ConfigStore, who: &Actor) -> Vec<VolumeListing> {
    store.read(who, &VolumesQuery::default()).unwrap().volumes
}

fn diff(store: &ConfigStore, who: &Actor) -> DiffView {
    store.read(who, &DiffQuery::default()).unwrap()
}

fn id(n: u8) -> DeploymentId {
    DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap()
}

/// Admit Deployment `n` of every Service. Accepting losses confirms them as a person
/// would: with the version the refusal that listed them handed back.
fn admit(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    accept: &[&str],
    observed: Option<VolumeObservation>,
) -> Result<(), RpcError> {
    match admit_at(store, who, n, accept, observed.clone(), None) {
        Err(refused)
            if !accept.is_empty() && refused.code == RpcErrorCode::ConfirmationRequired =>
        {
            let version = refused.details["version"].as_str().unwrap().to_owned();
            admit_at(store, who, n, accept, observed, Some(version))
        }
        admitted => admitted,
    }
}

fn admit_at(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    accept: &[&str],
    observed: Option<VolumeObservation>,
    version: Option<String>,
) -> Result<(), RpcError> {
    store
        .write_trusted(
            who,
            &Admit::Deploy(Deploy {
                id: id(n),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version,
                upload: None,
                accept_volume_loss: accept
                    .iter()
                    .map(|name| VolumeName::parse(*name).unwrap())
                    .collect(),
                message: None,
            }),
            &Trusted {
                volumes: observed,
                ..Trusted::default()
            },
        )
        .map(|_| ())
}

fn machine(letter: char) -> MachineId {
    MachineId::parse(letter.to_string().repeat(32)).unwrap()
}

fn held(letter: char) -> DockerVolumeId {
    DockerVolumeId {
        machine_id: machine(letter),
        name: DockerVolumeName::parse(DOCKER_VOLUME).unwrap(),
    }
}

/// The Servers answered, and `holders` hold the Volume's data.
fn observed(holders: &[char]) -> VolumeObservation {
    VolumeObservation {
        sought: vec![DockerVolumeName::parse(DOCKER_VOLUME).unwrap()],
        held: holders.iter().map(|letter| held(*letter)).collect(),
        unanswered: Vec::new(),
    }
}

/// The operation a preview of `web` plans.
fn remove_web() -> Value {
    json!({"type": "remove_container", "machine_id": "a".repeat(32), "container_id": "a".repeat(64)})
}

/// Claim Deployment `n` and record a Deploy Preview that plans `services`.
fn prepare(store: &ConfigStore, n: u8, services: &[&str]) -> ployz_store::Claimed {
    let runner = RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&id(n), &runner).unwrap();
    let operations: Vec<Value> = services
        .iter()
        .enumerate()
        .map(|(index, service)| {
            json!({
                "index": index, "machine_id": "a".repeat(32), "service_name": service,
                "operation": remove_web(), "status": {"type": "pending"}
            })
        })
        .collect();
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": "shop-production", "operations": operations,
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(&id(n), &runner, RunEvidence::Prepared(preview))
        .unwrap();
    claimed
}

/// Record that Deployment `n` succeeded at removing `web` and deleted `removed`.
fn execute(store: &ConfigStore, n: u8, removed: Vec<VolumeRemoval>) {
    let outcome: DeployOutcome<ployz_core::ExecutionError> =
        serde_json::from_value(json!({ "type": "success", "completed": [remove_web()] })).unwrap();
    store
        .record(
            &id(n),
            &RunnerId::parse("runner").unwrap(),
            RunEvidence::Executed {
                outcome: Box::new(outcome),
                removed,
            },
        )
        .unwrap();
}

/// Claim Deployment `n`, record a Deploy Preview of `web`, then a success that
/// deleted `removed`.
fn run(store: &ConfigStore, n: u8, removed: Vec<VolumeRemoval>) -> ployz_store::Claimed {
    let claimed = prepare(store, n, &["web"]);
    execute(store, n, removed);
    claimed
}

/// Deployment `n`'s Node Outcomes by node name.
fn outcomes(store: &ConfigStore, who: &Actor, n: u8) -> Vec<(String, NodeStatus)> {
    store
        .read(who, &ployz_store::DeploymentQuery { id: id(n) })
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| (node.node.name().to_owned(), node.outcome))
        .collect()
}

fn code(result: Result<impl std::fmt::Debug, RpcError>) -> RpcErrorCode {
    result.unwrap_err().code
}

#[test]
fn draft_storage_is_explicit_and_locks_even_when_deployment_fails() {
    let create: CreateVolume =
        serde_json::from_value(json!({ "id": VOLUME, "name": "data" })).unwrap();
    assert_eq!(create.storage, VolumeKind::provisioned_default());
    let (store, who) = shop();
    let set = |storage| {
        store.write(
            &who,
            &SetVolumeStorage {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
                storage,
            },
        )
    };
    let changed = set(VolumeKind::provisioned_default()).unwrap();
    assert_eq!(texts(&changed.staged), ["volumes.data"]);
    assert!(!listed(&store, &who)[0].storage_locked);
    let managed = VolumeKind::Provisioned {
        maximum_bytes: 7_000_000_000.try_into().unwrap(),
    };
    set(managed).unwrap();
    admit(&store, &who, 1, &[], None).unwrap();
    let runner = RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&id(1), &runner).unwrap();
    assert_eq!(
        claimed.input["volumes"][0]["storage"],
        json!({ "kind": "provisioned", "maximumBytes": 7_000_000_000_i64 })
    );
    let lowered = serde_json::to_value(&claimed.intent).unwrap();
    assert_eq!(
        lowered["target"][0]["volumes"][0]["source"]["maximum_bytes"],
        7_000_000_000_i64
    );
    store
        .record(
            &id(1),
            &runner,
            RunEvidence::NotExecuted("Preparation stopped".into()),
        )
        .unwrap();
    assert_eq!(
        store
            .read(
                &who,
                &ployz_store::DeploymentQuery {
                    id: ToOwned::to_owned(&id(1))
                }
            )
            .unwrap()
            .deployment
            .status,
        DeploymentStatus::Failed
    );
    assert!(listed(&store, &who)[0].storage_locked);
    for storage in [VolumeKind::Docker {}, VolumeKind::provisioned_default()] {
        let refused = set(storage).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Conflict);
        assert_eq!(refused.details["storage_locked"], true);
    }
    assert!(set(managed).unwrap().staged.is_empty());
    assert_eq!(listed(&store, &who)[0].volume.storage, managed);
}

#[test]
fn a_volume_mounts_by_setting_and_round_trips_get_patch_get() {
    let (store, who) = shop();
    let get = |at: &str| {
        store
            .read(
                &who,
                &EnvironmentQuery {
                    environment: EnvironmentRef::default(),
                    path: Some(path(at)),
                    all: false,
                },
            )
            .unwrap()
    };
    assert_eq!(
        get("web").values.unwrap()["mounts"],
        json!({ "data": "/data" })
    );
    assert_eq!(get("web.mounts.data").settings[0].value, json!("/data"));

    // `get` → `set --patch` → `get` changes nothing, and a patch moves the mount.
    let values = get("web").values.unwrap();
    let patch = |value: Value| Change::Patch {
        path: path("web"),
        value,
    };
    edit(&store, &who, patch(Value::Object(values.clone()))).unwrap();
    assert_eq!(get("web").values.unwrap(), values);
    edit(
        &store,
        &who,
        patch(json!({ "mounts": { "data": "/var/lib/data" } })),
    )
    .unwrap();
    assert_eq!(
        get("web.mounts.data").settings[0].value,
        json!("/var/lib/data")
    );

    for (change, expected) in [
        (
            Change::Set {
                path: path("web.mounts.data"),
                value: json!("relative"),
            },
            RpcErrorCode::InvalidArgument,
        ),
        (
            Change::Set {
                path: path("web.mounts.logs"),
                value: json!("/logs"),
            },
            RpcErrorCode::NotFound,
        ),
    ] {
        assert_eq!(code(edit(&store, &who, change)), expected);
    }
    let taken = store.write(
        &who,
        &CreateVolume {
            storage: ployz_core::config::VolumeKind::Docker {},
            id: VolumeId::parse("00000000-0000-4000-8000-000000000006").unwrap(),
            environment: EnvironmentRef::default(),
            name: VolumeName::parse("data").unwrap(),
            mounts: Vec::new(),
        },
    );
    assert_eq!(code(taken), RpcErrorCode::Conflict);

    let volume = store
        .read(
            &who,
            &VolumeQuery {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(volume.lineage.as_str(), VOLUME);
    assert_eq!(volume.volume.mounts[0].path, "/var/lib/data");
    assert_eq!(volume.volume.change, Some(ReviewLifecycleKind::Create));
    assert!(!volume.volume.deployed);
}

#[test]
fn an_undeployed_volume_is_removed_without_servers() {
    let (store, who) = shop();
    let removed = store
        .write(
            &who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(texts(&removed.staged), ["volumes.data", "web.mounts.data"]);
    assert!(listed(&store, &who).is_empty());
    assert!(
        diff(&store, &who)
            .changes
            .iter()
            .all(|change| change.data.is_none())
    );
    // No evidence and no Servers: nothing deployed can lose data.
    admit(&store, &who, 1, &[], None).unwrap();
}

#[test]
fn a_deployed_volume_removal_discards_alone() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    store
        .write(
            &who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    assert!(!diff(&store, &who).changes.is_empty());
    store
        .write(
            &who,
            &Discard {
                environment: EnvironmentRef::default(),
                path: Some(SettingPath::parse("volumes.data").unwrap()),
                version: None,
            },
        )
        .unwrap();
    // The Volume stays, and so does its mount, which went with it.
    assert!(diff(&store, &who).changes.is_empty());
}

#[test]
fn a_deployed_volume_is_applied_and_detaching_keeps_it() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let claimed = run(&store, 1, Vec::new());
    let spec = &claimed.intent.target[0];
    assert!(
        serde_json::to_string(spec)
            .unwrap()
            .contains("vol-00000000-0000-4000-8000-000000000005"),
        "the Deploy Intent mounts the Volume"
    );
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap();
    assert!(
        view.nodes
            .iter()
            .all(|node| node.outcome == NodeStatus::Deployed)
    );
    assert!(listed(&store, &who)[0].deployed);

    edit(
        &store,
        &who,
        Change::Unset {
            path: path("web.mounts.data"),
        },
    )
    .unwrap();
    let web = diff(&store, &who).changes.remove(0);
    assert_eq!(web.settings[0].path, "web.mounts.data");
    assert_eq!(web.data, Some(DataEffect::Kept));
    // The Volume stays, so the Deploy deletes nothing and needs no evidence.
    assert!(
        store
            .read(&who, &RemovalsQuery::default())
            .unwrap()
            .volumes
            .is_empty()
    );
    admit(&store, &who, 2, &[], None).unwrap();
    run(&store, 2, Vec::new());
    assert!(listed(&store, &who)[0].deployed);
}

#[test]
fn removing_a_deployed_volume_needs_evidence_and_a_typed_acceptance() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    store
        .write(
            &who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    let volume = diff(&store, &who)
        .changes
        .into_iter()
        .find(|change| change.name == "data")
        .unwrap();
    assert_eq!(volume.lifecycle, ReviewLifecycleKind::Delete);
    assert_eq!(volume.data, Some(DataEffect::Deleted));
    let removals = store.read(&who, &RemovalsQuery::default()).unwrap();
    assert_eq!(removals.volumes[0].docker_volume.as_str(), DOCKER_VOLUME);

    // Missing, off-target or incomplete evidence fails closed.
    let elsewhere = VolumeObservation {
        sought: vec![DockerVolumeName::parse("other").unwrap()],
        ..VolumeObservation::default()
    };
    let silent = VolumeObservation {
        unanswered: vec![machine('b')],
        ..observed(&['a'])
    };
    for evidence in [None, Some(elsewhere), Some(silent)] {
        assert_eq!(
            code(admit(&store, &who, 2, &["data"], evidence)),
            RpcErrorCode::Unavailable
        );
    }
    // Without acceptance it names what goes and where; a name it doesn't remove refuses.
    let refused = admit(&store, &who, 2, &[], Some(observed(&['a', 'b']))).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(refused.details["accept"], json!(["data"]));
    let bound = refused.details["version"].as_str().unwrap().to_owned();
    assert!(bound.starts_with(&format!("{}:", diff(&store, &who).version)));
    assert_eq!(
        refused.details["volumes"][0]["deletes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // The confirmation binds the exact holders: by name alone it asks again with
    // the same version; once a Server holds it that the confirmation didn't name,
    // with a new one.
    let again = |version: Option<&String>, holders: &[char]| {
        admit_at(
            &store,
            &who,
            2,
            &["data"],
            Some(observed(holders)),
            version.cloned(),
        )
        .unwrap_err()
    };
    let by_name = again(None, &['a', 'b']);
    assert_eq!(by_name.code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(by_name.details["version"], json!(bound));
    let new_holder = again(Some(&bound), &['a', 'b', 'c']);
    assert_eq!(new_holder.code, RpcErrorCode::ConfirmationRequired);
    assert_ne!(new_holder.details["version"], json!(bound));
    assert_eq!(
        code(admit(
            &store,
            &who,
            2,
            &["data", "logs"],
            Some(observed(&['a']))
        )),
        RpcErrorCode::InvalidArgument
    );

    // Accepted, the Deployment deletes exactly the Docker Volumes reviewed.
    admit(&store, &who, 2, &["data"], Some(observed(&['a']))).unwrap();
    let removal = |outcome| VolumeRemoval {
        id: held('a'),
        outcome,
    };
    let failed = VolumeRemovalOutcome::Failed {
        error: RpcError {
            code: RpcErrorCode::Unavailable,
            message: "in use".into(),
            details: Value::Null,
        },
    };
    let claimed = run(&store, 2, vec![removal(failed)]);
    assert_eq!(claimed.deletes, [held('a')]);
    // A deletion that failed keeps the Volume deployed and staged for removal: the
    // Deployment failed, and its Node Outcome says the Volume did.
    assert!(listed(&store, &who)[0].deployed);
    let failed = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(2) })
        .unwrap();
    assert_eq!(failed.deployment.status, DeploymentStatus::Failed);
    let data = failed
        .nodes
        .iter()
        .find(|node| node.node.name() == "data")
        .unwrap();
    assert_eq!(data.outcome, NodeStatus::Failed);

    admit(&store, &who, 3, &["data"], Some(observed(&['a']))).unwrap();
    // A Volume being removed waits while it runs.
    prepare(&store, 3, &["web"]);
    assert!(outcomes(&store, &who, 3).contains(&("data".to_owned(), NodeStatus::Pending)));
    execute(&store, 3, vec![removal(VolumeRemovalOutcome::Removed)]);
    assert!(listed(&store, &who).is_empty());
    assert!(diff(&store, &who).changes.is_empty());
    let removed = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(3) })
        .unwrap();
    assert_eq!(removed.deployment.status, DeploymentStatus::Applied);
    let data = removed
        .nodes
        .iter()
        .find(|node| node.node.name() == "data")
        .unwrap();
    assert_eq!(data.outcome, NodeStatus::Removed);
}

#[test]
fn a_removed_volume_no_server_holds_needs_no_acceptance() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    store
        .write(
            &who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    admit(&store, &who, 2, &[], Some(observed(&[]))).unwrap();
    assert!(run(&store, 2, Vec::new()).deletes.is_empty());
    assert!(listed(&store, &who).is_empty());
}

#[test]
fn a_retry_deletes_exactly_what_its_source_accepted_without_a_new_review() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    store
        .write(
            &who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    admit(&store, &who, 2, &["data"], Some(observed(&['a']))).unwrap();
    let runner = RunnerId::parse("runner").unwrap();
    store.claim(&id(2), &runner).unwrap();
    store
        .record(&id(2), &runner, RunEvidence::NotExecuted("down".into()))
        .unwrap();
    // The retry needs no evidence: it ships the source's frozen, accepted identities.
    store
        .write_trusted(
            &who,
            &Admit::Retry(Retry {
                id: id(3),
                deployment: id(2),
            }),
            &Trusted::default(),
        )
        .unwrap();
    let claimed = store.claim(&id(3), &runner).unwrap();
    assert_eq!(claimed.deletes, [held('a')]);
}

#[test]
fn publishing_a_deployed_volumes_removal_needs_evidence_and_acceptance() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    store
        .write(
            &who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    let publish =
        |accept: &[&str], observed: Option<VolumeObservation>, version: Option<String>| {
            store.write_trusted(
                &who,
                &Publish {
                    environment: EnvironmentRef::default(),
                    version,
                    accept_volume_loss: accept
                        .iter()
                        .map(|name| VolumeName::parse(*name).unwrap())
                        .collect(),
                },
                &Trusted {
                    volumes: observed,
                    ..Trusted::default()
                },
            )
        };
    assert_eq!(code(publish(&[], None, None)), RpcErrorCode::Unavailable);
    let refused = publish(&[], Some(observed(&['a'])), None).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(refused.details["accept"], json!(["data"]));
    let bound = refused.details["version"].as_str().unwrap().to_owned();
    let published = publish(&["data"], Some(observed(&['a'])), Some(bound)).unwrap();
    assert!(published.created);
}

#[test]
fn a_renamed_volume_keeps_its_mounts_and_refuses_a_taken_name() {
    let (store, who) = shop();
    let rename = |name: &str| {
        store.write(
            &who,
            &ployz_store::RenameVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
                name: VolumeName::parse(name).unwrap(),
            },
        )
    };
    assert!(rename("data").unwrap().staged.is_empty());
    let renamed = rename("files").unwrap();
    assert_eq!(texts(&renamed.staged), ["volumes.files"]);
    assert_eq!(texts(&[renamed.volume.name]), ["files"]);
    assert_eq!(listed(&store, &who)[0].mounts[0].path, "/data");
    let gone = rename("other").unwrap_err();
    assert_eq!(gone.code, RpcErrorCode::NotFound);
}

#[test]
fn a_deployed_volume_rename_discards_by_its_row() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    store
        .write(
            &who,
            &RenameVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
                name: VolumeName::parse("files").unwrap(),
            },
        )
        .unwrap();
    let changes = diff(&store, &who).changes;
    let row = &changes[0].settings[0];
    assert_eq!(row.path, "volumes.files.name");
    assert!(row.can_restore);
    store
        .write(
            &who,
            &Discard {
                environment: EnvironmentRef::default(),
                path: Some(path(&row.path)),
                version: None,
            },
        )
        .unwrap();
    assert!(diff(&store, &who).changes.is_empty());
    assert_eq!(
        texts(&[listed(&store, &who)[0].volume.name.clone()]),
        ["data"]
    );
}

#[test]
fn a_new_unmounted_volume_is_not_deployed_by_a_deploy_that_succeeded() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateVolume {
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("spare").unwrap(),
                mounts: Vec::new(),
            },
        )
        .unwrap();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    // No Service mounts it, so nothing that ran created its storage.
    assert!(
        outcomes(&store, &who, 1).contains(&("spare".to_owned(), NodeStatus::NotAttempted)),
        "{:?}",
        outcomes(&store, &who, 1)
    );
    assert!(
        diff(&store, &who)
            .changes
            .iter()
            .any(|change| change.name == "spare")
    );
}

#[test]
fn replaying_what_executing_did_changes_nothing_after_the_volume_landed() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    let first = outcomes(&store, &who, 1);
    // Applied State now holds data: the replay is judged on its evidence, not on it.
    execute(&store, 1, Vec::new());
    assert_eq!(outcomes(&store, &who, 1), first);
}

#[test]
fn a_new_volume_is_deployed_once_the_service_mounting_it_is_confirmed() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    prepare(&store, 1, &["web"]);
    let runner = RunnerId::parse("runner").unwrap();
    store
        .record(
            &id(1),
            &runner,
            RunEvidence::Confirmed(vec![ServiceName::parse("web").unwrap()]),
        )
        .unwrap();
    assert_eq!(
        outcomes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Deployed),
            ("data".to_owned(), NodeStatus::Deployed),
        ]
    );
    // Applied State holds the Volume web mounts, whatever happens to the runner.
    store
        .record(&id(1), &runner, RunEvidence::Abandoned)
        .unwrap();
    assert!(diff(&store, &who).changes.is_empty());
}

#[test]
fn a_volume_confirmed_mid_run_still_reads_deployed_once_the_deploy_ends() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    prepare(&store, 1, &["web"]);
    store
        .record(
            &id(1),
            &RunnerId::parse("runner").unwrap(),
            RunEvidence::Confirmed(vec![ServiceName::parse("web").unwrap()]),
        )
        .unwrap();
    // Applied State holds data already; the Deploy still created it.
    execute(&store, 1, Vec::new());
    assert_eq!(
        outcomes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Deployed),
            ("data".to_owned(), NodeStatus::Deployed),
        ]
    );
}

#[test]
fn a_shared_volume_lands_with_the_one_confirmed_service_whatever_the_other_does() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000004").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("api").unwrap(),
                image: Some("caddy:2".into()),
                template: None,
            },
        )
        .unwrap();
    edit(
        &store,
        &who,
        Change::Set {
            path: path("api.mounts.data"),
            value: json!("/srv"),
        },
    )
    .unwrap();
    admit(&store, &who, 1, &[], None).unwrap();
    prepare(&store, 1, &["web", "api"]);
    let runner = RunnerId::parse("runner").unwrap();
    store
        .record(
            &id(1),
            &runner,
            RunEvidence::Confirmed(vec![ServiceName::parse("web").unwrap()]),
        )
        .unwrap();
    // api is still pending, yet the storage web runs on is confirmed.
    assert!(outcomes(&store, &who, 1).contains(&("data".to_owned(), NodeStatus::Deployed)));
    store
        .record(&id(1), &runner, RunEvidence::Abandoned)
        .unwrap();
    assert!(
        !diff(&store, &who)
            .changes
            .iter()
            .any(|change| change.name == "data" || change.name == "web")
    );
}

#[test]
fn a_running_deploy_shows_a_volume_unchanged_once_its_preview_plans_nothing_that_mounts_it() {
    let (store, who) = shop();
    // A new Volume waits with the Service that mounts it.
    admit(&store, &who, 1, &[], None).unwrap();
    prepare(&store, 1, &["web"]);
    assert_eq!(
        outcomes(&store, &who, 1),
        [
            ("web".to_owned(), NodeStatus::Pending),
            ("data".to_owned(), NodeStatus::Pending),
        ]
    );
    execute(&store, 1, Vec::new());

    // Once deployed, a Deploy whose preview plans nothing for `web` settles both.
    admit(&store, &who, 2, &[], None).unwrap();
    prepare(&store, 2, &[]);
    assert_eq!(
        outcomes(&store, &who, 2),
        [
            ("web".to_owned(), NodeStatus::Unchanged),
            ("data".to_owned(), NodeStatus::Unchanged),
        ]
    );
}
