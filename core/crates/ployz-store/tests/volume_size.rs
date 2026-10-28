#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! A deployed Managed Volume's size: the `volume_resized` record Cloud alone makes,
//! the size its lock row projects into Head and Applied State, and the storage a
//! hand-typed edit or a replayed intent may give a deployed Volume. Through the
//! Store's interface only, on SQLite and on Postgres (see `backend`).

use ployz_core::config::VolumeKind;
use ployz_core::{
    DeployOutcome, DeployPreview, DockerVolumeName, ProvisionedVolumeMaximumBytes, RpcError,
    RpcErrorCode, ServiceName,
};
use ployz_store::{
    Actor, Admit, Change, ConfigStore, CreateProject, CreateService, CreateVolume, Deploy,
    DeploymentId, DeploymentQuery, Discard, DiscardTarget, Edit, EnvironmentId, EnvironmentRef,
    HistoryAction, HistoryPreviewQuery, Mount, NodeStatus, OrganizationId, ProjectId, ProjectName,
    Publish, RemoveVolume, Retry, Revision, RunEvidence, RunnerId, ServiceLineageId,
    SetVolumeStorage, SettingPath, StageHistory, SystemEvent, Trusted, Unheld, VolumeId,
    VolumeListing, VolumeName, VolumeObservation, VolumeResized, VolumesQuery,
};
use serde_json::{Value, json};

mod backend;
mod shop;

const ENVIRONMENT: &str = "00000000-0000-4000-8000-000000000002";
const VOLUME: &str = "00000000-0000-4000-8000-000000000005";
const CACHE: &str = "00000000-0000-4000-8000-000000000006";

fn gb(n: u64) -> ProvisionedVolumeMaximumBytes {
    (n * 1_000_000_000).try_into().unwrap()
}

fn managed(n: u64) -> VolumeKind {
    VolumeKind::Provisioned {
        maximum_bytes: gb(n),
    }
}

/// `managed(n)` as the Store serializes it.
fn storage_json(n: u64) -> Value {
    json!({ "kind": "provisioned", "maximumBytes": n * 1_000_000_000 })
}

fn org() -> OrganizationId {
    OrganizationId::parse("org").unwrap()
}

/// A store with Project `shop`, Service `web` and Volume `data` of `storage` mounted
/// at `/data`.
fn shop(storage: VolumeKind) -> (ConfigStore, Actor) {
    shop_on(backend::open(), storage)
}

fn shop_on(store: ConfigStore, storage: VolumeKind) -> (ConfigStore, Actor) {
    let who = Actor::system(org());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(ENVIRONMENT).unwrap(),
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
    create_volume(&store, &who, VOLUME, "data", storage);
    (store, who)
}

fn create_volume(store: &ConfigStore, who: &Actor, id: &str, name: &str, storage: VolumeKind) {
    create_volume_on(store, who, id, name, storage, "web");
}

/// Create Volume `name` of `storage`, mounted by Service `service` at `/name`.
fn create_volume_on(
    store: &ConfigStore,
    who: &Actor,
    id: &str,
    name: &str,
    storage: VolumeKind,
    service: &str,
) {
    store
        .write(
            who,
            &CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                storage,
                id: VolumeId::parse(id).unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse(name).unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse(service).unwrap(),
                    path: format!("/{name}"),
                }],
            },
        )
        .unwrap();
}

/// Add Service `api`, so a Deploy limited to `web` can leave a Volume out.
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

fn id(n: u8) -> DeploymentId {
    DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap()
}

fn admit(store: &ConfigStore, who: &Actor, n: u8) -> Result<(), RpcError> {
    admit_only(store, who, n, &[])
}

