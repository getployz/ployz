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
    DeployOutcome, DeployPreview, ProvisionedVolumeMaximumBytes, RpcError, RpcErrorCode,
    ServiceName,
};
use ployz_store::{
    Actor, Admit, Change, ConfigStore, CreateProject, CreateService, CreateVolume, Deploy,
    DeploymentId, DeploymentQuery, Edit, EnvironmentId, EnvironmentRef, HistoryAction,
    HistoryPreviewQuery, Mount, OrganizationId, ProjectId, ProjectName, Retry, Revision,
    RunEvidence, RunnerId, ServiceLineageId, SetVolumeStorage, SettingPath, StageHistory,
    SystemEvent, Trusted, VolumeId, VolumeListing, VolumeName, VolumeResized, VolumesQuery,
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
                    service: ServiceName::parse("web").unwrap(),
                    path: format!("/{name}"),
                }],
            },
        )
        .unwrap();
}

fn id(n: u8) -> DeploymentId {
    DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap()
}

fn admit(store: &ConfigStore, who: &Actor, n: u8) -> Result<(), RpcError> {
    store
        .write_trusted(
            who,
            &Admit::Deploy(Deploy {
                id: id(n),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
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

fn resized(volume: &str, from: u64, to: u64) -> SystemEvent {
    SystemEvent::VolumeResized(VolumeResized {
        environment: EnvironmentId::parse(ENVIRONMENT).unwrap(),
        volume: VolumeId::parse(volume).unwrap(),
        from: gb(from),
        to: gb(to),
    })
}

/// Cloud records `data` moved from `from` to `to` GB.
fn resize(store: &ConfigStore, from: u64, to: u64) -> Result<(), RpcError> {
    store
        .system(&org(), &resized(VOLUME, from, to), &Trusted::default())
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

#[test]
fn a_replayed_intent_above_the_lock_is_refused() {
    let (store, who) = shop(managed(20));
    deploy(&store, &who, 1);
    resize(&store, 20, 10).unwrap();
    let message = "This revision asks 20 GB for data, which is deployed at 10 GB. Grow it first \
                   with ployz volume set data --size 20";
    let details = json!({
        "volume": "data", "storage_locked": true,
        "recorded": 10_000_000_000_u64, "asked": 20_000_000_000_u64
    });
    let refused = restore(&store, &who, revision_of(&store, &who, 1)).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(refused.message, message);
    assert_eq!(refused.details, details);
    let refused = redeploy(&store, &who, 1, 2).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(refused.message, message);
    assert_eq!(refused.details, details);
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
