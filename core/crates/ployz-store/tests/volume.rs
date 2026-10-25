#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Volumes, mounts and the destructive review of a Deploy that deletes data, through
//! the Store's interface only, on SQLite and on Postgres (see `backend`).

use ployz_core::config::{ReviewLifecycleKind, VolumeKind};
use ployz_core::{
    DeployOutcome, DeployPreview, DockerVolumeId, DockerVolumeName, MachineId,
    ProvisionedVolumeMaximumBytes, RpcError, RpcErrorCode, ServiceName, VolumeRemoval,
    VolumeRemovalOutcome,
};
use ployz_store::{
    Actor, Admit, Approval, ApprovalDigest, Batch, BatchCommand, Change, ConfigStore,
    CreateProject, CreateService, CreateVolume, DataEffect, Deploy, DeploymentId, DeploymentQuery,
    DeploymentStatus, DiffQuery, DiffView, Discard, Edit, EnvironmentId, EnvironmentQuery,
    EnvironmentRef, Mount, NodeStatus, OrganizationId, Outcome, ProjectId, ProjectName, Publish,
    Published, RemovalsQuery, RemoveVolume, RenameVolume, Retry, RunEvidence, RunnerId,
    ServiceLineageId, SetVolumeSharedWrites, SetVolumeStorage, SettingPath, Trusted, VolumeId,
    VolumeListing, VolumeName, VolumeObservation, VolumeQuery, VolumesQuery,
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
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
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
                prune: false,
                no_prune: false,
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
        failure: None,
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
                progress: Vec::new(),
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
fn draft_storage_is_explicit_and_locks_once_a_run_prepares_it() {
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
    // Preparation that failed executed nothing, so no Server holds the Volume yet.
    assert!(!listed(&store, &who)[0].storage_locked);
    admit(&store, &who, 2, &[], None).unwrap();
    prepare(&store, 2, &["web"]);
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
fn a_deployment_admitted_before_an_earlier_run_fixed_the_storage_is_refused_at_claim() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    let runner = RunnerId::parse("runner").unwrap();
    store.claim(&id(1), &runner).unwrap();
    // #1 is still building, so nothing fixed data's storage yet.
    store
        .write(
            &who,
            &SetVolumeStorage {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
                storage: VolumeKind::provisioned_default(),
            },
        )
        .unwrap();
    admit(&store, &who, 2, &[], None).unwrap();
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": "shop-production",
        "operations": [{
            "index": 0, "machine_id": "a".repeat(32), "service_name": "web",
            "operation": remove_web(), "status": {"type": "pending"}
        }],
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(&id(1), &runner, RunEvidence::Prepared(preview))
        .unwrap();

    let refused = store.claim(&id(2), &runner).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(refused.details["storage_locked"], true);
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(2) })
        .unwrap();
    assert_eq!(view.deployment.status, DeploymentStatus::Failed);
}

#[test]
fn a_deploy_after_one_that_applied_a_managed_volume_marks_it_deployed() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &SetVolumeStorage {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
                storage: VolumeKind::provisioned_default(),
            },
        )
        .unwrap();
    let runner = RunnerId::parse("runner").unwrap();
    admit(&store, &who, 1, &[], None).unwrap();
    store.claim(&id(1), &runner).unwrap();
    store
        .record(
            &id(1),
            &runner,
            RunEvidence::NotExecuted("Preparation stopped".into()),
        )
        .unwrap();
    admit(&store, &who, 2, &[], None).unwrap();
    let failed_before = prepare(&store, 2, &["web"]);
    assert!(failed_before.intent.deployed_volumes.is_empty());
    store
        .record(
            &id(2),
            &runner,
            RunEvidence::Confirmed(vec![ServiceName::parse("web").unwrap()]),
        )
        .unwrap();
    execute(&store, 2, Vec::new());

    admit(&store, &who, 3, &[], None).unwrap();
    let claimed = store.claim(&id(3), &runner).unwrap();

    assert_eq!(
        texts(&claimed.intent.deployed_volumes.iter().collect::<Vec<_>>()),
        [DOCKER_VOLUME]
    );
    assert_eq!(claimed.input["volumes"][0]["deployed"], true);
}

#[test]
fn a_volume_created_with_its_storage_locked_refuses_other_storage_before_any_deploy() {
    let (store, who) = shop();
    let managed = VolumeKind::Provisioned {
        maximum_bytes: 5_000_000_000.try_into().unwrap(),
    };
    store
        .write(
            &who,
            &CreateVolume {
                shared_writes: false,
                storage_locked: true,
                restored_from: None,
                storage: managed,
                id: VolumeId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("restored").unwrap(),
                mounts: Vec::new(),
            },
        )
        .unwrap();
    let named = |name: &str| {
        listed(&store, &who)
            .into_iter()
            .find(|listing| listing.volume.name.as_str() == name)
            .unwrap()
    };
    assert!(named("restored").storage_locked);
    let larger = VolumeKind::Provisioned {
        maximum_bytes: 10_000_000_000.try_into().unwrap(),
    };
    for storage in [VolumeKind::Docker {}, larger] {
        let refused = store
            .write(
                &who,
                &SetVolumeStorage {
                    environment: EnvironmentRef::default(),
                    volume: VolumeName::parse("restored").unwrap(),
                    storage,
                },
            )
            .unwrap_err();
        assert_eq!(refused.details["storage_locked"], true);
    }
    assert_eq!(named("restored").volume.storage, managed);
    assert!(!named("data").storage_locked);
}

