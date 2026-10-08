//! Review, Publish and Discard through `read` and `write` only, on SQLite and on
//! Postgres (see `backend`).
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::{ReviewComparisonRole, ReviewLifecycleKind};
use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, AddDomain, Admit, Approval, ApprovalDigest, Change, ConfigStore, CreateProject,
    CreateService, CreateVolume, DataEffect, Deploy, DeploymentId, DestructiveEffect,
    DestructiveKind, DiffQuery, DiffView, Discard, Discarded, Edit, EnvironmentId,
    EnvironmentQuery, EnvironmentRef, Hostname, Mount, OrganizationId, ProjectId, ProjectName,
    Publish, Published, RemoveDomain, RemoveService, RemoveVolume, Revision, ServiceLineageId,
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
                    template: None,
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
    publish_as(store, who, version, Approval::NotRequired)
}

fn publish_as(
    store: &ConfigStore,
    who: &Actor,
    version: Option<&str>,
    approval: Approval,
) -> Result<Published, RpcError> {
    store.write_trusted(
        who,
        &Publish {
            environment: EnvironmentRef::default(),
            version: version.map(Into::into),
            accept_volume_loss: Vec::new(),
        },
        &Trusted {
            approval,
            ..Trusted::default()
        },
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
                "row": "00000000-0000-4000-8000-000000000003:node",
                "lifecycle": "create", "comparison": "introduction",
                "settings": [{
                    "path": "web.replicas", "kind": "update", "before": 1, "after": 3, "canRestore": true,
                    "row": "00000000-0000-4000-8000-000000000003:replicas",
                }],
                "data": null, "restarts": [],
            },
            {
                "type": "service", "id": "00000000-0000-4000-8000-000000000004", "name": "api",
                "row": "00000000-0000-4000-8000-000000000004:node", "lifecycle": "create", "comparison": "introduction", "settings": [], "data": null, "restarts": [],
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
    assert_eq!(
        (published.saved, published.created),
        (Some(Revision(1)), true)
    );
    let view = diff(&store, &who);
    assert_eq!(view.saved, Some(Revision(1)));
    assert!(view.published);
    // Published is not deployed: the changes still show against Head.
    assert_eq!(view.changes.len(), 2);

    let again = publish(&store, &who, None).unwrap();
    assert_eq!((again.saved, again.created), (Some(Revision(1)), false));
    set(&store, &who, "web.replicas", json!(2));
    assert!(!diff(&store, &who).published);
    assert_eq!(
        publish(&store, &who, None).unwrap().saved,
        Some(Revision(2))
    );
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
        ["web.source"]
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
                template: None,
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
    assert_eq!((saved.saved, saved.created), (Some(Revision(1)), true));
    assert_eq!(refused.code, RpcErrorCode::Conflict);
}

#[test]
fn discard_keeps_mounts_it_does_not_name() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateVolume {
                // Three replicas of web write it below.
                shared_writes: true,
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
    // The mount falls in web's row for the Volume's lineage, as a Sync moves it.
    let mount = diff(&store, &who)
        .changes
        .into_iter()
        .flat_map(|change| change.settings)
        .find(|row| row.path == "web.mounts.data")
        .unwrap();
    assert_eq!(
        mount.row.unwrap().to_string(),
        "00000000-0000-4000-8000-000000000003:mounts.00000000-0000-4000-8000-000000000005"
    );
    set(&store, &who, "web.replicas", json!(3));
    discard(&store, &who, Some("web.replicas"), None).unwrap();
    assert_eq!(value(&store, &who, "web.mounts.data"), Some(json!("/data")));
    // web was introduced before it mounted data: discarding the mount takes it out.
    discard(&store, &who, Some("web.mounts.data"), None).unwrap();
    assert_eq!(value(&store, &who, "web.mounts.data"), None);
}

#[test]
fn discarding_a_renamed_config_mount_restores_working_and_saved_state() {
    let (store, who) = shop();
    backend::deploy(&store, &who, "production", 1);
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ConfigName::parse("sentry").unwrap(),
                mounts: vec![ployz_store::ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/new".into(),
                }],
            },
        )
        .unwrap();
    let saved = publish(&store, &who, None).unwrap().saved.unwrap();
    let rename = |from: &str, to: &str| {
        store
            .write(
                &who,
                &ployz_store::RenameConfig {
                    environment: EnvironmentRef::default(),
                    config: ployz_core::ConfigName::parse(from).unwrap(),
                    name: ployz_core::ConfigName::parse(to).unwrap(),
                },
            )
            .unwrap();
    };
    rename("sentry", "errors");

    let discarded = discard(&store, &who, Some("web.configs.errors"), None).unwrap();
    assert_eq!(discarded.saved, Some(Revision(saved.0 + 1)));
    assert_eq!(value(&store, &who, "web.configs.errors"), None);
    let configs = store
        .read(&who, &ployz_store::ConfigsQuery::default())
        .unwrap();
    assert_eq!(
        configs.configs.first().unwrap().config.name.as_str(),
        "errors"
    );
    assert!(!diff(&store, &who).published);

    rename("errors", "sentry");
    assert!(diff(&store, &who).published);
}