/// Admit Deployment `n` of `services` alone, or of everything when none are named.
fn admit_only(store: &ConfigStore, who: &Actor, n: u8, services: &[&str]) -> Result<(), RpcError> {
    store
        .write_trusted(
            who,
            &Admit::Deploy(Deploy {
                id: id(n),
                environment: EnvironmentRef::default(),
                services: services
                    .iter()
                    .map(|name| ServiceName::parse(*name).unwrap())
                    .collect(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &Trusted::default(),
        )
        .map(|_| ())
}

/// Claim Deployment `n`, record a Deploy Preview of `web`, then a success.
fn run(store: &ConfigStore, n: u8) {
    let runner = RunnerId::parse("runner").unwrap();
    store.claim(&id(n), &runner).unwrap();
    let operation = json!({
        "type": "remove_container", "machine_id": "a".repeat(32), "container_id": "a".repeat(64)
    });
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
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
}

/// Admit Deployment `n` of Working State and run it to success.
fn deploy(store: &ConfigStore, who: &Actor, n: u8) {
    admit(store, who, n).unwrap();
    run(store, n);
}

/// Save Working State as a revision of its own, as the CLI does before a Deploy.
fn publish(store: &ConfigStore, who: &Actor) {
    store
        .write_trusted(
            who,
            &Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
            &Trusted::default(),
        )
        .unwrap();
}

fn resized(volume: &str, from: u64, to: u64) -> SystemEvent {
    SystemEvent::VolumeResized(VolumeResized {
        environment: EnvironmentId::parse(ENVIRONMENT).unwrap(),
        volume: VolumeId::parse(volume).unwrap(),
        from: gb(from),
        to: gb(to),
        unheld: None,
    })
}

/// Cloud records `data` moved from `from` to `to` GB from a run that found no copy,
/// having seen `latest` as the Environment's newest numbered Deployment.
fn resize_unheld(
    store: &ConfigStore,
    from: u64,
    to: u64,
    latest: Option<u64>,
) -> Result<(), RpcError> {
    let SystemEvent::VolumeResized(mut resized) = resized(VOLUME, from, to) else {
        unreachable!()
    };
    resized.unheld = Some(Unheld {
        latest_deployment: latest,
    });
    store
        .system(
            &org(),
            &SystemEvent::VolumeResized(resized),
            &Trusted::default(),
        )
        .map(|_| ())
}

/// Cloud records `data` moved from `from` to `to` GB.
fn resize(store: &ConfigStore, from: u64, to: u64) -> Result<(), RpcError> {
    resize_volume(store, VOLUME, from, to)
}

fn resize_volume(store: &ConfigStore, volume: &str, from: u64, to: u64) -> Result<(), RpcError> {
    store
        .system(&org(), &resized(volume, from, to), &Trusted::default())
        .map(|_| ())
}

fn listing(store: &ConfigStore, who: &Actor, name: &str) -> VolumeListing {
    store
        .read(who, &VolumesQuery::default())
        .unwrap()
        .volumes
        .into_iter()
        .find(|listing| listing.volume.name.as_str() == name)
        .unwrap()
}

/// The diff row of `data`'s storage, Head before Working after; none when they agree.
fn storage_row(store: &ConfigStore, who: &Actor) -> Option<(Value, Value)> {
    store
        .read(who, &ployz_store::DiffQuery::default())
        .unwrap()
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|setting| setting.path == "volumes.data.storage")
        .map(|setting| (setting.before.clone(), setting.after.clone()))
}

/// The storage row a Save would record, against the latest Saved revision.
fn draft_storage_row(store: &ConfigStore, who: &Actor) -> Option<(Value, Value)> {
    store
        .read(who, &ployz_store::DiffQuery::default())
        .unwrap()
        .draft_changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|setting| setting.path == "volumes.data.storage")
        .map(|setting| (setting.before.clone(), setting.after.clone()))
}

fn set_storage(
    store: &ConfigStore,
    who: &Actor,
    name: &str,
    storage: VolumeKind,
) -> Result<(), RpcError> {
    store
        .write(
            who,
            &SetVolumeStorage {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse(name).unwrap(),
                storage,
            },
        )
        .map(|_| ())
}

fn set(store: &ConfigStore, who: &Actor, path: &str, value: Value) {
    store
        .write(
            who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse(path).unwrap(),
                    value,
                }],
            },
        )
        .unwrap();
}

/// The revision Deployment `n` deployed.
fn revision_of(store: &ConfigStore, who: &Actor, n: u8) -> Revision {
    store
        .read(who, &DeploymentQuery { id: id(n) })
        .unwrap()
        .deployment
        .saved
}

/// Stage a History restore of `revision` into Working State.
fn restore(store: &ConfigStore, who: &Actor, revision: Revision) -> Result<(), RpcError> {
    let preview = store.read(
        who,
        &HistoryPreviewQuery {
            environment: EnvironmentRef::default(),
            revision,
            action: HistoryAction::Restore,
            change: None,
        },
    )?;
    store
        .write(
            who,
            &StageHistory {
                environment: EnvironmentRef::default(),
                revision,
                action: HistoryAction::Restore,
                change: None,
                version: preview.version,
                accept_overwrite: true,
            },
        )
        .map(|_| ())
}

/// Redeploy Deployment `source` as Deployment `n`, rolling back what came after it.
fn redeploy(store: &ConfigStore, who: &Actor, source: u8, n: u8) -> Result<(), RpcError> {
    let rolls_back = store
        .read(who, &DeploymentQuery { id: id(source) })
        .unwrap()
        .rolls_back;
    store
        .write_trusted(
            who,
            &Admit::Retry(Retry {
                id: id(n),
                deployment: id(source),
                rolls_back: Some(rolls_back),
                version: None,
                accept_volume_loss: Vec::new(),
            }),
            &Trusted::default(),
        )
        .map(|_| ())
}

#[test]
fn a_record_moves_the_lock_working_state_and_applied_state_at_once() {
    let (store, who) = shop(managed(10));
    assert_eq!(listing(&store, &who, "data").recorded_storage, None);
    deploy(&store, &who, 1);
    let listed = listing(&store, &who, "data");
    assert_eq!(listed.recorded_storage, Some(gb(10)));
    assert!(listed.storage_locked);

    resize(&store, 10, 20).unwrap();
    let listed = listing(&store, &who, "data");
    assert_eq!(listed.volume.storage, managed(20));
    assert_eq!(listed.recorded_storage, Some(gb(20)));
    assert!(listed.deployed);
    assert_eq!(storage_row(&store, &who), None);
    // The Saved revision #1 deployed still asks 10 GB: restoring it stages 10 GB
    // against a Head at 20 GB.
    restore(&store, &who, revision_of(&store, &who, 1)).unwrap();
    assert_eq!(
        storage_row(&store, &who),
        Some((storage_json(20), storage_json(10)))
    );
}