#[test]
fn a_volume_restores_only_from_a_volume_in_its_environment() {
    let (store, who) = shop();
    let restore = |volume: &str| {
        store.write(
            &who,
            &CreateVolume {
                shared_writes: false,
                storage_locked: true,
                restored_from: Some(ployz_core::config::RestoredFrom {
                    volume: volume.to_owned(),
                    at: 1_790_000_000,
                }),
                storage: VolumeKind::Docker {},
                id: VolumeId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("restored").unwrap(),
                mounts: Vec::new(),
            },
        )
    };
    let elsewhere = restore("00000000-0000-4000-8000-0000000000ff").unwrap_err();
    assert_eq!(elsewhere.code, RpcErrorCode::NotFound, "{elsewhere:?}");
    assert!(
        listed(&store, &who)
            .iter()
            .all(|listing| listing.volume.name.as_str() != "restored")
    );
    restore(VOLUME).unwrap();
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

    let before_invalid = get("web").values.unwrap();
    for (change, expected) in [
        (
            Change::Set {
                path: path("web.mounts.data"),
                value: json!("relative"),
            },
            RpcErrorCode::InvalidArgument,
        ),
        (
            patch(json!({ "cpuLimit": 1, "mounts": { "data": "/data\u{0}bad" } })),
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
        assert_eq!(get("web").values.unwrap(), before_invalid);
    }
    let taken = store.write(
        &who,
        &CreateVolume {
            shared_writes: false,
            storage_locked: false,
            restored_from: None,
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
fn a_volume_never_mounts_over_the_kernel_filesystems_or_another_mount() {
    let (store, who) = shop();
    let create = |n: u8, name: &str, at: &str| {
        store
            .write(
                &who,
                &CreateVolume {
                    shared_writes: false,
                    storage_locked: false,
                    restored_from: None,
                    storage: ployz_core::config::VolumeKind::Docker {},
                    id: VolumeId::parse(format!("00000000-0000-4000-8000-0000000000{n:02}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: VolumeName::parse(name).unwrap(),
                    mounts: vec![Mount {
                        service: ServiceName::parse("web").unwrap(),
                        path: at.into(),
                    }],
                },
            )
            .map(|_| ())
    };
    let set = |at: &str, to: &str| {
        edit(
            &store,
            &who,
            Change::Set {
                path: path(at),
                value: json!(to),
            },
        )
    };

    for at in ["/proc", "/sys/fs", "/dev/shm", "/proc/", "/var/../dev"] {
        let refused = create(10, "kernel", at).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{at}");
        assert!(
            refused.message.contains("which the kernel fills"),
            "{}",
            refused.message
        );
    }

    for at in ["/data", "/data/", "/data/.", "//data"] {
        let refused = create(10, "again", at).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Conflict, "{at}");
        assert_eq!(
            refused.message,
            "web already mounts Volume data at /data; pick another directory"
        );
    }
    assert_eq!(listed(&store, &who).len(), 1);

    create(11, "logs", "/devices").unwrap();
    let moved = set("web.mounts.logs", "/data/").unwrap_err();
    assert_eq!(moved.code, RpcErrorCode::Conflict);
    let kernel = set("web.mounts.logs", "/sys").unwrap_err();
    assert_eq!(kernel.code, RpcErrorCode::InvalidArgument);

    // A Volume moving to another spelling of its own path is not a clash.
    set("web.mounts.data", "/data/").unwrap();

    // Mounts are judged where the whole edit leaves them, whatever order it applies them.
    let patch = |mounts: Value| {
        edit(
            &store,
            &who,
            Change::Patch {
                path: path("web"),
                value: json!({ "mounts": mounts }),
            },
        )
    };
    patch(json!({ "data": "/devices", "logs": "/data" })).unwrap();
    patch(json!({ "data": "/archive", "logs": "/devices" })).unwrap();
    let both = patch(json!({ "data": "/same", "logs": "/same/" })).unwrap_err();
    assert_eq!(both.code, RpcErrorCode::Conflict);
}

#[test]
fn a_discard_never_puts_a_mount_back_where_another_volume_now_mounts() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    edit(
        &store,
        &who,
        Change::Set {
            path: path("web.mounts.data"),
            value: json!("/moved"),
        },
    )
    .unwrap();
    store
        .write(
            &who,
            &CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse("00000000-0000-4000-8000-000000000012").unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("cache").unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: "/data/".into(),
                }],
            },
        )
        .unwrap();
    let refused = store
        .write(
            &who,
            &ployz_store::Discard {
                environment: EnvironmentRef::default(),
                target: Some(ployz_store::DiscardTarget::Head),
                path: Some(path("web.mounts.data")),
                version: None,
            },
        )
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert_eq!(
        refused.message,
        "web already mounts Volume cache at /data; pick another directory"
    );
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
                target: Some(ployz_store::DiscardTarget::Head),
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
            code: RpcErrorCode::Conflict,
            message: "provider stderr token=review-sentinel".into(),
            details: Value::Null,
            cause: vec!["token=review-sentinel".into()],
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
    // The reason is fixed words: what the provider printed stays out of it.
    let outcome = serde_json::to_value(&failed.deployment.outcome).unwrap();
    assert_eq!(
        outcome["reason"],
        json!("data: deleting its data failed: it is still in use on a Server")
    );
    let shown = serde_json::to_string(&failed).unwrap();
    assert!(!shown.contains("review-sentinel"), "{shown}");

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
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &Trusted::default(),
        )
        .unwrap();
    let claimed = store.claim(&id(3), &runner).unwrap();
    assert_eq!(claimed.deletes, [held('a')]);
}

