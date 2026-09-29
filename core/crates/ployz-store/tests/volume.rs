#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Volumes, mounts and the destructive review of a Deploy that deletes data, through
//! the Store's interface only, on SQLite and on Postgres (see `backend`).

use ployz_core::config::ReviewLifecycleKind;
use ployz_core::{
    DeployOutcome, DeployPreview, DockerVolumeId, DockerVolumeName, MachineId, RpcError,
    RpcErrorCode, ServiceName, VolumeRemoval, VolumeRemovalOutcome,
};
use ployz_store::{
    Actor, Admit, Change, ConfigStore, CreateProject, CreateService, CreateVolume, DataEffect,
    DeploymentId, DiffQuery, DiffView, Edit, EnvironmentId, EnvironmentQuery, EnvironmentRef,
    Mount, NodeStatus, OrganizationId, ProjectId, ProjectName, RemovalsQuery, RemoveVolume,
    RunEvidence, RunnerId, ServiceId, SettingPath, Trusted, VolumeId, VolumeListing, VolumeName,
    VolumeObservation, VolumeQuery, VolumesQuery,
};
use serde_json::{Value, json};

mod backend;

const VOLUME: &str = "00000000-0000-4000-8000-000000000005";
const DOCKER_VOLUME: &str = "shop-production_vol-00000000-0000-4000-8000-000000000005";

/// A store with Project `shop`, Service `web` and Volume `data` mounted at `/data`.
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor {
        organization: OrganizationId::parse("org").unwrap(),
    };
    store
        .create_project(
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
        .create_service(
            &who,
            &CreateService {
                id: ServiceId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("postgres:17".into()),
            },
        )
        .unwrap();
    let created = store
        .create_volume(
            &who,
            &CreateVolume {
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
    assert_eq!(created.staged, ["volumes.data", "web.mounts.data"]);
    (store, who)
}

fn edit(store: &ConfigStore, who: &Actor, change: Change) -> Result<(), RpcError> {
    store
        .edit(
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
    store
        .volumes(who, &VolumesQuery::default())
        .unwrap()
        .volumes
}

fn diff(store: &ConfigStore, who: &Actor) -> DiffView {
    store.diff(who, &DiffQuery::default()).unwrap()
}

fn id(n: u8) -> DeploymentId {
    DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap()
}

fn admit(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    accept: &[&str],
    observed: Option<VolumeObservation>,
) -> Result<(), RpcError> {
    store
        .admit(
            who,
            &Admit {
                id: id(n),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                accept_volume_loss: accept
                    .iter()
                    .map(|name| VolumeName::parse(*name).unwrap())
                    .collect(),
            },
            &Trusted {
                repositories: Vec::new(),
                volumes: observed,
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

/// Claim Deployment `n`, record a Deploy Preview of `web`, then a success that
/// deleted `removed`.
fn run(store: &ConfigStore, n: u8, removed: Vec<VolumeRemoval>) -> ployz_store::Claimed {
    let runner = RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&id(n), &runner).unwrap();
    let operation = json!({"type": "remove_container", "machine_id": "a".repeat(32), "container_id": "a".repeat(64)});
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": "shop-production",
        "operations": [{
            "index": 0, "machine_id": "a".repeat(32), "service_name": "web",
            "operation": operation, "status": {"type": "pending"}
        }],
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(&id(n), &runner, RunEvidence::Prepared(preview))
        .unwrap();
    let outcome: DeployOutcome<ployz_core::ExecutionError> =
        serde_json::from_value(json!({ "type": "success", "completed": [operation] })).unwrap();
    store
        .record(
            &id(n),
            &runner,
            RunEvidence::Executed {
                outcome: Box::new(outcome),
                removed,
            },
        )
        .unwrap();
    claimed
}

fn code(result: Result<impl std::fmt::Debug, RpcError>) -> RpcErrorCode {
    result.unwrap_err().code
}

#[test]
fn a_volume_mounts_by_setting_and_round_trips_get_patch_get() {
    let (store, who) = shop();
    let get = |at: &str| {
        store
            .environment(
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
    let taken = store.create_volume(
        &who,
        &CreateVolume {
            id: VolumeId::parse("00000000-0000-4000-8000-000000000006").unwrap(),
            environment: EnvironmentRef::default(),
            name: VolumeName::parse("data").unwrap(),
            mounts: Vec::new(),
        },
    );
    assert_eq!(code(taken), RpcErrorCode::Conflict);

    let volume = store
        .volume(
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
        .remove_volume(
            &who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(removed.staged, ["volumes.data", "web.mounts.data"]);
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
    let view = store.deployment(&who, &id(1)).unwrap();
    assert!(
        view.nodes
            .iter()
            .all(|node| node.outcome == NodeStatus::Applied)
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
            .removals(&who, &RemovalsQuery::default())
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
        .remove_volume(
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
    let removals = store.removals(&who, &RemovalsQuery::default()).unwrap();
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
    assert_eq!(
        refused.details["version"],
        json!(diff(&store, &who).version)
    );
    assert_eq!(
        refused.details["volumes"][0]["deletes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
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
    // A deletion that failed keeps the Volume deployed and staged for removal.
    assert!(listed(&store, &who)[0].deployed);

    admit(&store, &who, 3, &["data"], Some(observed(&['a']))).unwrap();
    run(&store, 3, vec![removal(VolumeRemovalOutcome::Removed)]);
    assert!(listed(&store, &who).is_empty());
    assert!(diff(&store, &who).changes.is_empty());
}

#[test]
fn a_removed_volume_no_server_holds_needs_no_acceptance() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    store
        .remove_volume(
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