#[test]
fn discarding_a_variable_or_its_export_restores_working_and_saved_state() {
    let (store, who) = shop();
    set(&store, &who, "web.env.KEY", json!("old"));
    backend::deploy(&store, &who, "production", 1);
    set(&store, &who, "web.env.KEY", json!("new"));
    set(&store, &who, "web.env.KEY.exported", json!(true));
    publish(&store, &who, None).unwrap();

    discard(&store, &who, Some("web.env.KEY.exported"), None).unwrap();
    assert_eq!(
        value(&store, &who, "web.env.KEY.exported"),
        Some(json!(false))
    );
    assert_eq!(value(&store, &who, "web.env.KEY"), Some(json!("new")));
    assert!(diff(&store, &who).published);

    discard(&store, &who, Some("web.env.KEY"), None).unwrap();
    assert_eq!(value(&store, &who, "web.env.KEY"), Some(json!("old")));
    let view = diff(&store, &who);
    assert!(view.changes.is_empty());
    assert!(view.published);
}

/// A row of a compound Setting discards that Setting: a healthcheck's path edit
/// discards `web.healthcheck`, and a source emptied the Setting it removed.
#[test]
fn a_compound_settings_row_discards_that_setting() {
    let (store, who) = shop();
    set(&store, &who, "web.healthcheck", json!("/old"));
    backend::deploy(&store, &who, "production", 1);
    set(&store, &who, "web.healthcheck", json!("/new"));
    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("api.image").unwrap(),
                }],
            },
        )
        .unwrap();
    let view = diff(&store, &who);
    let rows: Vec<_> = view
        .changes
        .iter()
        .flat_map(|node| &node.settings)
        .map(|row| (row.path.as_str(), row.can_restore))
        .collect();
    assert_eq!(rows, [("web.healthcheck", true), ("api.source", true)]);
    for (path, _) in rows {
        discard(&store, &who, Some(path), None).unwrap();
    }
    assert!(diff(&store, &who).changes.is_empty());
    assert_eq!(value(&store, &who, "api.image"), Some(json!("caddy:2")));
}

const WEB: &str = "00000000-0000-4000-8000-000000000003";

fn apply_all(store: &ConfigStore, who: &Actor, trusted: &Trusted) {
    let id = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
    store
        .write_trusted(
            who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
            }),
            trusted,
        )
        .unwrap();
    backend::run(store, &id);
}

fn remove_service(store: &ConfigStore, who: &Actor, name: &str) {
    store
        .write(
            who,
            &RemoveService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse(name).unwrap(),
            },
        )
        .unwrap();
}

fn mount_data(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &CreateVolume {
                shared_writes: false,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("data").unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: "/data".into(),
                }],
            },
        )
        .unwrap();
}

fn effects(store: &ConfigStore, who: &Actor) -> Vec<(DestructiveKind, String)> {
    diff(store, who)
        .effects
        .into_iter()
        .map(|effect| (effect.kind, effect.path))
        .collect()
}

#[test]
fn removing_a_deployed_service_is_destructive_and_a_new_one_is_not() {
    let (store, who) = shop();
    remove_service(&store, &who, "api");
    assert!(diff(&store, &who).effects.is_empty());
    apply_all(&store, &who, &Trusted::default());
    remove_service(&store, &who, "web");
    assert_eq!(
        Vec::from_iter(diff(&store, &who).effects),
        [DestructiveEffect {
            kind: DestructiveKind::RemovesService,
            node: WEB.into(),
            path: "web".into(),
        }]
    );
}

#[test]
fn deleting_a_deployed_volume_is_destructive() {
    let (store, who) = shop();
    mount_data(&store, &who);
    apply_all(&store, &who, &Trusted::default());
    store
        .write(
            &who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(
        effects(&store, &who),
        [(DestructiveKind::DeletesVolume, "volumes.data".to_owned())]
    );
}

#[test]
fn detaching_a_volume_from_a_deployed_service_is_destructive_and_keeps_its_data() {
    let (store, who) = shop();
    mount_data(&store, &who);
    apply_all(&store, &who, &Trusted::default());
    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("web.mounts.data").unwrap(),
                }],
            },
        )
        .unwrap();
    assert_eq!(diff(&store, &who).changes[0].data, Some(DataEffect::Kept));
    assert_eq!(
        effects(&store, &who),
        [(
            DestructiveKind::DetachesVolume,
            "web.mounts.data".to_owned()
        )]
    );
}