#[test]
fn a_record_is_no_draft_against_the_saved_revision_it_outgrew() {
    let (store, who) = shop(managed(10));
    publish(&store, &who);
    deploy(&store, &who, 1);
    resize(&store, 10, 20).unwrap();
    let view = store
        .read(&who, &ployz_store::DiffQuery::default())
        .unwrap();
    assert_eq!(draft_storage_row(&store, &who), None);
    assert_eq!(view.draft_count, 0);
    assert_eq!(storage_row(&store, &who), None);

    restore(&store, &who, revision_of(&store, &who, 1)).unwrap();
    assert_eq!(
        draft_storage_row(&store, &who),
        Some((storage_json(20), storage_json(10)))
    );
}

/// Discard every draft back to `target`.
fn discard(store: &ConfigStore, who: &Actor, target: DiscardTarget) {
    store
        .write(
            who,
            &Discard {
                environment: EnvironmentRef::default(),
                target: Some(target),
                path: None,
                version: None,
            },
        )
        .unwrap();
}

#[test]
fn a_discard_after_a_record_keeps_the_recorded_size() {
    for target in [
        DiscardTarget::Saved,
        DiscardTarget::Review,
        DiscardTarget::Head,
    ] {
        let (store, who) = shop(managed(10));
        publish(&store, &who);
        deploy(&store, &who, 1);
        resize(&store, 10, 20).unwrap();
        set(&store, &who, "web.image", json!("postgres:18"));

        discard(&store, &who, target);

        let view = store
            .read(&who, &ployz_store::DiffQuery::default())
            .unwrap();
        assert_eq!(
            listing(&store, &who, "data").volume.storage,
            managed(20),
            "{target:?}"
        );
        assert_eq!(draft_storage_row(&store, &who), None, "{target:?}");
        assert_eq!(view.draft_count, 0, "{target:?}");
        assert_eq!(view.total_count, 0, "{target:?}");
    }
}

#[test]
fn a_save_or_deploy_after_a_record_creates_no_revision() {
    let (store, who) = shop(managed(10));
    publish(&store, &who);
    deploy(&store, &who, 1);
    resize(&store, 10, 20).unwrap();

    let published = store
        .write_trusted(
            &who,
            &Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
            &Trusted::default(),
        )
        .unwrap();
    assert!(!published.created);
    assert_eq!(published.saved, Some(revision_of(&store, &who, 1)));

    deploy(&store, &who, 2);
    assert_eq!(revision_of(&store, &who, 2), revision_of(&store, &who, 1));
    assert_eq!(listing(&store, &who, "data").volume.storage, managed(20));
}

#[test]
fn a_restore_after_a_record_overwrites_nothing() {
    let (store, who) = shop(managed(10));
    publish(&store, &who);
    deploy(&store, &who, 1);
    resize(&store, 10, 20).unwrap();

    let revision = revision_of(&store, &who, 1);
    let preview = store
        .read(
            &who,
            &HistoryPreviewQuery {
                environment: EnvironmentRef::default(),
                revision,
                action: HistoryAction::Restore,
                change: None,
            },
        )
        .unwrap();
    assert!(preview.overwritten.is_empty(), "{:?}", preview.overwritten);
    store
        .write(
            &who,
            &StageHistory {
                environment: EnvironmentRef::default(),
                revision,
                action: HistoryAction::Restore,
                change: None,
                version: preview.version,
                accept_overwrite: false,
            },
        )
        .unwrap();
    assert_eq!(
        draft_storage_row(&store, &who),
        Some((storage_json(20), storage_json(10)))
    );
}

#[test]
fn a_partial_deploy_after_a_record_needs_no_volume_evidence() {
    let (store, who) = shop(managed(10));
    add_api(&store, &who);
    create_volume_on(
        &store,
        &who,
        "00000000-0000-4000-8000-000000000009",
        "cache",
        managed(5),
        "api",
    );
    publish(&store, &who);
    deploy(&store, &who, 1);
    store
        .write(
            &who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("cache").unwrap(),
            },
        )
        .unwrap();
    store
        .write_trusted(
            &who,
            &Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
            &Trusted {
                volumes: Some(VolumeObservation {
                    sought: vec![
                        DockerVolumeName::parse(
                            "shop-production_vol-00000000-0000-4000-8000-000000000009",
                        )
                        .unwrap(),
                    ],
                    held: Vec::new(),
                    unanswered: Vec::new(),
                    failure: None,
                }),
                ..Trusted::default()
            },
        )
        .unwrap();
    resize(&store, 10, 20).unwrap();

    admit_only(&store, &who, 2, &["web"]).unwrap();
}