/// Claim Deployment `n` and run `web` and `api`: both deploy, or `web` deploys and
/// `api` fails. It deletes `removed`.
fn run_both(store: &ConfigStore, n: u8, api_fails: bool, removed: Vec<VolumeRemoval>) {
    let runner = RunnerId::parse("runner").unwrap();
    let op = |container: &str| json!({"type": "remove_container", "machine_id": "a".repeat(32), "container_id": container.repeat(64)});
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": "shop-production",
        "operations": [
            {"index": 0, "machine_id": "a".repeat(32), "service_name": "web",
             "operation": op("a"), "status": {"type": "pending"}},
            {"index": 1, "machine_id": "a".repeat(32), "service_name": "api",
             "operation": op("b"), "status": {"type": "pending"}}
        ],
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store.claim(&id(n), &runner).unwrap();
    store
        .record(&id(n), &runner, RunEvidence::Prepared(preview))
        .unwrap();
    let outcome = match api_fails {
        false => json!({ "type": "success", "completed": [op("a"), op("b")] }),
        true => json!({
            "type": "failed", "completed": [op("a")],
            "failed": {"type": "operation", "operation": op("b"), "error": {"type": "cancelled"}},
            "unexecuted": []
        }),
    };
    store
        .record(
            &id(n),
            &runner,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(serde_json::from_value(outcome).unwrap()),
                removed,
            },
        )
        .unwrap();
}

/// How the Deployment that brought `data` back ended.
#[derive(Clone, Copy, PartialEq)]
enum Ended {
    Applied,
    Failed,
    /// It prepared, then lost track, so Applied State never lists `data` again.
    Unknown,
}

/// #2 deleted `data`, then #3 brought it back, and ended as `ended`. Redeploying #2
/// rolls #3 back: its data is reviewed afresh and deleted only once a person
/// accepts it again.
fn rolling_back_asks_again_before_deleting_data_a_newer_deployment_brought_back(ended: Ended) {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-00000000000a").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("api").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    admit(&store, &who, 1, &[], None).unwrap();
    run_both(&store, 1, false, Vec::new());
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
    let removed = || VolumeRemoval {
        id: held('a'),
        outcome: VolumeRemovalOutcome::Removed,
    };
    run_both(&store, 2, false, vec![removed()]);
    // The Volume comes back under the same identity, so its new data lives in the
    // same Docker Volume #2 deleted.
    let first = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(1) })
        .unwrap()
        .deployment
        .saved;
    let preview = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: EnvironmentRef::default(),
                revision: first,
                action: ployz_store::HistoryAction::Restore,
                change: None,
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::StageHistory {
                environment: EnvironmentRef::default(),
                revision: first,
                action: ployz_store::HistoryAction::Restore,
                change: None,
                version: preview.version,
                accept_overwrite: true,
            },
        )
        .unwrap();
    admit(&store, &who, 3, &[], None).unwrap();
    match ended {
        Ended::Applied | Ended::Failed => {
            run_both(&store, 3, ended == Ended::Failed, Vec::new());
            assert!(listed(&store, &who)[0].deployed);
        }
        Ended::Unknown => {
            let runner = RunnerId::parse("runner").unwrap();
            prepare(&store, 3, &["web", "api"]);
            store
                .record(&id(3), &runner, RunEvidence::Abandoned)
                .unwrap();
            assert!(!listed(&store, &who)[0].deployed);
        }
    }

    let rolled_back = store
        .read(&who, &ployz_store::DeploymentQuery { id: id(2) })
        .unwrap();
    assert_eq!(rolled_back.rolls_back, [3]);
    // What a rollback asks about is what it deletes now, not what #2 accepted.
    let removals = store
        .read(
            &who,
            &RemovalsQuery {
                deployment: Some(id(2)),
                ..RemovalsQuery::default()
            },
        )
        .unwrap();
    assert_eq!(removals.volumes.len(), 1);
    let redeploy = |accept: &[&str], version: Option<String>| {
        store.write_trusted(
            &who,
            &Admit::Retry(Retry {
                id: id(4),
                deployment: id(2),
                rolls_back: Some(vec![3]),
                version,
                accept_volume_loss: accept
                    .iter()
                    .map(|name| VolumeName::parse(*name).unwrap())
                    .collect(),
            }),
            &Trusted {
                volumes: Some(observed(&['a'])),
                ..Trusted::default()
            },
        )
    };
    let asked = redeploy(&[], None).unwrap_err();
    assert_eq!(asked.code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(asked.details["volumes"][0]["name"], json!("data"));
    let version = asked.details["version"].as_str().unwrap().to_owned();
    redeploy(&["data"], Some(version)).unwrap();
    assert_eq!(prepare(&store, 4, &["web"]).deletes, [held('a')]);
}

