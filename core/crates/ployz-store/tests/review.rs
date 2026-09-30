//! Review, Publish and Discard through `read` and `write` only, on SQLite and on
//! Postgres (see `backend`).

use ployz_core::config::{ReviewComparisonRole, ReviewLifecycleKind};
use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Change, ConfigStore, CreateProject, CreateService, CreateVolume, DiffQuery, DiffView,
    Discard, Discarded, Edit, EnvironmentId, EnvironmentQuery, EnvironmentRef, Mount,
    OrganizationId, ProjectId, ProjectName, Publish, Published, Revision, ServiceLineageId,
    SettingPath, Trusted, VolumeId, VolumeName,
};
use serde_json::{Value, json};

mod backend;

const PROJECT: &str = "00000000-0000-4000-8000-000000000001";
const ENVIRONMENT: &str = "00000000-0000-4000-8000-000000000002";

/// A store with Project `shop` and new Services `web` (nginx) and `api` (caddy).
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(PROJECT).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(ENVIRONMENT).unwrap(),
            },
        )
        .unwrap();
    for (n, name, image) in [(3, "web", "nginx:1"), (4, "api", "caddy:2")] {
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-00000000000{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some(image.into()),
                },
            )
            .unwrap();
    }
    (store, who)
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

fn diff(store: &ConfigStore, who: &Actor) -> DiffView {
    store.read(who, &DiffQuery::default()).unwrap()
}

/// Every Setting value in Working State, by path.
fn working(store: &ConfigStore, who: &Actor) -> Vec<(String, Value)> {
    let query = EnvironmentQuery {
        all: true,
        ..EnvironmentQuery::default()
    };
    let view = store.read(who, &query).unwrap();
    view.settings
        .into_iter()
        .map(|row| (row.path.to_string(), row.value))
        .collect()
}

fn value(store: &ConfigStore, who: &Actor, path: &str) -> Option<Value> {
    working(store, who)
        .into_iter()
        .find(|(at, _)| at == path)
        .map(|(_, value)| value)
}

fn publish(store: &ConfigStore, who: &Actor, version: Option<&str>) -> Result<Published, RpcError> {
    store.write_trusted(
        who,
        &Publish {
            environment: EnvironmentRef::default(),
            version: version.map(Into::into),
            accept_volume_loss: Vec::new(),
        },
        &Trusted::default(),
    )
}

fn discard(
    store: &ConfigStore,
    who: &Actor,
    path: Option<&str>,
    version: Option<&str>,
) -> Result<Discarded, RpcError> {
    store.write(
        who,
        &Discard {
            environment: EnvironmentRef::default(),
            path: path.map(|path| SettingPath::parse(path).unwrap()),
            version: version.map(Into::into),
        },
    )
}

#[test]
fn diff_groups_new_services_and_compares_edits_with_their_introduction() {
    let (store, who) = shop();
    set(&store, &who, "web.replicas", json!(3));
    let view = diff(&store, &who);
    assert_eq!(view.version, "4:0:0.0");
    assert_eq!(view.saved, None);
    assert!(!view.published);
    assert_eq!(
        serde_json::to_value(&view.changes).unwrap(),
        json!([
            {
                "type": "service", "id": "00000000-0000-4000-8000-000000000003", "name": "web",
                "lifecycle": "create", "comparison": "introduction",
                "settings": [{ "path": "web.replicas", "kind": "update", "before": 1, "after": 3, "canRestore": true }],
                "data": null,
            },
            {
                "type": "service", "id": "00000000-0000-4000-8000-000000000004", "name": "api",
                "lifecycle": "create", "comparison": "introduction", "settings": [], "data": null,
            },
        ])
    );
    assert_eq!(view.total_count, 3);
}

#[test]
fn publish_saves_working_state_once() {
    let (store, who) = shop();
    let version = diff(&store, &who).version;
    let published = publish(&store, &who, Some(&version)).unwrap();
    assert_eq!((published.saved, published.created), (Revision(1), true));
    let view = diff(&store, &who);
    assert_eq!(view.saved, Some(Revision(1)));
    assert!(view.published);
    // Published is not deployed: the changes still show against Head.
    assert_eq!(view.changes.len(), 2);

    let again = publish(&store, &who, None).unwrap();
    assert_eq!((again.saved, again.created), (Revision(1), false));
    set(&store, &who, "web.replicas", json!(2));
    assert!(!diff(&store, &who).published);
    assert_eq!(publish(&store, &who, None).unwrap().saved, Revision(2));
}

#[test]
fn a_stale_review_is_refused_with_the_fresh_one() {
    let (store, who) = shop();
    let reviewed = diff(&store, &who).version;
    set(&store, &who, "web.replicas", json!(3));
    let fresh = diff(&store, &who);
    for error in [
        publish(&store, &who, Some(&reviewed)).unwrap_err(),
        discard(&store, &who, None, Some(&reviewed)).unwrap_err(),
    ] {
        assert_eq!(error.code, RpcErrorCode::Conflict);
        assert_eq!(
            error.details.pointer("/diff/version"),
            Some(&json!(fresh.version))
        );
    }
    // Nothing moved.
    assert_eq!(diff(&store, &who), fresh);

    // A publish moves Saved State, which stales a review taken before it.
    publish(&store, &who, Some(&fresh.version)).unwrap();
    let error = discard(&store, &who, None, Some(&fresh.version)).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
}