#[test]
fn the_held_size_typed_over_a_staged_restore_clears_it() {
    let (store, who) = shop(managed(10));
    publish(&store, &who);
    deploy(&store, &who, 1);
    resize(&store, 10, 20).unwrap();
    restore(&store, &who, revision_of(&store, &who, 1)).unwrap();
    assert_eq!(
        draft_storage_row(&store, &who),
        Some((storage_json(20), storage_json(10)))
    );

    set_storage(&store, &who, "data", managed(20)).unwrap();

    assert_eq!(draft_storage_row(&store, &who), None);
    assert_eq!(listing(&store, &who, "data").volume.storage, managed(20));
}

#[test]
fn a_replayed_record_writes_nothing() {
    let (store, who) = shop(managed(10));
    deploy(&store, &who, 1);
    resize(&store, 10, 20).unwrap();
    restore(&store, &who, revision_of(&store, &who, 1)).unwrap();
    resize(&store, 10, 20).unwrap();
    assert_eq!(listing(&store, &who, "data").volume.storage, managed(10));
    assert_eq!(
        storage_row(&store, &who),
        Some((storage_json(20), storage_json(10)))
    );
}

#[test]
fn a_record_from_a_size_the_lock_left_is_refused() {
    let (store, who) = shop(managed(10));
    deploy(&store, &who, 1);
    resize(&store, 10, 20).unwrap();
    let refused = resize(&store, 10, 30).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.details,
        json!({ "reason": "size_changed", "volume": VOLUME, "recorded": storage_json(20) })
    );
    assert_eq!(
        refused.message,
        "data is recorded at 20 GB, not 10 GB; read its size again."
    );
    assert_eq!(listing(&store, &who, "data").recorded_storage, Some(gb(20)));
}

#[test]
fn a_record_moves_a_size_either_way() {
    let (store, who) = shop(managed(10));
    deploy(&store, &who, 1);
    resize(&store, 10, 20).unwrap();
    resize(&store, 20, 10).unwrap();
    let listed = listing(&store, &who, "data");
    assert_eq!(listed.volume.storage, managed(10));
    assert_eq!(listed.recorded_storage, Some(gb(10)));
    assert_eq!(storage_row(&store, &who), None);

    resize(&store, 10, 20).unwrap();
    let listed = listing(&store, &who, "data");
    assert_eq!(listed.volume.storage, managed(20));
    assert_eq!(listed.recorded_storage, Some(gb(20)));
    assert_eq!(storage_row(&store, &who), None);
    // Only the lock gates a record: a second 20 -> 10 after the lock came back to
    // 20 is a fresh compare-and-set that matches, not a replay the lock moved past;
    // once it moved, the same record again is the replay that writes nothing, and a
    // record from a size the lock left is refused.
    resize(&store, 20, 10).unwrap();
    assert_eq!(listing(&store, &who, "data").recorded_storage, Some(gb(10)));
    resize(&store, 20, 10).unwrap();
    let refused = resize(&store, 20, 30).unwrap_err();
    assert_eq!(refused.details["reason"], "size_changed");
    assert_eq!(refused.details["recorded"], storage_json(10));
    assert_eq!(listing(&store, &who, "data").recorded_storage, Some(gb(10)));
}

#[test]
fn a_staged_restore_is_left_alone_both_ways() {
    let (store, who) = shop(managed(10));
    deploy(&store, &who, 1);
    resize(&store, 10, 20).unwrap();
    resize(&store, 20, 30).unwrap();
    restore(&store, &who, revision_of(&store, &who, 1)).unwrap();
    assert_eq!(listing(&store, &who, "data").volume.storage, managed(10));

    resize(&store, 30, 20).unwrap();
    let listed = listing(&store, &who, "data");
    assert_eq!(listed.volume.storage, managed(10));
    assert_eq!(listed.recorded_storage, Some(gb(20)));
    assert_eq!(
        storage_row(&store, &who),
        Some((storage_json(20), storage_json(10)))
    );
    resize(&store, 20, 30).unwrap();
    let listed = listing(&store, &who, "data");
    assert_eq!(listed.volume.storage, managed(10));
    assert_eq!(listed.recorded_storage, Some(gb(30)));
}

#[test]
fn applied_state_takes_the_lock_whatever_size_it_held() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_on(
        ConfigStore::open(&url, backend::key()).unwrap(),
        managed(10),
    );
    deploy(&store, &who, 1);
    backend::execute(
        &url,
        &format!(
            "UPDATE config_applied SET node = replace(node, '10000000000', '5000000000') \
             WHERE node_id = '{VOLUME}'"
        ),
    );
    assert_eq!(
        storage_row(&store, &who),
        Some((storage_json(5), storage_json(10)))
    );
    resize(&store, 10, 20).unwrap();
    assert_eq!(storage_row(&store, &who), None);
    assert_eq!(listing(&store, &who, "data").volume.storage, managed(20));
}