#[test]
fn rolling_back_past_a_deployment_that_brought_a_volume_back_asks_before_deleting_it() {
    rolling_back_asks_again_before_deleting_data_a_newer_deployment_brought_back(Ended::Applied);
}

#[test]
fn rolling_back_past_a_partly_applied_deployment_asks_before_deleting_what_it_brought_back() {
    rolling_back_asks_again_before_deleting_data_a_newer_deployment_brought_back(Ended::Failed);
}

#[test]
fn rolling_back_past_a_deployment_that_lost_track_asks_before_deleting_what_it_may_have_brought_back()
 {
    rolling_back_asks_again_before_deleting_data_a_newer_deployment_brought_back(Ended::Unknown);
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
    let publish = |accept: &[&str],
                   observed: Option<VolumeObservation>,
                   version: Option<&str>,
                   approval: Approval| {
        store.write_trusted(
            &who,
            &Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: version.map(Into::into),
                message: None,
                accept_volume_loss: accept
                    .iter()
                    .map(|name| VolumeName::parse(*name).unwrap())
                    .collect(),
            },
            &Trusted {
                volumes: observed,
                approval,
                ..Trusted::default()
            },
        )
    };
    let free = Approval::NotRequired;
    assert_eq!(
        code(publish(&[], None, None, free.clone())),
        RpcErrorCode::Unavailable
    );
    let partial = VolumeObservation {
        unanswered: vec![machine('b')],
        ..observed(&['a'])
    };
    assert_eq!(
        code(publish(&["data"], Some(partial), None, free.clone())),
        RpcErrorCode::Unavailable
    );
    let refused = publish(&[], Some(observed(&['a'])), None, free.clone()).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(refused.details["accept"], json!(["data"]));
    assert_eq!(
        refused.message,
        "Next deploy deletes the data of data. Accept each by name"
    );
    let bound = refused.details["version"].as_str().unwrap().to_owned();
    let bound = Some(bound.as_str());

    let wrong = publish(&["cache"], Some(observed(&['a'])), bound, free.clone()).unwrap_err();
    assert_eq!(wrong.code, RpcErrorCode::InvalidArgument);
    let unbound = publish(&["data"], Some(observed(&['a'])), None, free.clone()).unwrap_err();
    assert_eq!(unbound.code, RpcErrorCode::ConfirmationRequired);
    // A Server found holding more data changes what the acceptance names.
    let grown = publish(&["data"], Some(observed(&['a', 'b'])), bound, free.clone()).unwrap_err();
    assert_eq!(grown.code, RpcErrorCode::ConfirmationRequired);
    assert_ne!(grown.details["version"].as_str(), bound);

    let asked = publish(&["data"], Some(observed(&['a'])), bound, Approval::Required).unwrap_err();
    assert_eq!(asked.code.as_str(), "approval_required");
    let approved = Approval::Approved(
        ApprovalDigest::parse(asked.details["approval"].as_str().unwrap()).unwrap(),
    );
    let unaccepted = publish(&[], Some(observed(&['a'])), None, approved.clone()).unwrap_err();
    assert_eq!(unaccepted.code, RpcErrorCode::ConfirmationRequired);

    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("web.replicas").unwrap(),
                    value: json!(2),
                }],
            },
        )
        .unwrap();
    let stale = publish(&["data"], Some(observed(&['a'])), bound, free.clone()).unwrap_err();
    assert_eq!(stale.code, RpcErrorCode::Conflict);
    let fresh = publish(&[], Some(observed(&['a'])), None, free.clone()).unwrap_err();
    let fresh = fresh.details["version"].as_str().unwrap().to_owned();
    let published = publish(&["data"], Some(observed(&['a'])), Some(&fresh), free).unwrap();
    assert!(published.created);
}

/// Save Working State, accepting losses as [`admit`] does.
fn save(
    store: &ConfigStore,
    who: &Actor,
    accept: &[&str],
    observed: Option<VolumeObservation>,
) -> Result<Published, RpcError> {
    let save = |version: Option<String>| {
        store.write_trusted(
            who,
            &Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version,
                message: None,
                accept_volume_loss: accept
                    .iter()
                    .map(|name| VolumeName::parse(*name).unwrap())
                    .collect(),
            },
            &Trusted {
                volumes: observed.clone(),
                ..Trusted::default()
            },
        )
    };
    match save(None) {
        Err(refused)
            if !accept.is_empty() && refused.code == RpcErrorCode::ConfirmationRequired =>
        {
            save(Some(
                refused.details["version"].as_str().unwrap().to_owned(),
            ))
        }
        saved => saved,
    }
}