#[test]
fn resetting_a_new_nodes_setting_uses_its_introduction_and_publishes_nothing() {
    let (store, who) = shop();
    set(&store, &who, "web.replicas", json!(4));
    set(&store, &who, "web.image", json!("nginx:2"));
    let discarded = discard(&store, &who, Some("web.replicas"), None).unwrap();
    assert_eq!(discarded.saved, None);
    assert_eq!(value(&store, &who, "web.replicas"), Some(json!(1)));
    assert_eq!(value(&store, &who, "web.image"), Some(json!("nginx:2")));
    let view = diff(&store, &who);
    assert_eq!(view.saved, None);
    assert_eq!(
        view.changes
            .first()
            .unwrap()
            .settings
            .iter()
            .map(|row| row.path.as_str())
            .collect::<Vec<_>>(),
        ["web.image"]
    );
}

/// The paths of `service`'s changed Settings in the diff.
fn changed(store: &ConfigStore, who: &Actor, service: &str) -> Vec<String> {
    diff(store, who)
        .changes
        .into_iter()
        .find(|change| change.name == service)
        .map(|change| change.settings.into_iter().map(|row| row.path).collect())
        .unwrap_or_default()
}

#[test]
fn edits_to_a_published_service_never_deployed_are_changes_that_discard_resets() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.preDeployCommand", json!("Jenje"));
    // Never deployed, it compares against its introduction: the edit is a change.
    let view = diff(&store, &who);
    let web = view
        .changes
        .iter()
        .find(|change| change.name == "web")
        .unwrap();
    assert_eq!(web.lifecycle, ReviewLifecycleKind::Create);
    assert_eq!(web.comparison, Some(ReviewComparisonRole::Introduction));
    assert_eq!(changed(&store, &who, "web"), ["web.preDeployCommand"]);
    assert_eq!(view.total_count, 3);
    // Discard targets it; Saved State never held the edit.
    let discarded = discard(&store, &who, Some("web.preDeployCommand"), None).unwrap();
    assert_eq!(discarded.saved, Some(Revision(1)));
    assert_eq!(
        value(&store, &who, "web.preDeployCommand"),
        Some(Value::Null)
    );
    assert!(changed(&store, &who, "web").is_empty());
    // Published with the edit, a discard resets Saved State too.
    set(&store, &who, "web.replicas", json!(4));
    publish(&store, &who, None).unwrap();
    assert_eq!(changed(&store, &who, "web"), ["web.replicas"]);
    let discarded = discard(&store, &who, Some("web.replicas"), None).unwrap();
    assert_eq!(discarded.saved, Some(Revision(3)));
    assert_eq!(value(&store, &who, "web.replicas"), Some(json!(1)));
    let view = diff(&store, &who);
    assert!(view.published);
    assert!(changed(&store, &who, "web").is_empty());
}

#[test]
fn discarding_a_new_service_removes_it_from_working_and_saved_state() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    let discarded = discard(&store, &who, Some("web"), None).unwrap();
    assert_eq!(discarded.saved, Some(Revision(2)));
    assert_eq!(value(&store, &who, "web.image"), None);
    let view = diff(&store, &who);
    assert!(view.published);
    assert_eq!(
        view.changes
            .iter()
            .map(|change| change.name.as_str())
            .collect::<Vec<_>>(),
        ["api"]
    );
}

#[test]
fn discarding_everything_returns_to_head() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    let discarded = discard(&store, &who, None, None).unwrap();
    assert_eq!(discarded.saved, Some(Revision(2)));
    assert!(working(&store, &who).is_empty());
    let view = diff(&store, &who);
    assert!(view.changes.is_empty() && view.published);
    // Nothing left to discard keeps the revision.
    let again = discard(&store, &who, None, None).unwrap();
    assert_eq!(again.environment.revision, discarded.environment.revision);
    assert_eq!(again.saved, Some(Revision(2)));
}

#[test]
fn discard_names_what_it_cannot_find() {
    let (store, who) = shop();
    let error = discard(&store, &who, Some("db"), None).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::NotFound);
    assert_eq!(
        error.details,
        json!({ "did_you_mean": "web", "valid_children": ["web", "api"] })
    );
}

#[test]
fn simultaneous_publishers_save_one_revision() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(PROJECT).unwrap(),
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
                image: Some("nginx:1".into()),
            },
        )
        .unwrap();
    let version = diff(&store, &who).version;
    let published: Vec<Result<Published, RpcError>> = std::thread::scope(|threads| {
        let publishers: Vec<_> = (0..2)
            .map(|_| {
                let (url, who, version) = (&url, &who, &version);
                threads.spawn(move || {
                    let store = ConfigStore::open(url, backend::key()).unwrap();
                    publish(&store, who, Some(version))
                })
            })
            .collect();
        publishers
            .into_iter()
            .map(|one| one.join().unwrap())
            .collect()
    });
    // They take turns: one saves what it reviewed; the other's review is stale.
    let [first, second] = published.try_into().unwrap();
    let (saved, refused) = match (first, second) {
        (Ok(saved), Err(refused)) | (Err(refused), Ok(saved)) => (saved, refused),
        other => panic!("one publisher wins: {other:?}"),
    };
    assert_eq!((saved.saved, saved.created), (Revision(1), true));
    assert_eq!(refused.code, RpcErrorCode::Conflict);
}

#[test]
fn discard_keeps_mounts_it_does_not_name() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateVolume {
                id: VolumeId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("data").unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: "/data".into(),
                }],
                storage: ployz_core::config::VolumeKind::provisioned_default(),
            },
        )
        .unwrap();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.replicas", json!(3));
    discard(&store, &who, Some("web.replicas"), None).unwrap();
    assert_eq!(value(&store, &who, "web.mounts.data"), Some(json!("/data")));
    // web was introduced before it mounted data: discarding the mount takes it out.
    discard(&store, &who, Some("web.mounts.data"), None).unwrap();
    assert_eq!(value(&store, &who, "web.mounts.data"), None);
}