#[test]
fn a_volume_without_fixed_storage_has_no_size_to_record() {
    let (store, who) = shop(managed(10));
    assert_eq!(
        resize(&store, 10, 20).unwrap_err().code,
        RpcErrorCode::NotFound
    );
    deploy(&store, &who, 1);
    let unknown = store
        .system(&org(), &resized(CACHE, 10, 20), &Trusted::default())
        .unwrap_err();
    assert_eq!(unknown.code, RpcErrorCode::NotFound);
    assert_eq!(unknown.details, json!({ "volume": CACHE }));
    assert_eq!(listing(&store, &who, "data").recorded_storage, Some(gb(10)));
}

#[test]
fn a_docker_volume_has_no_size_to_record_or_project() {
    let (store, who) = shop(VolumeKind::Docker {});
    deploy(&store, &who, 1);
    let listed = listing(&store, &who, "data");
    assert!(listed.storage_locked);
    assert_eq!(listed.recorded_storage, None);
    let refused = resize(&store, 10, 20).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.details,
        json!({ "reason": "size_changed", "volume": VOLUME, "recorded": { "kind": "docker" } })
    );
    assert_eq!(
        listing(&store, &who, "data").volume.storage,
        VolumeKind::Docker {}
    );
    assert_eq!(storage_row(&store, &who), None);
}

#[test]
fn head_and_applied_state_read_the_lock_for_a_redeployed_older_revision() {
    let (store, who) = shop(managed(10));
    deploy(&store, &who, 1);
    set(&store, &who, "web.image", json!("postgres:18"));
    deploy(&store, &who, 2);
    resize(&store, 10, 20).unwrap();

    // Revision #1 asks 10 GB; in flight, Head shows the 20 GB the lock holds.
    redeploy(&store, &who, 1, 3).unwrap();
    assert_eq!(storage_row(&store, &who), None);
    run(&store, 3);
    let listed = listing(&store, &who, "data");
    assert!(listed.deployed);
    assert_eq!(listed.volume.storage, managed(20));
    assert_eq!(listed.recorded_storage, Some(gb(20)));
    assert_eq!(storage_row(&store, &who), None);
}

#[test]
fn a_hold_made_before_a_record_confirms_at_the_recorded_size() {
    let (store, who) = shop::shop();
    create_volume(&store, &who, VOLUME, "data", managed(10));
    backend::deploy(&store, &who, "production", 1);
    shop::set(&store, &who, "web.waitForCi", json!(true));
    shop::publish(&store, &who);
    shop::system(&store, &shop::push(None, Some(shop::H2), None));
    let [hold] = <[_; 1]>::try_from(shop::held(&store, &who)).unwrap();

    resize(&store, 10, 20).unwrap();
    shop::system(
        &store,
        &shop::suite(1, shop::H2, "completed", Some("success"), 1),
    );
    assert!(shop::held(&store, &who).is_empty());
    backend::run(&store, &hold.id);
    let listed = listing(&store, &who, "data");
    assert!(listed.deployed);
    assert_eq!(listed.volume.storage, managed(20));
    assert_eq!(listed.recorded_storage, Some(gb(20)));
    assert_eq!(storage_row(&store, &who), None);
}

#[test]
fn a_hand_typed_size_for_a_deployed_volume_points_at_volume_set() {
    let (store, who) = shop(managed(10));
    deploy(&store, &who, 1);
    for (asked, message) in [
        (
            managed(20),
            "data is deployed at 10 GB; grow it with ployz volume set data --size 20",
        ),
        (
            managed(5),
            "data is deployed at 10 GB; shrink it with ployz volume set data --size 5",
        ),
    ] {
        let refused = set_storage(&store, &who, "data", asked).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Conflict);
        assert_eq!(refused.message, message);
        assert_eq!(
            refused.details,
            json!({ "volume": "data", "storage_locked": true, "recorded": 10_000_000_000_u64 })
        );
    }
    let refused = set_storage(&store, &who, "data", VolumeKind::Docker {}).unwrap_err();
    assert_eq!(
        refused.message,
        "Volume data keeps its storage once a Deploy has been requested; add a new Volume for \
         other storage"
    );
    assert_eq!(
        refused.details,
        json!({ "volume": "data", "storage_locked": true })
    );
    assert_eq!(listing(&store, &who, "data").volume.storage, managed(10));
}

#[test]
fn a_replayed_intent_fits_at_or_below_the_lock() {
    let (store, who) = shop(managed(10));
    deploy(&store, &who, 1);
    resize(&store, 10, 20).unwrap();
    // Restoring revision #1 asks 10 GB of a Volume held at 20 GB: it stages, deploys,
    // and Applied State still reads the lock.
    restore(&store, &who, revision_of(&store, &who, 1)).unwrap();
    deploy(&store, &who, 2);
    let listed = listing(&store, &who, "data");
    assert_eq!(listed.volume.storage, managed(10));
    assert_eq!(listed.recorded_storage, Some(gb(20)));
    assert_eq!(
        storage_row(&store, &who),
        Some((storage_json(20), storage_json(10)))
    );
}