/// Deploy `data`, remove it, and Save that removal.
fn save_removal(store: &ConfigStore, who: &Actor, accept: &[&str], holders: &[char]) -> Published {
    admit(store, who, 1, &[], None).unwrap();
    run(store, 1, Vec::new());
    store
        .write(
            who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    let saved = save(store, who, accept, Some(observed(holders))).unwrap();
    assert!(saved.created);
    saved
}

fn save_accepting(store: &ConfigStore, who: &Actor, holders: &[char]) {
    save_removal(store, who, &["data"], holders);
}

#[test]
fn a_loss_accepted_on_save_is_not_asked_again_by_the_next_deploy() {
    let (store, who) = shop();
    save_accepting(&store, &who, &['a']);
    admit(&store, &who, 2, &[], Some(observed(&['a']))).unwrap();
    assert_eq!(run(&store, 2, Vec::new()).deletes, [held('a')]);
}

#[test]
fn a_server_holding_more_since_the_save_asks_again() {
    let (store, who) = shop();
    save_accepting(&store, &who, &['a']);
    let refused = admit(&store, &who, 2, &[], Some(observed(&['a', 'b']))).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(refused.details["accept"], json!(["data"]));
    // Evidence is still required: consent never stands in for looking.
    assert_eq!(
        code(admit(&store, &who, 2, &[], None)),
        RpcErrorCode::Unavailable
    );
}

#[test]
fn an_unchanged_save_records_consent_for_a_removal_saved_when_no_server_held_it() {
    let (store, who) = shop();
    let first = save_removal(&store, &who, &[], &[]);
    let renewed = save(&store, &who, &["data"], Some(observed(&['a']))).unwrap();
    assert!(!renewed.created);
    assert_eq!(renewed.saved, first.saved);
    admit(&store, &who, 2, &[], Some(observed(&['a']))).unwrap();
    assert_eq!(run(&store, 2, Vec::new()).deletes, [held('a')]);
}

#[test]
fn an_unchanged_save_asks_again_once_a_server_holds_more_and_renews_consent() {
    let (store, who) = shop();
    save_accepting(&store, &who, &['a']);
    assert_eq!(
        code(save(&store, &who, &[], Some(observed(&['a', 'b'])))),
        RpcErrorCode::ConfirmationRequired
    );
    assert!(
        !save(&store, &who, &[], Some(observed(&['a'])))
            .unwrap()
            .created
    );
    assert!(
        !save(&store, &who, &["data"], Some(observed(&['a', 'b'])))
            .unwrap()
            .created
    );
    admit(&store, &who, 2, &[], Some(observed(&['a', 'b']))).unwrap();
    assert_eq!(run(&store, 2, Vec::new()).deletes, [held('a'), held('b')]);
}

#[test]
fn a_deploy_that_leaves_out_the_volume_keeps_the_saved_consent() {
    let (store, who) = shop();
    save_accepting(&store, &who, &['a']);
    add_api(&store, &who);
    store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: id(2),
                environment: EnvironmentRef::default(),
                services: vec![ServiceName::parse("api").unwrap()],
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &Trusted {
                volumes: Some(observed(&['a'])),
                ..Trusted::default()
            },
        )
        .unwrap();
    assert!(prepare(&store, 2, &["api"]).deletes.is_empty());
    execute(&store, 2, Vec::new());
    admit(&store, &who, 3, &[], Some(observed(&['a']))).unwrap();
    assert_eq!(run(&store, 3, Vec::new()).deletes, [held('a')]);
}

#[test]
fn fewer_holders_than_were_accepted_still_deletes_them() {
    let (store, who) = shop();
    save_accepting(&store, &who, &['a', 'b']);
    admit(&store, &who, 2, &[], Some(observed(&['b']))).unwrap();
    assert_eq!(run(&store, 2, Vec::new()).deletes, [held('b')]);
}