#[test]
fn removing_a_deployed_domain_is_destructive() {
    let (store, who) = shop();
    store
        .write_trusted(
            &who,
            &AddDomain {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
                hostname: Some(Hostname::parse("app.example.com").unwrap()),
                port: None,
            },
            &Trusted::default(),
        )
        .unwrap();
    apply_all(&store, &who, &Trusted::default());
    store
        .write_trusted(
            &who,
            &RemoveDomain {
                environment: EnvironmentRef::default(),
                domain: "app.example.com".into(),
            },
            &Trusted::default(),
        )
        .unwrap();
    let effects = effects(&store, &who);
    assert_eq!(effects.len(), 1, "{effects:?}");
    assert_eq!(effects[0].0, DestructiveKind::RemovesDomain);
    assert!(effects[0].1.starts_with("web.routes."), "{effects:?}");
}

#[test]
fn rolling_a_deployed_service_destroys_nothing_and_publishes_unasked() {
    let (store, who) = shop();
    apply_all(&store, &who, &Trusted::default());
    set(&store, &who, "web.image", json!("nginx:2"));
    set(&store, &who, "web.env.MODE", json!("fast"));
    set(&store, &who, "web.replicas", json!(3));
    assert!(diff(&store, &who).effects.is_empty());
    publish_as(&store, &who, None, Approval::Required).unwrap();
}

#[test]
fn publishing_a_removal_waits_for_a_human_to_approve_exactly_it() {
    let (store, who) = shop();
    apply_all(&store, &who, &Trusted::default());
    remove_service(&store, &who, "web");
    let refused = publish_as(&store, &who, None, Approval::Required).unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    assert_eq!(
        refused.details["effects"],
        json!([{"kind": "removes_service", "node": WEB, "path": "web"}])
    );
    assert!(
        refused.message.contains("remove Service web"),
        "{}",
        refused.message
    );
    assert!(refused.details.get("retry").is_none());
    let approval = refused.details["approval"].as_str().unwrap();
    let version = diff(&store, &who).version;
    assert!(approval.starts_with(&format!("{version}:")), "{approval}");
    let approved = Approval::Approved(ApprovalDigest::parse(approval).unwrap());
    publish_as(&store, &who, None, approved).unwrap();
}

#[test]
fn an_approval_of_an_older_review_is_refused_with_the_fresh_one() {
    let (store, who) = shop();
    apply_all(&store, &who, &Trusted::default());
    remove_service(&store, &who, "web");
    let first = publish_as(&store, &who, None, Approval::Required).unwrap_err();
    let old = first.details["approval"].as_str().unwrap().to_owned();
    set(&store, &who, "api.replicas", json!(2));
    let stale = Approval::Approved(ApprovalDigest::parse(&old).unwrap());
    let refused = publish_as(&store, &who, None, stale).unwrap_err();
    assert_eq!(refused.code.as_str(), "approval_required");
    assert_ne!(refused.details["approval"], json!(old));
    assert_eq!(
        refused.details["diff"]["version"],
        json!(diff(&store, &who).version)
    );
}

#[test]
#[ignore = "perf"]
fn publish_review_perf() {
    let store = ConfigStore::open("sqlite::memory:", backend::key()).unwrap();
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
    for n in 0..200_u32 {
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-1{n:011}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(format!("s{n}")).unwrap(),
                    image: Some("nginx:1".into()),
                    template: None,
                },
            )
            .unwrap();
    }
    apply_all(&store, &who, &Trusted::default());
    let rounds = 200_u32;
    let mut spent = std::time::Duration::ZERO;
    for round in 0..rounds {
        let changes = (0..50)
            .map(|n| Change::Set {
                path: SettingPath::parse(&format!("s{n}.replicas")).unwrap(),
                value: json!(2 + round % 2),
            })
            .collect();
        store
            .write(
                &who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes,
                },
            )
            .unwrap();
        let started = std::time::Instant::now();
        store
            .write_trusted(
                &who,
                &Publish {
                    environment: EnvironmentRef::default(),
                    version: None,
                    accept_volume_loss: Vec::new(),
                },
                &Trusted::default(),
            )
            .unwrap();
        spent += started.elapsed();
    }
    println!(
        "publish (review + gate), 50 staged changes over 200 deployed Services: {} us mean over {rounds} publishes",
        spent.as_micros() / u128::from(rounds)
    );
}