/// Every Managed Volume size Deployment `n`'s Deploy Intent asks its Servers to hold,
/// in GB.
fn shipped_sizes(store: &ConfigStore, n: u8) -> Vec<u64> {
    fn collect(value: &Value, sizes: &mut Vec<u64>) {
        match value {
            Value::Object(fields) => {
                for (key, field) in fields {
                    match field.as_u64() {
                        Some(bytes) if key == "maximum_bytes" || key == "maximumBytes" => {
                            sizes.push(bytes / 1_000_000_000);
                        }
                        _ => collect(field, sizes),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect(item, sizes)),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    let claimed = store
        .claim(&id(n), &RunnerId::parse("runner").unwrap())
        .unwrap();
    let mut sizes = Vec::new();
    collect(&serde_json::to_value(&claimed.intent).unwrap(), &mut sizes);
    sizes
}

/// Deployment `n` ships `data` at `size` GB, runs, and leaves the lock and Applied
/// State there: a Deploy grows or shrinks nothing.
fn deploys_at(store: &ConfigStore, who: &Actor, n: u8, size: u64) {
    let shipped = shipped_sizes(store, n);
    assert!(!shipped.is_empty(), "Deployment {n} ships a Managed Volume");
    assert!(
        shipped.iter().all(|&shipped| shipped == size),
        "{shipped:?}"
    );
    run(store, n);
    let listed = listing(store, who, "data");
    assert_eq!(listed.volume.storage, managed(size));
    assert_eq!(listed.recorded_storage, Some(gb(size)));
    assert_eq!(storage_row(store, who), None);
}

#[test]
fn a_replayed_intent_above_the_lock_deploys_at_the_held_size() {
    let (store, who) = shop(managed(20));
    deploy(&store, &who, 1);
    resize(&store, 20, 10).unwrap();
    redeploy(&store, &who, 1, 2).unwrap();
    deploys_at(&store, &who, 2, 10);

    restore(&store, &who, revision_of(&store, &who, 1)).unwrap();
    assert_eq!(
        storage_row(&store, &who),
        Some((storage_json(10), storage_json(20)))
    );
    admit(&store, &who, 3).unwrap();
    let shipped = shipped_sizes(&store, 3);
    assert_eq!(shipped.iter().max(), Some(&10));
    run(&store, 3);
    assert_eq!(listing(&store, &who, "data").recorded_storage, Some(gb(10)));
}

#[test]
fn a_revision_saved_before_a_shrink_deploys_at_the_held_size() {
    let (store, who) = shop(managed(5));
    deploy(&store, &who, 1);
    set(&store, &who, "web.image", json!("postgres:18"));
    publish(&store, &who);
    resize(&store, 5, 3).unwrap();
    admit(&store, &who, 2).unwrap();
    deploys_at(&store, &who, 2, 3);
}

#[test]
fn a_deploy_with_nothing_staged_after_a_shrink_ships_at_the_held_size() {
    let (store, who) = shop(managed(5));
    publish(&store, &who);
    deploy(&store, &who, 1);
    resize(&store, 5, 3).unwrap();
    admit(&store, &who, 2).unwrap();
    assert_eq!(revision_of(&store, &who, 2), revision_of(&store, &who, 1));
    deploys_at(&store, &who, 2, 3);
}

/// Deployment `n` runs with nothing to do, an empty preview then success, and
/// settles every Node Outcome.
fn runs_nothing(store: &ConfigStore, who: &Actor, n: u8) -> Vec<(String, NodeStatus)> {
    let runner = RunnerId::parse("runner").unwrap();
    store.claim(&id(n), &runner).unwrap();
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": "shop-production", "operations": [], "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(&id(n), &runner, RunEvidence::Prepared(preview))
        .unwrap();
    let outcome: DeployOutcome<ployz_core::ExecutionError> =
        serde_json::from_value(json!({ "type": "success", "completed": [] })).unwrap();
    store
        .record(
            &id(n),
            &runner,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
    store
        .read(who, &DeploymentQuery { id: id(n) })
        .unwrap()
        .nodes
        .into_iter()
        .map(|node| (node.node.name().to_owned(), node.outcome))
        .collect()
}

fn unchanged() -> Vec<(String, NodeStatus)> {
    vec![
        ("web".to_owned(), NodeStatus::Unchanged),
        ("data".to_owned(), NodeStatus::Unchanged),
    ]
}

#[test]
fn a_plain_deploy_after_a_shrink_settles_the_volume_unchanged() {
    let (store, who) = shop(managed(5));
    publish(&store, &who);
    deploy(&store, &who, 1);
    resize(&store, 5, 3).unwrap();
    admit(&store, &who, 2).unwrap();
    assert_eq!(runs_nothing(&store, &who, 2), unchanged());
}

#[test]
fn a_plain_deploy_after_a_grow_settles_the_volume_unchanged() {
    let (store, who) = shop(managed(5));
    publish(&store, &who);
    deploy(&store, &who, 1);
    resize(&store, 5, 8).unwrap();
    admit(&store, &who, 2).unwrap();
    assert_eq!(runs_nothing(&store, &who, 2), unchanged());
}

/// Assert `refused` is the record refused while Deployment `n` is in flight.
fn refused_while_deploying(refused: &RpcError, n: u8) {
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.details,
        json!({
            "reason": "deploying",
            "volume": VOLUME,
            "deployment": { "id": id(n), "number": n },
        })
    );
    assert_eq!(
        refused.message,
        format!(
            "Deployment #{n} is in flight; shrink data when it ends, or end it now with ployz \
             deployment cancel {n}."
        )
    );
}

#[test]
fn a_shrink_is_not_recorded_while_a_deployment_is_in_flight() {
    let (store, who) = shop(managed(5));
    publish(&store, &who);
    deploy(&store, &who, 1);
    admit(&store, &who, 2).unwrap();
    // Queued, then claimed: both may still run, so both hold the size.
    refused_while_deploying(&resize(&store, 5, 3).unwrap_err(), 2);
    assert_eq!(shipped_sizes(&store, 2), vec![5]);
    refused_while_deploying(&resize(&store, 5, 3).unwrap_err(), 2);
    // The refusal commits nothing: the lock row, Applied State and Working State all
    // still say 5, so Cloud's change log, read from Applied State, has no resize.
    let listed = listing(&store, &who, "data");
    assert_eq!(listed.recorded_storage, Some(gb(5)));
    assert_eq!(listed.volume.storage, managed(5));
    assert_eq!(storage_row(&store, &who), None);
    assert_eq!(runs_nothing(&store, &who, 2), unchanged());
    resize(&store, 5, 3).unwrap();
    assert_eq!(listing(&store, &who, "data").recorded_storage, Some(gb(3)));
}

#[test]
fn a_shrink_recorded_before_an_admission_is_the_size_its_claim_reads() {
    let (store, who) = shop(managed(5));
    publish(&store, &who);
    deploy(&store, &who, 1);
    resize(&store, 5, 3).unwrap();
    admit(&store, &who, 2).unwrap();
    deploys_at(&store, &who, 2, 3);
}

#[test]
fn a_record_that_raises_or_replays_is_taken_while_a_deployment_is_in_flight() {
    let (store, who) = shop(managed(5));
    publish(&store, &who);
    deploy(&store, &who, 1);
    resize(&store, 5, 3).unwrap();
    admit(&store, &who, 2).unwrap();
    // The shrink's own record again is a replay; its undo and a grow raise the size.
    resize(&store, 5, 3).unwrap();
    resize(&store, 3, 5).unwrap();
    resize(&store, 5, 8).unwrap();
    assert_eq!(listing(&store, &who, "data").recorded_storage, Some(gb(8)));
    refused_while_deploying(&resize(&store, 8, 5).unwrap_err(), 2);
}

/// The refusal of a record from a run that found no copy, since Deployment `n` may
/// have created it.
fn refused_as_deployed_since(refused: &RpcError, n: u8) {
    assert_eq!(refused.code, RpcErrorCode::Conflict, "{refused:?}");
    assert_eq!(refused.details["reason"], json!("deploying"));
    assert_eq!(refused.details["unheld"], json!(true));
    assert_eq!(refused.details["deployment"]["id"], json!(id(n)));
}

/// #3501: a run sees Deployment 1 as the latest and finds no copy. Deployment 2 is
/// then admitted, creates the copy at the old size and ends before the run records.
/// Nothing is in flight at the record, yet it refuses; a run that looks again sees
/// the copy and resizes it, so its record is an ordinary one.
#[test]
fn a_record_from_a_run_that_found_no_copy_refuses_once_a_deployment_came_since() {
    let (store, who) = shop(managed(10));
    publish(&store, &who);
    deploy(&store, &who, 1);
    let seen = Some(1);
    deploy(&store, &who, 2);
    refused_as_deployed_since(&resize_unheld(&store, 10, 20, seen).unwrap_err(), 2);
    assert_eq!(listing(&store, &who, "data").recorded_storage, Some(gb(10)));
    // Looking again, the run finds the copy Deployment 2 created and grows it.
    resize(&store, 10, 20).unwrap();
    assert_eq!(listing(&store, &who, "data").recorded_storage, Some(gb(20)));
}

#[test]
fn a_record_from_a_run_that_found_no_copy_waits_for_a_deployment_in_flight_either_way() {
    let (store, who) = shop(managed(10));
    publish(&store, &who);
    deploy(&store, &who, 1);
    admit(&store, &who, 2).unwrap();
    // Admitted before the run looked, so the run saw it; it is in flight all the same.
    refused_as_deployed_since(&resize_unheld(&store, 10, 20, Some(2)).unwrap_err(), 2);
    refused_as_deployed_since(&resize_unheld(&store, 10, 5, Some(2)).unwrap_err(), 2);
    run(&store, 2);
    resize_unheld(&store, 10, 20, Some(2)).unwrap();
    assert_eq!(listing(&store, &who, "data").recorded_storage, Some(gb(20)));
    // The next Deploy claims the Volume at the recorded size.
    admit(&store, &who, 3).unwrap();
    deploys_at(&store, &who, 3, 20);
}

/// A runner whose lease lapsed may still have a `PrepareVolumes` on the wire, so its
/// Deployment counts though it no longer reads as in flight.
#[test]
fn a_record_from_a_run_that_found_no_copy_waits_for_a_deployment_whose_runner_lapsed() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_on(
        ConfigStore::open(&url, backend::key()).unwrap(),
        managed(10),
    );
    publish(&store, &who);
    deploy(&store, &who, 1);
    admit(&store, &who, 2).unwrap();
    store
        .claim(&id(2), &RunnerId::parse("runner").unwrap())
        .unwrap();
    backend::execute(&url, "UPDATE config_deployment SET lease = 0");
    refused_as_deployed_since(&resize_unheld(&store, 10, 20, Some(2)).unwrap_err(), 2);
    assert_eq!(listing(&store, &who, "data").recorded_storage, Some(gb(10)));
}

#[test]
fn a_record_from_a_run_that_found_no_copy_moves_the_size_with_no_deployment_since() {
    let (store, who) = shop(managed(10));
    publish(&store, &who);
    deploy(&store, &who, 1);
    resize_unheld(&store, 10, 4, Some(1)).unwrap();
    let listed = listing(&store, &who, "data");
    assert_eq!(listed.recorded_storage, Some(gb(4)));
    assert_eq!(listed.volume.storage, managed(4));
    // A replay of the same record answers ok whatever came since.
    deploy(&store, &who, 2);
    resize_unheld(&store, 10, 4, Some(1)).unwrap();
}

#[test]
fn a_redeploy_leaves_a_volume_it_never_shipped_out_of_the_check() {
    let (store, who) = shop(managed(10));
    add_api(&store, &who);
    create_volume_on(&store, &who, CACHE, "cache", managed(20), "api");
    // Deployment 1 ships web and data; its revision holds cache at 20 GB unshipped.
    admit_only(&store, &who, 1, &["web"]).unwrap();
    run(&store, 1);
    deploy(&store, &who, 2);
    resize_volume(&store, CACHE, 20, 10).unwrap();
    redeploy(&store, &who, 1, 3).unwrap();
    run(&store, 3);
    assert_eq!(listing(&store, &who, "cache").volume.storage, managed(10));
    assert_eq!(listing(&store, &who, "data").volume.storage, managed(10));
}

#[test]
fn a_staged_restore_of_one_volume_does_not_block_an_edit_of_another() {
    let (store, who) = shop(managed(10));
    deploy(&store, &who, 1);
    resize(&store, 10, 20).unwrap();
    restore(&store, &who, revision_of(&store, &who, 1)).unwrap();
    create_volume(
        &store,
        &who,
        CACHE,
        "cache",
        VolumeKind::provisioned_default(),
    );
    set_storage(&store, &who, "cache", managed(7)).unwrap();
    assert_eq!(listing(&store, &who, "cache").volume.storage, managed(7));
    assert_eq!(listing(&store, &who, "data").volume.storage, managed(10));
}

#[test]
fn the_record_is_a_system_event_by_event_name() {
    let event: SystemEvent = serde_json::from_value(json!({
        "event": "volume_resized",
        "environment": ENVIRONMENT,
        "volume": VOLUME,
        "from": 10_000_000_000_u64,
        "to": 20_000_000_000_u64,
    }))
    .unwrap();
    assert_eq!(event, resized(VOLUME, 10, 20));
    assert_eq!(
        serde_json::to_value(&event).unwrap()["event"],
        "volume_resized"
    );
}

#[test]
fn a_deploy_whose_preparation_failed_leaves_the_size_editable() {
    let (store, who) = shop(managed(10));
    admit(&store, &who, 1).unwrap();
    let runner = RunnerId::parse("runner").unwrap();
    store.claim(&id(1), &runner).unwrap();
    store
        .record(
            &id(1),
            &runner,
            RunEvidence::NotExecuted("Not enough disk space".to_owned().into()),
        )
        .unwrap();

    assert!(!listing(&store, &who, "data").storage_locked);
    set_storage(&store, &who, "data", managed(5)).unwrap();
    assert_eq!(listing(&store, &who, "data").volume.storage, managed(5));
}

#[test]
fn an_unmounted_volume_a_deploy_never_prepares_stays_editable() {
    let (store, who) = shop(managed(10));
    store
        .write(
            &who,
            &CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                storage: managed(999_999),
                id: VolumeId::parse(CACHE).unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("cache").unwrap(),
                mounts: Vec::new(),
            },
        )
        .unwrap();
    deploy(&store, &who, 1);

    assert!(listing(&store, &who, "data").storage_locked);
    assert!(!listing(&store, &who, "cache").storage_locked);
    set_storage(&store, &who, "cache", managed(7)).unwrap();
    assert_eq!(listing(&store, &who, "cache").volume.storage, managed(7));
}