#[test]
fn publishing_two_volumes_removal_needs_each_sought_and_each_accepted() {
    const LOGS: &str = "00000000-0000-4000-8000-000000000006";
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateVolume {
                storage_locked: false,
                restored_from: None,
                shared_writes: false,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse(LOGS).unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("logs").unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: "/logs".into(),
                }],
            },
        )
        .unwrap();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    for volume in ["data", "logs"] {
        store
            .write(
                &who,
                &RemoveVolume {
                    environment: EnvironmentRef::default(),
                    volume: VolumeName::parse(volume).unwrap(),
                },
            )
            .unwrap();
    }
    let logs = DockerVolumeName::parse(format!("shop-production_vol-{LOGS}")).unwrap();
    let both = VolumeObservation {
        sought: vec![
            DockerVolumeName::parse(DOCKER_VOLUME).unwrap(),
            logs.clone(),
        ],
        held: vec![
            held('a'),
            DockerVolumeId {
                machine_id: machine('a'),
                name: logs,
            },
        ],
        unanswered: Vec::new(),
        failure: None,
    };
    let publish = |accept: &[&str], observed: VolumeObservation, version: Option<&str>| {
        store.write_trusted(
            &who,
            &Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: version.map(Into::into),
                message: None,
                accept_volume_loss: accept
                    .iter()
                    .map(|name| VolumeName::parse(*name).unwrap())
                    .collect(),
            },
            &Trusted {
                volumes: Some(observed),
                approval: Approval::NotRequired,
                ..Trusted::default()
            },
        )
    };
    assert_eq!(
        code(publish(&["data", "logs"], observed(&['a']), None)),
        RpcErrorCode::Unavailable
    );
    let refused = publish(&[], both.clone(), None).unwrap_err();
    assert_eq!(refused.details["accept"], json!(["data", "logs"]));
    let bound = refused.details["version"].as_str().unwrap().to_owned();
    let one = publish(&["data"], both.clone(), Some(&bound)).unwrap_err();
    assert_eq!(one.code, RpcErrorCode::ConfirmationRequired);
    assert!(
        publish(&["data", "logs"], both, Some(&bound))
            .unwrap()
            .created
    );
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
                target: Some(ployz_store::DiscardTarget::Head),
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
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
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
fn a_deploy_with_a_blocked_service_fails_and_leaves_its_applied_state_alone() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000004").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("api").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    admit(&store, &who, 1, &[], None).unwrap();
    let runner = RunnerId::parse("runner").unwrap();
    store.claim(&id(1), &runner).unwrap();
    // The planner found data without a writer: it planned api and blocked web.
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": "shop-production",
        "operations": [{
            "index": 0, "machine_id": "a".repeat(32), "service_name": "api",
            "operation": remove_web(), "status": {"type": "pending"}
        }],
        "warnings": [], "would_remove": [], "preserved_volumes": [],
        "blocked": [{
            "service": "web",
            "error": {
                "code": "conflict",
                "message": "Volume data has no writer; fsn-2 holds its only copy",
                "details": {"reason": "no_writer"}
            }
        }]
    }))
    .unwrap();
    store
        .record(&id(1), &runner, RunEvidence::Prepared(preview))
        .unwrap();
    execute(&store, 1, Vec::new());

    let view = store.read(&who, &DeploymentQuery { id: id(1) }).unwrap();
    assert_eq!(view.deployment.status, DeploymentStatus::Failed);
    assert!(
        matches!(
            &view.deployment.outcome,
            Some(Outcome::Executed { reason: Some(reason), .. })
                if reason == "web: Volume data has no writer; fsn-2 holds its only copy"
        ),
        "{:?}",
        view.deployment.outcome
    );
    let outcomes = outcomes(&store, &who, 1);
    for expected in [
        ("api".to_owned(), NodeStatus::Deployed),
        ("web".to_owned(), NodeStatus::NotAttempted),
        ("data".to_owned(), NodeStatus::NotAttempted),
    ] {
        assert!(outcomes.contains(&expected), "{expected:?} in {outcomes:?}");
    }
    // Applied State took api, and still waits for web and the Volume it mounts.
    let pending: Vec<String> = diff(&store, &who)
        .changes
        .iter()
        .map(|change| change.name.clone())
        .collect();
    assert!(pending.contains(&"web".to_owned()), "{pending:?}");
    assert!(pending.contains(&"data".to_owned()), "{pending:?}");
    assert!(!pending.contains(&"api".to_owned()), "{pending:?}");
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
    set_shared_writes(&store, &who, true).unwrap();
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
fn a_shared_volume_one_planned_service_mounts_locks_whatever_blocks_the_other() {
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
    set_shared_writes(&store, &who, true).unwrap();
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
    let runner = RunnerId::parse("runner").unwrap();
    store.claim(&id(1), &runner).unwrap();
    // The run prepares data for api, while web waits on something else.
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": "shop-production",
        "operations": [{
            "index": 0, "machine_id": "a".repeat(32), "service_name": "api",
            "operation": remove_web(), "status": {"type": "pending"}
        }],
        "warnings": [], "would_remove": [], "preserved_volumes": [],
        "blocked": [{
            "service": "web",
            "error": {"code": "conflict", "message": "blocked", "details": {}}
        }]
    }))
    .unwrap();
    store
        .record(&id(1), &runner, RunEvidence::Prepared(preview))
        .unwrap();
    assert!(listed(&store, &who)[0].storage_locked);
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

fn set_shared_writes(store: &ConfigStore, who: &Actor, on: bool) -> Result<bool, RpcError> {
    store
        .write(
            who,
            &SetVolumeSharedWrites {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
                shared_writes: on,
            },
        )
        .map(|set| {
            assert!(set.staged.is_empty(), "shared writes applies at once");
            set.volume.shared_writes
        })
}

fn add_api(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000007").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("api").unwrap(),
                image: Some("caddy:2".into()),
                template: None,
            },
        )
        .unwrap();
}

fn replicas(n: u8) -> Change {
    Change::Set {
        path: path("web.replicas"),
        value: json!(n),
    }
}

const NEXT: &str = "ployz volume set data --shared-writes --env production --project shop";

#[test]
fn a_second_replica_on_a_volume_is_refused_until_shared_writes_is_on() {
    let (store, who) = shop();
    assert!(!listed(&store, &who)[0].volume.shared_writes);
    let refused = edit(&store, &who, replicas(2)).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.message,
        "data is attached to web; set replicas to 1, or allow shared writes: ployz volume set data --shared-writes"
    );
    assert_eq!(refused.details["next"], NEXT);

    assert!(set_shared_writes(&store, &who, true).unwrap());
    assert!(listed(&store, &who)[0].volume.shared_writes);
    edit(&store, &who, replicas(2)).unwrap();
}

#[test]
fn a_second_service_mounting_a_volume_is_refused_until_shared_writes_is_on() {
    let (store, who) = shop();
    add_api(&store, &who);
    let mount = || Change::Set {
        path: path("api.mounts.data"),
        value: json!("/data"),
    };
    let refused = edit(&store, &who, mount()).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.message,
        "data is already mounted by web; allow shared writes first: ployz volume set data --shared-writes"
    );
    assert_eq!(refused.details["next"], NEXT);

    // A new Volume mounted into two Services at once has the same second writer.
    let both = |shared_writes| CreateVolume {
        shared_writes,
        storage_locked: false,
        restored_from: None,
        storage: VolumeKind::Docker {},
        id: VolumeId::parse("00000000-0000-4000-8000-000000000008").unwrap(),
        environment: EnvironmentRef::default(),
        name: VolumeName::parse("uploads").unwrap(),
        mounts: ["web", "api"]
            .map(|service| Mount {
                service: ServiceName::parse(service).unwrap(),
                path: "/uploads".into(),
            })
            .into(),
    };
    let refused = store.write(&who, &both(false)).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.message,
        "uploads would be mounted by web, api; add it with shared writes on"
    );
    assert_eq!(
        refused.details["next"],
        "ployz volume add uploads --mount web:/uploads --mount api:/uploads --docker --shared-writes --env production --project shop"
    );
    assert!(store.write(&who, &both(true)).unwrap().volume.shared_writes);

    set_shared_writes(&store, &who, true).unwrap();
    edit(&store, &who, mount()).unwrap();
}

#[test]
fn a_new_volume_on_a_second_replica_is_refused_with_an_add_that_works() {
    let (store, who) = shop();
    add_api(&store, &who);
    edit(
        &store,
        &who,
        Change::Set {
            path: path("api.replicas"),
            value: json!(2),
        },
    )
    .unwrap();
    let add = |shared_writes| CreateVolume {
        shared_writes,
        storage_locked: true,
        restored_from: None,
        storage: VolumeKind::Provisioned {
            maximum_bytes: ProvisionedVolumeMaximumBytes::parse_gb("1.5GB").unwrap(),
        },
        id: VolumeId::parse("00000000-0000-4000-8000-000000000008").unwrap(),
        environment: EnvironmentRef::default(),
        name: VolumeName::parse("uploads").unwrap(),
        mounts: vec![Mount {
            service: ServiceName::parse("api").unwrap(),
            path: "/my $uploads".into(),
        }],
    };
    let refused = store.write(&who, &add(false)).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.message,
        "uploads would be attached to api; set replicas to 1, or add it with shared writes on"
    );
    assert_eq!(
        refused.details["next"],
        "ployz volume add uploads --mount 'api:/my $uploads' --size 1.5GB --shared-writes --env production --project shop"
    );
    let words = shell_words::split(refused.details["next"].as_str().unwrap()).unwrap();
    assert!(words.contains(&"api:/my $uploads".to_owned()), "{words:?}");
    assert!(store.write(&who, &add(true)).unwrap().volume.shared_writes);
}

#[test]
fn a_new_volume_refused_in_a_batch_that_creates_its_service_has_no_next() {
    let (store, who) = shop();
    let batch = Batch {
        environment: EnvironmentRef::default(),
        commands: vec![
            BatchCommand::CreateService(CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000007").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("api").unwrap(),
                image: Some("caddy:2".into()),
                template: None,
            }),
            BatchCommand::Edit(Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: path("api.replicas"),
                    value: json!(2),
                }],
            }),
            BatchCommand::CreateVolume(CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                storage: VolumeKind::Docker {},
                id: VolumeId::parse("00000000-0000-4000-8000-000000000008").unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("uploads").unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("api").unwrap(),
                    path: "/uploads".into(),
                }],
            }),
        ],
        expect: None,
    };
    let refused = store.write(&who, &batch).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.message,
        "uploads would be attached to api; set replicas to 1, or add it with shared writes on"
    );
    assert!(refused.details.get("next").is_none(), "{}", refused.details);
}

#[test]
fn start_first_on_a_volume_is_refused_until_shared_writes_is_on() {
    let (store, who) = shop();
    let order = |order: &str| Change::Set {
        path: path("web.updateOrder"),
        value: json!(order),
    };
    edit(&store, &who, order("rolling")).unwrap();
    edit(&store, &who, order("stop-first")).unwrap();
    let refused = edit(&store, &who, order("start-first")).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.message,
        "web mounts data, and start-first runs two copies on it; set web.updateOrder=stop-first, or allow shared writes: ployz volume set data --shared-writes"
    );
    assert_eq!(
        refused.details["next"],
        "ployz set web.updateOrder=stop-first --env production --project shop"
    );

    set_shared_writes(&store, &who, true).unwrap();
    edit(&store, &who, order("start-first")).unwrap();
    let refused = set_shared_writes(&store, &who, false).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{}", refused.message);
}

#[test]
fn shared_writes_turns_off_only_while_one_container_writes() {
    let (store, who) = shop();
    set_shared_writes(&store, &who, true).unwrap();
    edit(&store, &who, replicas(2)).unwrap();
    let refused = set_shared_writes(&store, &who, false).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.message,
        "data is written by web; leave one Service with 1 replica before turning shared writes off"
    );
    assert_eq!(
        refused.details["next"],
        "ployz volume inspect data --env production --project shop"
    );
    assert!(listed(&store, &who)[0].volume.shared_writes);

    edit(&store, &who, replicas(1)).unwrap();
    assert!(!set_shared_writes(&store, &who, false).unwrap());
}

#[test]
fn discarding_one_setting_cannot_bring_back_a_second_writer() {
    let (store, who) = shop();
    set_shared_writes(&store, &who, true).unwrap();
    edit(&store, &who, replicas(3)).unwrap();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    edit(&store, &who, replicas(1)).unwrap();
    set_shared_writes(&store, &who, false).unwrap();
    let discard = |path: Option<&str>| Discard {
        environment: EnvironmentRef::default(),
        target: Some(ployz_store::DiscardTarget::Head),
        path: path.map(|path| SettingPath::parse(path).unwrap()),
        version: None,
    };
    let refused = store
        .write(&who, &discard(Some("web.replicas")))
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    // Returning wholly to what was saved is never refused.
    store.write(&who, &discard(None)).unwrap();
    assert!(listed(&store, &who)[0].volume.shared_writes);
}

#[test]
fn shared_writes_applies_at_once_and_is_no_diff_row() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    set_shared_writes(&store, &who, true).unwrap();
    let view = diff(&store, &who);
    assert!(view.changes.is_empty(), "{:?}", view.changes);
    assert!(view.draft_changes.is_empty(), "{:?}", view.draft_changes);
    assert_eq!((view.total_count, view.draft_count), (0, 0));

    // Discarding the whole Volume returns it to the deployed value.
    store
        .write(
            &who,
            &Discard {
                environment: EnvironmentRef::default(),
                target: Some(ployz_store::DiscardTarget::Head),
                path: Some(SettingPath::parse("volumes.data").unwrap()),
                version: None,
            },
        )
        .unwrap();
    assert!(!listed(&store, &who)[0].volume.shared_writes);
}

/// A Deploy or Save at a stale version is refused for the version. When the fresh
/// review deletes data, the refusal hands back the version that binds that loss and
/// the Volumes to accept, so one retry with both goes through.
#[test]
fn a_stale_version_hands_back_the_version_that_binds_the_fresh_loss() {
    let (store, who) = shop();
    admit(&store, &who, 1, &[], None).unwrap();
    run(&store, 1, Vec::new());
    let stale = diff(&store, &who).version;
    store
        .write(
            &who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    let bound = admit(&store, &who, 2, &[], Some(observed(&['a'])))
        .unwrap_err()
        .details["version"]
        .as_str()
        .unwrap()
        .to_owned();

    let save_at = |version: &str, accept: &[&str]| {
        store.write_trusted(
            &who,
            &Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: Some(version.to_owned()),
                message: None,
                accept_volume_loss: accept
                    .iter()
                    .map(|name| VolumeName::parse(*name).unwrap())
                    .collect(),
            },
            &Trusted {
                volumes: Some(observed(&['a'])),
                ..Trusted::default()
            },
        )
    };
    let deploy = admit_at(
        &store,
        &who,
        2,
        &["data"],
        Some(observed(&['a'])),
        Some(stale.clone()),
    )
    .unwrap_err();
    let save = save_at(&stale, &["data"]).unwrap_err();
    for refused in [deploy, save] {
        assert_eq!(refused.code, RpcErrorCode::Conflict);
        assert_eq!(refused.details["volume_loss"]["version"], json!(bound));
        assert_eq!(refused.details["volume_loss"]["accept"], json!(["data"]));
        assert_eq!(
            refused.details["volume_loss"]["volumes"][0]["name"],
            json!("data")
        );
    }

    // Without the observation that finds the data, it is the plain conflict.
    let unobserved = admit_at(&store, &who, 2, &["data"], None, Some(stale.clone())).unwrap_err();
    assert_eq!(unobserved.code, RpcErrorCode::Conflict);
    assert!(
        unobserved.details.get("volume_loss").is_none(),
        "{unobserved:?}"
    );

    admit_at(
        &store,
        &who,
        2,
        &["data"],
        Some(observed(&['a'])),
        Some(bound),
    )
    .unwrap();
}
