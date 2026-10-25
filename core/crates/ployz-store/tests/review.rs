//! Review, Publish and Discard through `read` and `write` only, on SQLite and on
//! Postgres (see `backend`).
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::{ReviewComparisonRole, ReviewLifecycleKind};
use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, AddDomain, Admit, Approval, ApprovalDigest, Asks, Change, Command, ConfigStore,
    CreateProject, CreateService, CreateVolume, DataEffect, Deploy, DeploymentId,
    DestructiveEffect, DestructiveKind, DiffQuery, DiffView, Discard, Discarded, Edit,
    EnvironmentId, EnvironmentQuery, EnvironmentRef, Hostname, Mount, OrganizationId, Permission,
    ProjectId, ProjectName, Publish, Published, RemoveDomain, RemoveService, RemoveVolume,
    Revision, RunningObservation, ServiceLineageId, SettingPath, Trusted, VolumeId, VolumeName,
};
use serde_json::{Value, json};

mod backend;

const PROJECT: &str = "00000000-0000-4000-8000-000000000001";
const ENVIRONMENT: &str = "00000000-0000-4000-8000-000000000002";

/// A store with Project `shop` and new Services `web` (nginx) and `api` (caddy).
fn shop() -> (ConfigStore, Actor) {
    shop_in(backend::open())
}

fn shop_in(store: ConfigStore) -> (ConfigStore, Actor) {
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

/// Variable `key` of the Service created as `service` in the default Environment, as the
/// browser sealed it holding ciphertext `n`.
fn sealed(service: u8, key: &str, n: u8) -> Value {
    let service = format!("00000000-0000-4000-8000-0000000000{service:02}");
    backend::sealed_secret(ENVIRONMENT, &service, key, n)
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
            id: None,
            environment: EnvironmentRef::default(),
            version: version.map(Into::into),
            message: None,
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
            target: Some(ployz_store::DiscardTarget::Head),
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
                "type": "service", "id": "00000000-0000-4000-8000-000000000004", "name": "api",
                "row": "00000000-0000-4000-8000-000000000004:node", "lifecycle": "create", "comparison": "introduction",
                "settings": [{
                    "path": "api.image", "kind": "add", "before": null, "after": "caddy:2", "canRestore": false,
                    "row": "00000000-0000-4000-8000-000000000004:source",
                }],
                "data": null, "restarts": [],
            },
            {
                "type": "service", "id": "00000000-0000-4000-8000-000000000003", "name": "web",
                "row": "00000000-0000-4000-8000-000000000003:node",
                "lifecycle": "create", "comparison": "introduction",
                // What it runs, though the introduction already holds it.
                "settings": [{
                    "path": "web.image", "kind": "add", "before": null, "after": "nginx:1", "canRestore": false,
                    "row": "00000000-0000-4000-8000-000000000003:source",
                }, {
                    "path": "web.replicas", "kind": "update", "before": 1, "after": 3, "canRestore": true,
                    "row": "00000000-0000-4000-8000-000000000003:replicas",
                }],
                "data": null, "restarts": [],
            },
        ])
    );
    assert_eq!(view.total_count, 5);
}

#[test]
fn a_draft_reversal_lists_by_name_among_the_deploy_changes() {
    let (store, who) = shop();
    backend::deploy(&store, &who, "production", 1);
    set(&store, &who, "api.replicas", json!(3));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.replicas", json!(1));
    set(&store, &who, "web.replicas", json!(2));
    let names: Vec<_> = diff(&store, &who)
        .review_changes()
        .into_iter()
        .map(|change| change.name)
        .collect();
    assert_eq!(names, ["api", "web"]);
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
            .iter()
            .find(|change| change.name == "web")
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
    assert_eq!(
        changed(&store, &who, "web"),
        ["web.image", "web.preDeployCommand"]
    );
    assert_eq!(view.total_count, 5);
    // Discard targets it; Saved State never held the edit.
    let discarded = discard(&store, &who, Some("web.preDeployCommand"), None).unwrap();
    assert_eq!(discarded.saved, Some(Revision(1)));
    assert_eq!(
        value(&store, &who, "web.preDeployCommand"),
        Some(Value::Null)
    );
    assert_eq!(changed(&store, &who, "web"), ["web.image"]);
    set(&store, &who, "web.replicas", json!(4));
    publish(&store, &who, None).unwrap();
    assert_eq!(changed(&store, &who, "web"), ["web.image", "web.replicas"]);
    let discarded = discard(&store, &who, Some("web.replicas"), None).unwrap();
    assert_eq!(discarded.saved, Some(Revision(2)));
    assert_eq!(value(&store, &who, "web.replicas"), Some(json!(1)));
    let view = diff(&store, &who);
    assert!(!view.published);
    assert_eq!(changed(&store, &who, "web"), ["web.image"]);
}

#[test]
fn discarding_a_new_service_removes_only_working_state() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    let discarded = discard(&store, &who, Some("web"), None).unwrap();
    assert_eq!(discarded.saved, Some(Revision(1)));
    assert_eq!(value(&store, &who, "web.image"), None);
    let view = diff(&store, &who);
    assert!(!view.published);
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
    assert_eq!(discarded.saved, Some(Revision(1)));
    assert!(working(&store, &who).is_empty());
    let view = diff(&store, &who);
    assert!(view.changes.is_empty() && !view.published);
    assert_eq!(view.draft_count, 2);
    // Nothing left to discard keeps the revision.
    let again = discard(&store, &who, None, None).unwrap();
    assert_eq!(again.environment.revision, discarded.environment.revision);
    assert_eq!(again.saved, Some(Revision(1)));
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
                storage_locked: false,
                restored_from: None,
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
fn discarding_a_renamed_config_mount_stages_a_reversal() {
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
    assert_eq!(discarded.saved, Some(saved));
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
    assert!(!diff(&store, &who).published);
}

#[test]
fn discarding_a_variable_or_its_export_stages_a_reversal() {
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
    assert!(!diff(&store, &who).published);

    discard(&store, &who, Some("web.env.KEY"), None).unwrap();
    assert_eq!(value(&store, &who, "web.env.KEY"), Some(json!("old")));
    let view = diff(&store, &who);
    assert!(view.changes.is_empty());
    assert!(!view.published);
    assert_eq!(view.draft_count, 2);
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
    assert_eq!(rows, [("api.source", true), ("web.healthcheck", true)]);
    for (path, _) in rows {
        discard(&store, &who, Some(path), None).unwrap();
    }
    assert!(diff(&store, &who).changes.is_empty());
    assert_eq!(value(&store, &who, "api.image"), Some(json!("caddy:2")));
}

#[test]
fn saved_history_keeps_message_actor_and_no_op_save_immutable() {
    let (store, mut who) = shop();
    who.principal = Some(ployz_store::Principal::parse("Ada Lovelace").unwrap());
    let held = Trusted {
        permissions: Some(Permission::ALL.iter().copied().collect()),
        ..Trusted::default()
    };
    let save = |message: &str| {
        store
            .write_trusted(
                &who,
                &Publish {
                    message: Some(message.into()),
                    ..Publish::default()
                },
                &held,
            )
            .unwrap()
    };
    assert!(save("  First version  ").created);
    let before = store
        .read(&who, &ployz_store::HistoryQuery::default())
        .unwrap();
    let revision = before.revisions.first().unwrap();
    assert_eq!(revision.message.as_deref(), Some("First version"));
    assert_eq!(revision.saved_by, who.principal);
    assert!(revision.saved_at.is_some());
    assert_eq!(revision.predecessor, None);
    let mut created: Vec<_> = revision
        .changes
        .iter()
        .map(|change| (change.name.as_str(), change.lifecycle))
        .collect();
    created.sort_by_key(|(name, _)| *name);
    assert_eq!(
        created,
        [
            ("api", ReviewLifecycleKind::Create),
            ("web", ReviewLifecycleKind::Create)
        ]
    );
    assert!(!save("A duplicate must not replace the message").created);
    assert_eq!(
        store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap(),
        before
    );
    store
        .write_trusted(
            &who,
            &Discard {
                environment: EnvironmentRef::default(),
                target: Some(ployz_store::DiscardTarget::Head),
                path: Some(SettingPath::parse("web").unwrap()),
                version: None,
            },
            &held,
        )
        .unwrap();
    assert_eq!(
        store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap()
            .revisions,
        before.revisions
    );
}

#[test]
fn history_restore_previews_exact_overwrites_and_refuses_stale_confirmation() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.replicas", json!(2));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.replicas", json!(5));
    set(&store, &who, "api.startCommand", json!("keep"));
    let query = ployz_store::HistoryPreviewQuery {
        environment: EnvironmentRef::default(),
        revision: Revision(1),
        action: ployz_store::HistoryAction::Restore,
        change: None,
    };
    let preview = store.read(&who, &query).unwrap();
    assert_eq!(preview.overwritten, ["api.startCommand", "web.replicas"]);
    assert_eq!(value(&store, &who, "web.replicas"), Some(json!(5)));
    let command = ployz_store::StageHistory {
        environment: EnvironmentRef::default(),
        revision: query.revision,
        action: query.action,
        change: None,
        version: preview.version,
        accept_overwrite: false,
    };
    let error = store.write(&who, &command).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(value(&store, &who, "web.replicas"), Some(json!(5)));
    set(&store, &who, "web.replicas", json!(6));
    let error = store
        .write(
            &who,
            &ployz_store::StageHistory {
                accept_overwrite: true,
                ..command
            },
        )
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    let fresh = store.read(&who, &query).unwrap();
    store
        .write(
            &who,
            &ployz_store::StageHistory {
                environment: EnvironmentRef::default(),
                revision: query.revision,
                action: query.action,
                change: None,
                version: fresh.version,
                accept_overwrite: true,
            },
        )
        .unwrap();
    assert_eq!(value(&store, &who, "web.replicas"), Some(json!(1)));
    assert_eq!(diff(&store, &who).saved, Some(Revision(2)));
}

#[test]
fn history_undo_stages_only_its_delta_and_preserves_later_unrelated_edits() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.replicas", json!(4));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.startCommand", json!("keep"));
    let query = ployz_store::HistoryPreviewQuery {
        environment: EnvironmentRef::default(),
        revision: Revision(2),
        action: ployz_store::HistoryAction::Undo,
        change: None,
    };
    let preview = store.read(&who, &query).unwrap();
    assert!(preview.overwritten.is_empty());
    store
        .write(
            &who,
            &ployz_store::StageHistory {
                environment: EnvironmentRef::default(),
                revision: query.revision,
                action: query.action,
                change: None,
                version: preview.version,
                accept_overwrite: false,
            },
        )
        .unwrap();
    assert_eq!(value(&store, &who, "web.replicas"), Some(json!(1)));
    assert_eq!(value(&store, &who, "api.startCommand"), Some(json!("keep")));
    assert_eq!(diff(&store, &who).saved, Some(Revision(2)));
    assert_eq!(
        store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap()
            .revisions
            .len(),
        2
    );
    let error = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                revision: Revision(1),
                ..query
            },
        )
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
}

#[test]
fn discard_of_a_never_deployed_save_lists_it_and_its_path_brings_it_back() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    let discard_path = |target, path: &str| {
        store.write(
            &who,
            &Discard {
                environment: EnvironmentRef::default(),
                target,
                path: Some(SettingPath::parse(path).unwrap()),
                version: None,
            },
        )
    };
    let reversed = |discarded: &Discarded| {
        let mut names: Vec<_> = discarded
            .reversed
            .iter()
            .map(|reversal| (reversal.change.name.clone(), reversal.change.lifecycle))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        names
    };

    let discarded = discard(&store, &who, None, None).unwrap();
    assert!(discarded.changed);
    let removed = vec![
        ("api".to_owned(), ReviewLifecycleKind::Delete),
        ("web".to_owned(), ReviewLifecycleKind::Delete),
    ];
    assert_eq!(reversed(&discarded), removed);
    assert_eq!(value(&store, &who, "web.image"), None);
    let view = diff(&store, &who);
    assert_eq!(view.total_count, 0);
    assert_eq!(view.review_changes().len(), 2);
    assert!(view.review_count() >= 2, "status counts what diff lists");

    let again = discard(&store, &who, None, None).unwrap();
    assert!(!again.changed);
    assert_eq!(reversed(&again), removed);

    let error = discard_path(Some(ployz_store::DiscardTarget::Head), "web").unwrap_err();
    assert_eq!(error.code, RpcErrorCode::NotFound);

    assert!(discard_path(None, "web").unwrap().changed);
    assert_eq!(value(&store, &who, "web.image"), Some(json!("nginx:1")));
    assert_eq!(value(&store, &who, "api.image"), None);
    let view = diff(&store, &who);
    assert_eq!(
        view.review_changes()
            .iter()
            .map(|change| (change.name.as_str(), change.lifecycle))
            .collect::<Vec<_>>(),
        [
            ("api", ReviewLifecycleKind::Delete),
            ("web", ReviewLifecycleKind::Create)
        ]
    );
    assert_eq!(view.reversals().len(), 1);
}

fn discard_all(
    store: &ConfigStore,
    who: &Actor,
    target: Option<ployz_store::DiscardTarget>,
) -> Discarded {
    store
        .write(
            who,
            &Discard {
                target,
                ..Discard::default()
            },
        )
        .unwrap()
}

#[test]
fn a_plain_discard_sends_no_target_so_an_older_store_still_reads_it() {
    let wire = serde_json::to_value(Discard::default()).unwrap();
    assert_eq!(wire.get("target"), None, "{wire}");
}

#[test]
fn a_replaced_service_of_the_same_name_comes_back_only_by_returning_to_saved() {
    let (store, who) = shop();
    backend::deploy(&store, &who, "production", 1);
    remove_service(&store, &who, "web");
    add_service(&store, &who, 5, "web");
    publish(&store, &who, None).unwrap();

    let discarded = discard_all(&store, &who, None);
    assert_eq!(value(&store, &who, "web.image"), Some(json!("nginx:1")));
    let webs: Vec<_> = discarded
        .reversed
        .iter()
        .filter(|reversal| reversal.change.name == "web")
        .collect();
    assert_eq!(webs.len(), 2, "{:?}", discarded.reversed);
    for web in webs {
        assert_eq!(web.way_back.path, None, "`discard web` picks one of two");
        assert!(web.way_back.drops_unsaved);
    }

    assert!(discard_all(&store, &who, Some(ployz_store::DiscardTarget::Saved)).changed);
    assert_eq!(value(&store, &who, "web.image"), Some(json!("postgres:16")));
    assert!(diff(&store, &who).reversals().is_empty());
}

#[test]
fn a_saved_service_whose_name_a_staged_one_took_has_no_path_back() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    remove_service(&store, &who, "web");
    add_service(&store, &who, 5, "web");

    let discarded = discard(&store, &who, Some("api"), None).unwrap();
    assert!(discarded.changed);
    let way_back = |name: &str| {
        discarded
            .reversed
            .iter()
            .find(|reversal| reversal.change.name == name)
            .map(|reversal| reversal.way_back.clone())
            .unwrap_or_else(|| panic!("{name} is reversed: {:?}", discarded.reversed))
    };
    assert_eq!(
        way_back("api").path.map(|path| path.to_string()),
        Some("api".to_owned())
    );
    let web = way_back("web");
    assert_eq!(web.path, None, "`discard web` names the staged web");
    assert!(web.drops_unsaved);
}

#[test]
fn a_config_comes_back_by_its_id_and_a_replaced_one_only_by_returning_to_saved() {
    let (store, who) = shop();
    let sentry = ployz_core::ConfigName::parse("sentry").unwrap();
    let create = |n: u8, name: &ployz_core::ConfigName| {
        store
            .write(
                &who,
                &ployz_store::CreateConfig {
                    id: ployz_store::ConfigId::parse(format!(
                        "00000000-0000-4000-8000-0000000000{n:02}"
                    ))
                    .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: name.clone(),
                    mounts: Vec::new(),
                },
            )
            .unwrap();
        store
            .write(
                &who,
                &ployz_store::PutConfigFile {
                    environment: EnvironmentRef::default(),
                    config: name.clone(),
                    file: ployz_core::ConfigFileName::parse("sentry.yml").unwrap(),
                    content: format!("version: {n}"),
                    mode: None,
                    uid: None,
                    gid: None,
                },
            )
            .unwrap();
    };
    let working_config = || {
        let view = store
            .read(&who, &ployz_store::ConfigsQuery::default())
            .unwrap();
        let mut ids: Vec<_> = view
            .configs
            .into_iter()
            .filter(|listing| listing.change != Some(ReviewLifecycleKind::Delete))
            .map(|listing| listing.config.id.to_string())
            .collect();
        ids.sort();
        ids
    };
    create(10, &sentry);
    backend::deploy(&store, &who, "production", 1);
    store
        .write(
            &who,
            &ployz_store::DeleteConfig {
                environment: EnvironmentRef::default(),
                config: sentry.clone(),
            },
        )
        .unwrap();
    create(11, &sentry);
    create(12, &ployz_core::ConfigName::parse("errors").unwrap());
    publish(&store, &who, None).unwrap();
    let replacement = "00000000-0000-4000-8000-000000000011";

    let errors = "00000000-0000-4000-8000-000000000012";
    let discarded = discard_all(&store, &who, None);
    assert_eq!(working_config(), ["00000000-0000-4000-8000-000000000010"]);
    let back = discarded
        .reversed
        .iter()
        .find(|reversal| reversal.change.node.id == errors)
        .and_then(|reversal| reversal.way_back.path.clone())
        .expect("errors has a way back");
    assert_eq!(back.to_string(), format!("configs.@{errors}"));
    let sentries: Vec<_> = discarded
        .reversed
        .iter()
        .filter(|reversal| reversal.change.name == "sentry")
        .collect();
    assert_eq!(sentries.len(), 2, "{:?}", discarded.reversed);
    for sentry in sentries {
        assert_eq!(sentry.way_back.path, None, "one path brings back neither");
    }
    let alone = store
        .write(
            &who,
            &Discard {
                path: Some(SettingPath::parse(&format!("configs.@{replacement}")).unwrap()),
                ..Discard::default()
            },
        )
        .unwrap_err();
    assert_eq!(
        alone.code,
        RpcErrorCode::Conflict,
        "two Configs named sentry"
    );

    let restored = store
        .write(
            &who,
            &Discard {
                path: Some(back),
                ..Discard::default()
            },
        )
        .unwrap();
    assert!(restored.changed);
    assert_eq!(
        working_config(),
        ["00000000-0000-4000-8000-000000000010", errors]
    );

    assert!(discard_all(&store, &who, Some(ployz_store::DiscardTarget::Saved)).changed);
    assert_eq!(working_config(), [replacement, errors]);
}

#[test]
fn discard_inverse_returns_to_saved_without_history_and_save_records_reversal() {
    let (store, who) = shop();
    backend::deploy(&store, &who, "production", 1);
    set(&store, &who, "web.replicas", json!(3));
    let saved = publish(&store, &who, None).unwrap().saved;
    let history = store
        .read(&who, &ployz_store::HistoryQuery::default())
        .unwrap();
    discard(&store, &who, Some("web.replicas"), None).unwrap();
    let reversed = diff(&store, &who);
    assert_eq!(reversed.total_count, 0);
    assert_eq!(reversed.draft_count, 1);
    assert_eq!(
        reversed
            .review_changes()
            .first()
            .unwrap()
            .settings
            .first()
            .unwrap()
            .path,
        "web.replicas"
    );
    store
        .write(
            &who,
            &Discard {
                environment: EnvironmentRef::default(),
                target: Some(ployz_store::DiscardTarget::Saved),
                path: Some(SettingPath::parse("web.replicas").unwrap()),
                version: Some(reversed.version),
            },
        )
        .unwrap();
    assert_eq!(value(&store, &who, "web.replicas"), Some(json!(3)));
    assert_eq!(diff(&store, &who).draft_count, 0);
    assert_eq!(
        store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap()
            .revisions,
        history.revisions
    );
    discard(&store, &who, Some("web.replicas"), None).unwrap();
    assert_eq!(diff(&store, &who).saved, saved);
    assert!(publish(&store, &who, None).unwrap().created);
    assert_eq!(diff(&store, &who).draft_count, 0);
    assert_eq!(
        store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap()
            .revisions
            .len(),
        history.revisions.len() + 1
    );
}

#[test]
fn discard_review_restores_mixed_runtime_and_inverse_rows_once() {
    let (store, who) = shop();
    backend::deploy(&store, &who, "production", 1);
    set(&store, &who, "web.replicas", json!(3));
    publish(&store, &who, None).unwrap();
    discard(&store, &who, Some("web.replicas"), None).unwrap();
    set(&store, &who, "web.startCommand", json!("runtime change"));
    let before = diff(&store, &who);
    assert_eq!(before.total_count, 1);
    assert_eq!(before.review_changes().first().unwrap().settings.len(), 2);
    let history = store
        .read(&who, &ployz_store::HistoryQuery::default())
        .unwrap();
    let written = store
        .write(
            &who,
            &Discard {
                target: Some(ployz_store::DiscardTarget::Review),
                version: Some(before.version),
                ..Discard::default()
            },
        )
        .unwrap();
    assert!(written.changed);
    assert_eq!(value(&store, &who, "web.replicas"), Some(json!(3)));
    assert_eq!(value(&store, &who, "web.startCommand"), Some(Value::Null));
    assert_eq!(diff(&store, &who).draft_count, 0);
    assert_eq!(
        store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap()
            .revisions,
        history.revisions
    );
}

#[test]
fn discard_review_returns_to_saved_and_keeps_an_undeployed_save_listed() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    let saved = working(&store, &who);
    set(&store, &who, "web.replicas", json!(3));
    let before = diff(&store, &who);
    assert_eq!(before.review_changes().len(), 2);
    assert_eq!(
        before.review_changes().first().unwrap().lifecycle,
        ReviewLifecycleKind::Create
    );
    let history = store
        .read(&who, &ployz_store::HistoryQuery::default())
        .unwrap();
    store
        .write(
            &who,
            &Discard {
                target: Some(ployz_store::DiscardTarget::Review),
                version: Some(before.version),
                ..Discard::default()
            },
        )
        .unwrap();
    assert_eq!(working(&store, &who), saved);
    let after = diff(&store, &who);
    assert_eq!(after.draft_count, 0);
    // Each Service the Save creates lists itself and the image it runs.
    assert_eq!(after.total_count, 4);
    assert_eq!(
        store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap()
            .revisions,
        history.revisions
    );
}

#[test]
fn discard_review_names_the_settings_an_undeployed_save_loses() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.replicas", json!(3));
    let discarded = store
        .write(
            &who,
            &Discard {
                target: Some(ployz_store::DiscardTarget::Review),
                version: Some(diff(&store, &who).version),
                ..Discard::default()
            },
        )
        .unwrap();
    assert_eq!(discarded.dropped, ["web.replicas"]);
}

#[test]
fn discard_review_names_the_source_an_undeployed_save_loses_as_set_names_it() {
    // Core compares a new Service with its introduction, nginx:1, so a Save of
    // nginx:2 lists as `web.source` and one of nginx:1 as this PR's `web.image`.
    for (saved, staged, web_row) in [
        ("nginx:1", "nginx:2", "web.image"),
        ("nginx:2", "nginx:1", "web.source"),
    ] {
        let (store, who) = shop();
        set(&store, &who, "web.image", json!(saved));
        publish(&store, &who, None).unwrap();
        set(&store, &who, "web.image", json!(staged));
        let discarded = store
            .write(
                &who,
                &Discard {
                    target: Some(ployz_store::DiscardTarget::Review),
                    version: Some(diff(&store, &who).version),
                    ..Discard::default()
                },
            )
            .unwrap();
        // api's image row is unchanged, so the discard doesn't name it.
        assert_eq!(discarded.dropped, ["web.image"], "saved {saved}");
        let view = diff(&store, &who);
        let rows: Vec<_> = view
            .review_changes()
            .iter()
            .flat_map(|change| change.settings.clone())
            .map(|row| row.path)
            .collect();
        assert_eq!(rows, ["api.image", web_row], "saved {saved}");
        assert_eq!(value(&store, &who, "web.image"), Some(json!(saved)));
    }
}

#[test]
fn discard_review_names_a_staged_repository_on_a_saved_image_as_the_repository() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    git_edit(&store, &who, "web.repository", json!("acme/web"));
    let discarded = discard_review(&store, &who);
    // The after-view's `web.image` row shares the source RowId; the repository went.
    assert_eq!(discarded.dropped, ["web.branch", "web.repository"]);
    assert_eq!(value(&store, &who, "web.image"), Some(json!("nginx:1")));
}

#[test]
fn discard_review_names_a_staged_image_on_a_saved_repository_as_the_image() {
    let (store, who) = shop();
    git_edit(&store, &who, "web.repository", json!("acme/web"));
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "web.repository");
    set(&store, &who, "web.image", json!("nginx:2"));
    let before = diff_rows(&store, &who);
    let discarded = discard_review(&store, &who);
    assert_eq!(discarded.dropped, ["web.image"]);
    // The staged image carries no branch, so only the saved repository's comes back:
    // a row the discard adds, not one it drops.
    assert_eq!(before, ["api.image", "web.source"]);
    assert_eq!(
        diff_rows(&store, &who),
        ["api.image", "web.branch", "web.source"]
    );
    assert_eq!(
        value(&store, &who, "web.repository"),
        Some(json!("acme/web"))
    );
}

#[test]
fn discard_review_names_a_staged_image_on_a_saved_empty_source_as_the_image() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000006").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("blank").unwrap(),
                image: None,
                template: None,
            },
        )
        .unwrap();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "blank.image", json!("nginx:2"));
    // The saved empty source leaves blank no source row to keep after the discard.
    assert_eq!(discard_review(&store, &who).dropped, ["blank.image"]);
    assert_eq!(value(&store, &who, "blank.image"), None);
}

#[test]
fn discard_review_names_a_deployed_services_source_as_core_does() {
    let (store, who) = shop();
    git_edit(&store, &who, "web.repository", json!("acme/web"));
    apply_all(&store, &who, &Trusted::default());
    set(&store, &who, "web.rootDir", json!("/one"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.rootDir", json!("/two"));
    // The saved `/one` keeps a `web.source` row, but web is deployed: only the
    // rootDir went, and `web.source` is how its review names that.
    assert_eq!(discard_review(&store, &who).dropped, ["web.source"]);
    assert_eq!(value(&store, &who, "web.rootDir"), Some(json!("/one")));
}

fn diff_rows(store: &ConfigStore, who: &Actor) -> Vec<String> {
    diff(store, who)
        .review_changes()
        .iter()
        .flat_map(|change| change.settings.clone())
        .map(|row| row.path)
        .collect()
}

fn unset(store: &ConfigStore, who: &Actor, path: &str) {
    store
        .write(
            who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse(path).unwrap(),
                }],
            },
        )
        .unwrap();
}

fn history_preview(
    store: &ConfigStore,
    who: &Actor,
    revision: u64,
    action: ployz_store::HistoryAction,
) -> Result<ployz_store::HistoryPreview, RpcError> {
    store.read(
        who,
        &ployz_store::HistoryPreviewQuery {
            environment: EnvironmentRef::default(),
            revision: Revision(revision),
            action,
            change: None,
        },
    )
}

#[test]
fn history_of_a_revision_no_save_made_names_the_revision() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    let error = history_preview(&store, &who, 99, ployz_store::HistoryAction::Undo).unwrap_err();
    assert_eq!(error.code.as_str(), "not_found");
    assert_eq!(error.message, "No saved version has revision 99");
    assert_eq!(error.details, json!({ "revision": 99 }));
}

fn stage_history(
    store: &ConfigStore,
    who: &Actor,
    revision: u64,
    action: ployz_store::HistoryAction,
) -> ployz_store::HistoryStaged {
    let query = ployz_store::HistoryPreviewQuery {
        environment: EnvironmentRef::default(),
        revision: Revision(revision),
        action,
        change: None,
    };
    let preview = store.read(who, &query).unwrap();
    store
        .write(
            who,
            &ployz_store::StageHistory {
                environment: EnvironmentRef::default(),
                revision: query.revision,
                action,
                change: None,
                version: preview.version,
                accept_overwrite: true,
            },
        )
        .unwrap()
}

fn stage_change(store: &ConfigStore, who: &Actor, revision: u64, change: &str) {
    let query = ployz_store::HistoryPreviewQuery {
        environment: EnvironmentRef::default(),
        revision: Revision(revision),
        action: ployz_store::HistoryAction::Undo,
        change: Some(change.into()),
    };
    let preview = store.read(who, &query).unwrap();
    assert_eq!(preview.change.as_deref(), Some(change));
    store
        .write(
            who,
            &ployz_store::StageHistory {
                environment: EnvironmentRef::default(),
                revision: query.revision,
                action: query.action,
                change: query.change,
                version: preview.version,
                accept_overwrite: true,
            },
        )
        .unwrap();
}

fn discard_to_saved(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &Discard {
                environment: EnvironmentRef::default(),
                target: Some(ployz_store::DiscardTarget::Saved),
                path: None,
                version: None,
            },
        )
        .unwrap();
}

fn linked(store: &ConfigStore, who: &Actor, revision: u64) -> Vec<(String, usize)> {
    store
        .read(who, &ployz_store::HistoryQuery::default())
        .unwrap()
        .revisions
        .into_iter()
        .find(|saved| saved.revision == Revision(revision))
        .unwrap()
        .linked
        .into_iter()
        .collect()
}

fn add_service(store: &ConfigStore, who: &Actor, n: u8, name: &str) {
    store
        .write(
            who,
            &CreateService {
                id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-0000000000{n:02}"))
                    .unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse(name).unwrap(),
                image: Some("postgres:16".into()),
                template: None,
            },
        )
        .unwrap();
}

#[test]
fn history_undo_of_one_change_keeps_the_rest_of_its_save() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.replicas", json!(2));
    set(&store, &who, "api.startCommand", json!("serve"));
    publish(&store, &who, None).unwrap();
    assert_eq!(linked(&store, &who, 2), []);
    let error = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: EnvironmentRef::default(),
                revision: Revision(2),
                action: ployz_store::HistoryAction::Undo,
                change: Some("web.image".into()),
            },
        )
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::NotFound);
    let error = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: EnvironmentRef::default(),
                revision: Revision(2),
                action: ployz_store::HistoryAction::Restore,
                change: Some("web.replicas".into()),
            },
        )
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    stage_change(&store, &who, 2, "web.replicas");
    assert_eq!(value(&store, &who, "web.replicas"), Some(json!(1)));
    assert_eq!(
        value(&store, &who, "api.startCommand"),
        Some(json!("serve"))
    );
}

#[test]
fn history_undo_of_a_service_add_undoes_same_save_refs_to_it() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    add_service(&store, &who, 10, "pg");
    set(&store, &who, "pg.env.PASS", json!("secret"));
    set(&store, &who, "web.env.DATABASE", json!("${{ pg.PASS }}"));
    set(&store, &who, "api.replicas", json!(3));
    publish(&store, &who, None).unwrap();
    assert_eq!(linked(&store, &who, 2), [("pg".to_owned(), 2)]);
    stage_change(&store, &who, 2, "web.env.DATABASE");
    assert_eq!(value(&store, &who, "web.env.DATABASE"), None);
    assert_eq!(value(&store, &who, "pg.env.PASS"), Some(json!("secret")));
    discard_to_saved(&store, &who);
    stage_change(&store, &who, 2, "pg");
    assert_eq!(value(&store, &who, "web.env.DATABASE"), None);
    assert_eq!(value(&store, &who, "pg.image"), None);
    assert_eq!(value(&store, &who, "api.replicas"), Some(json!(3)));
}

#[test]
fn history_undo_of_a_volume_add_removes_its_mounts() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    mount_data(&store, &who);
    set(&store, &who, "api.replicas", json!(3));
    publish(&store, &who, None).unwrap();
    assert_eq!(linked(&store, &who, 2), [("volumes.data".to_owned(), 2)]);
    stage_change(&store, &who, 2, "volumes.data");
    assert_eq!(value(&store, &who, "web.mounts.data"), None);
    assert!(
        store
            .read(&who, &ployz_store::VolumesQuery::default())
            .unwrap()
            .volumes
            .is_empty()
    );
    assert_eq!(value(&store, &who, "api.replicas"), Some(json!(3)));
}

#[test]
fn history_undo_of_a_volume_that_replaced_a_mount_puts_the_old_mount_back() {
    let (store, who) = shop();
    mount_data(&store, &who);
    publish(&store, &who, None).unwrap();
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
    store
        .write(
            &who,
            &CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse("00000000-0000-4000-8000-000000000007").unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("fresh").unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: "/data".into(),
                }],
            },
        )
        .unwrap();
    publish(&store, &who, None).unwrap();
    stage_change(&store, &who, 2, "volumes.fresh");
    assert_eq!(value(&store, &who, "web.mounts.data"), Some(json!("/data")));
    assert_eq!(value(&store, &who, "web.mounts.fresh"), None);
}

#[test]
fn history_undo_of_a_mount_move_takes_back_what_took_its_old_path() {
    let (store, who) = shop();
    mount_data(&store, &who);
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.mounts.data", json!("/other"));
    store
        .write(
            &who,
            &CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse("00000000-0000-4000-8000-000000000007").unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("fresh").unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: "/data".into(),
                }],
            },
        )
        .unwrap();
    publish(&store, &who, None).unwrap();
    stage_change(&store, &who, 2, "web.mounts.data");
    assert_eq!(value(&store, &who, "web.mounts.data"), Some(json!("/data")));
    assert_eq!(value(&store, &who, "web.mounts.fresh"), None);
}

#[test]
fn history_undo_of_a_service_removal_brings_back_what_it_referred_to() {
    let (store, who) = shop();
    set(&store, &who, "web.env.SOURCE", json!("old"));
    set(&store, &who, "api.env.USE", json!("${{ web.SOURCE }}"));
    publish(&store, &who, None).unwrap();
    remove_service(&store, &who, "api");
    remove_service(&store, &who, "web");
    publish(&store, &who, None).unwrap();
    assert_eq!(linked(&store, &who, 2), [("api".to_owned(), 2)]);
    stage_change(&store, &who, 2, "web");
    assert_eq!(value(&store, &who, "web.env.SOURCE"), Some(json!("old")));
    assert_eq!(value(&store, &who, "api.image"), None);
    discard_to_saved(&store, &who);
    stage_change(&store, &who, 2, "api");
    assert_eq!(
        value(&store, &who, "api.env.USE"),
        Some(json!("${{ web.SOURCE }}"))
    );
    assert_eq!(value(&store, &who, "web.env.SOURCE"), Some(json!("old")));
}

#[test]
fn history_undo_that_leaves_a_broken_reference_stages_and_the_review_lists_it() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    add_service(&store, &who, 10, "pg");
    publish(&store, &who, None).unwrap();
    set(&store, &who, "pg.env.PASS", json!("secret"));
    set(&store, &who, "web.env.DATABASE", json!("${{ pg.PASS }}"));
    publish(&store, &who, None).unwrap();
    stage_history(&store, &who, 2, ployz_store::HistoryAction::Undo);
    assert_eq!(value(&store, &who, "pg.image"), None);
    let diff = store
        .read(
            &who,
            &ployz_store::DiffQuery {
                environment: EnvironmentRef::default(),
            },
        )
        .unwrap();
    let broken: Vec<String> = diff.broken.iter().map(ToString::to_string).collect();
    assert_eq!(
        broken,
        ["web.env.DATABASE references pg, which was removed"]
    );
}

fn rename_service(store: &ConfigStore, who: &Actor, from: &str, to: &str) {
    store
        .write(
            who,
            &ployz_store::RenameService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse(from).unwrap(),
                name: ServiceName::parse(to).unwrap(),
            },
        )
        .unwrap();
}

#[test]
fn history_undo_of_one_side_of_a_name_swap_undoes_both() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    rename_service(&store, &who, "web", "tmp");
    set(&store, &who, "tmp.privateDns", json!("tmp"));
    rename_service(&store, &who, "api", "web");
    set(&store, &who, "web.privateDns", json!("web"));
    rename_service(&store, &who, "tmp", "api");
    set(&store, &who, "api.privateDns", json!("api"));
    publish(&store, &who, None).unwrap();
    for path in ["api.name", "web.privateDns"] {
        stage_change(&store, &who, 2, path);
        assert_eq!(
            value(&store, &who, "web.image"),
            Some(json!("nginx:1")),
            "{path}"
        );
        assert_eq!(
            value(&store, &who, "api.image"),
            Some(json!("caddy:2")),
            "{path}"
        );
        discard_to_saved(&store, &who);
    }
}

fn rename_variable(store: &ConfigStore, who: &Actor, path: &str, to: &str) {
    store
        .write(
            who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Rename {
                    path: SettingPath::parse(path).unwrap(),
                    to: to.to_owned(),
                    update_references: false,
                }],
            },
        )
        .unwrap();
}

#[test]
fn history_undo_of_one_side_of_a_variable_rename_undoes_both() {
    let (store, who) = shop();
    set(&store, &who, "web.env.OLD", json!("value"));
    publish(&store, &who, None).unwrap();
    rename_variable(&store, &who, "web.env.OLD", "NEW");
    publish(&store, &who, None).unwrap();
    for path in ["web.env.OLD", "web.env.NEW"] {
        stage_change(&store, &who, 2, path);
        assert_eq!(
            value(&store, &who, "web.env.OLD"),
            Some(json!("value")),
            "{path}"
        );
        assert_eq!(value(&store, &who, "web.env.NEW"), None, "{path}");
        discard_to_saved(&store, &who);
    }
}

#[test]
fn history_undo_of_a_config_file_replaced_by_one_inside_it_undoes_both() {
    let (store, who) = shop();
    let config = ployz_core::ConfigName::parse("sentry").unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                environment: EnvironmentRef::default(),
                id: ployz_store::ConfigId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                name: config.clone(),
                mounts: Vec::new(),
            },
        )
        .unwrap();
    let put = |file: &str| {
        store
            .write(
                &who,
                &ployz_store::PutConfigFile {
                    environment: EnvironmentRef::default(),
                    config: config.clone(),
                    file: ployz_core::ConfigFileName::parse(file).unwrap(),
                    content: "content".into(),
                    mode: None,
                    uid: None,
                    gid: None,
                },
            )
            .unwrap();
    };
    put("a");
    publish(&store, &who, None).unwrap();
    store
        .write(
            &who,
            &ployz_store::RemoveConfigFile {
                environment: EnvironmentRef::default(),
                config: config.clone(),
                file: ployz_core::ConfigFileName::parse("a").unwrap(),
            },
        )
        .unwrap();
    put("a/b");
    publish(&store, &who, None).unwrap();
    let history = store
        .read(&who, &ployz_store::HistoryQuery::default())
        .unwrap();
    let paths: Vec<String> = history.revisions[0]
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .map(|setting| setting.path.to_string())
        .collect();
    assert_eq!(paths.len(), 2, "{paths:?}");
    let files = store
        .read(
            &who,
            &ployz_store::ConfigItemQuery {
                environment: EnvironmentRef::default(),
                config: config.clone().into(),
            },
        )
        .unwrap();
    for path in &paths {
        stage_change(&store, &who, 2, path);
        let staged = store
            .read(
                &who,
                &ployz_store::ConfigItemQuery {
                    environment: EnvironmentRef::default(),
                    config: config.clone().into(),
                },
            )
            .unwrap();
        assert_ne!(
            serde_json::to_value(&staged).unwrap(),
            serde_json::to_value(&files).unwrap(),
            "{path}"
        );
        assert!(
            !serde_json::to_string(&staged).unwrap().contains("a/b"),
            "{path}"
        );
        discard_to_saved(&store, &who);
    }
}

#[test]
fn history_never_changes_a_sealed_value_and_never_shows_it() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", json!("plain"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 2));
    publish(&store, &who, None).unwrap();
    let query = |revision| ployz_store::HistoryPreviewQuery {
        environment: EnvironmentRef::default(),
        revision: Revision(revision),
        action: ployz_store::HistoryAction::Restore,
        change: None,
    };
    let preview = store.read(&who, &query(2)).unwrap();
    let public = serde_json::to_string(&preview).unwrap();
    assert!(!public.contains(&backend::ciphertext(1)));
    assert!(!public.contains(&backend::ciphertext(2)));
    let history = serde_json::to_string(
        &store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap(),
    )
    .unwrap();
    assert!(!history.contains(&backend::ciphertext(1)));
    assert!(!history.contains(&backend::ciphertext(2)));
    for (revision, action) in [
        (1, ployz_store::HistoryAction::Restore),
        (2, ployz_store::HistoryAction::Restore),
        (3, ployz_store::HistoryAction::Undo),
        (2, ployz_store::HistoryAction::Undo),
    ] {
        let preview = history_preview(&store, &who, revision, action).unwrap();
        assert_eq!(preview.total_count, 0, "#{revision} {action:?}");
        let public = serde_json::to_string(&preview).unwrap();
        assert!(!public.contains(&backend::ciphertext(1)));
        assert!(!public.contains(&backend::ciphertext(2)));
        stage_history(&store, &who, revision, action);
        assert_eq!(diff(&store, &who).draft_count, 0);
    }
    let deployed = backend::deploy(&store, &who, "production", 1);
    assert!(deployed.to_string().contains(&backend::ciphertext(2)));
    assert!(!deployed.to_string().contains(&backend::ciphertext(1)));
}

#[test]
fn history_undo_of_a_sealed_rotation_by_delete_and_add_keeps_the_new_value() {
    let (store, who) = shop();
    set(&store, &who, "web.env.ROT_KEY", sealed(3, "ROT_KEY", 1));
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "web.env.ROT_KEY");
    set(&store, &who, "web.env.ROT_KEY", sealed(3, "ROT_KEY", 2));
    publish(&store, &who, None).unwrap();
    let preview = history_preview(&store, &who, 2, ployz_store::HistoryAction::Undo).unwrap();
    assert_eq!(preview.total_count, 0);
    stage_history(&store, &who, 2, ployz_store::HistoryAction::Undo);
    assert_eq!(diff(&store, &who).draft_count, 0);
    let deployed = backend::deploy(&store, &who, "production", 1);
    assert!(deployed.to_string().contains(&backend::ciphertext(2)));
    assert!(!deployed.to_string().contains(&backend::ciphertext(1)));
}

/// The keys a History action brought back Not set, as `service.KEY`.
fn not_set(staged: &ployz_store::HistoryStaged) -> Vec<String> {
    staged
        .not_set
        .iter()
        .map(|variable| format!("{}.{}", variable.service, variable.key))
        .collect()
}

#[test]
fn history_brings_a_sealed_key_deleted_since_back_not_set() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "web.env.TOKEN");
    publish(&store, &who, None).unwrap();
    for (revision, action) in [
        (1, ployz_store::HistoryAction::Restore),
        (2, ployz_store::HistoryAction::Undo),
    ] {
        let preview = history_preview(&store, &who, revision, action).unwrap();
        assert!(
            !serde_json::to_string(&preview)
                .unwrap()
                .contains(&backend::ciphertext(1))
        );
        let staged = stage_history(&store, &who, revision, action);
        assert_eq!(not_set(&staged), ["web.TOKEN"], "#{revision} {action:?}");
        assert_eq!(
            value(&store, &who, "web.env.TOKEN"),
            Some(json!({ "secret": false })),
            "#{revision} {action:?}"
        );
        discard_to_saved(&store, &who);
    }
}

#[test]
fn history_brings_a_removed_service_back_with_its_sealed_keys_not_set() {
    let (store, who) = shop();
    set(&store, &who, "api.env.PLAIN", json!("kept"));
    set(
        &store,
        &who,
        "api.env.STRIPE_KEY",
        sealed(4, "STRIPE_KEY", 1),
    );
    set(
        &store,
        &who,
        "api.env.WEBHOOK_SECRET",
        sealed(4, "WEBHOOK_SECRET", 2),
    );
    publish(&store, &who, None).unwrap();
    remove_service(&store, &who, "api");
    publish(&store, &who, None).unwrap();
    for (revision, action) in [
        (1, ployz_store::HistoryAction::Restore),
        (2, ployz_store::HistoryAction::Undo),
    ] {
        let staged = stage_history(&store, &who, revision, action);
        assert_eq!(
            not_set(&staged),
            ["api.STRIPE_KEY", "api.WEBHOOK_SECRET"],
            "#{revision} {action:?}"
        );
        assert_eq!(value(&store, &who, "api.env.PLAIN"), Some(json!("kept")));
        for key in ["STRIPE_KEY", "WEBHOOK_SECRET"] {
            assert_eq!(
                value(&store, &who, &format!("api.env.{key}")),
                Some(json!({ "secret": false })),
                "#{revision} {action:?} {key}"
            );
        }
        discard_to_saved(&store, &who);
    }
}

#[test]
fn a_deploy_leaves_out_a_variable_history_brought_back_not_set() {
    let (store, who) = shop();
    set(&store, &who, "web.env.PLAIN", json!("plain-value"));
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "web.env.TOKEN");
    publish(&store, &who, None).unwrap();
    stage_history(&store, &who, 2, ployz_store::HistoryAction::Undo);
    let id = backend::admit(&store, &who, "production", 1);
    let claimed = store
        .claim(&id, &ployz_store::RunnerId::parse("runner").unwrap())
        .unwrap();
    let input = serde_json::to_value(&claimed.input).unwrap();
    assert!(
        !input.to_string().contains(&backend::ciphertext(1)),
        "{input}"
    );
    let web = input["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|snapshot| snapshot["config"]["privateDns"] == "web")
        .unwrap();
    assert_eq!(web["resolvedEnv"], json!({ "PLAIN": "plain-value" }));
    assert_eq!(
        serde_json::to_value(&claimed.deployment.warnings).unwrap(),
        json!([{ "service": "web", "variable": "TOKEN" }])
    );
}

#[test]
fn undoing_a_service_creation_still_removes_it_with_its_sealed_keys() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    create_service(&store, &who, EnvironmentRef::default(), 31, "billing");
    set(
        &store,
        &who,
        "billing.env.STRIPE_KEY",
        sealed(31, "STRIPE_KEY", 1),
    );
    publish(&store, &who, None).unwrap();
    let staged = stage_history(&store, &who, 2, ployz_store::HistoryAction::Undo);
    assert!(staged.not_set.is_empty());
    let working = working(&store, &who);
    assert!(
        working
            .iter()
            .all(|(path, _)| !path.starts_with("billing.")),
        "{working:?}"
    );
}

#[test]
fn history_keeps_a_sealed_key_added_since() {
    let (store, who) = shop();
    set(&store, &who, "web.env.PLAIN", json!("plain-value"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    for (revision, action) in [
        (1, ployz_store::HistoryAction::Restore),
        (2, ployz_store::HistoryAction::Undo),
    ] {
        let preview = history_preview(&store, &who, revision, action).unwrap();
        assert_eq!(preview.total_count, 0, "#{revision} {action:?}");
        stage_history(&store, &who, revision, action);
        assert_eq!(
            value(&store, &who, "web.env.TOKEN"),
            Some(json!({ "secret": true }))
        );
    }
}

#[test]
fn history_never_brings_back_plain_text_of_a_key_once_sealed() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", json!("plain-value"));
    // A queued deployment keeps its revision's plain text from the scrub.
    backend::admit(&store, &who, "production", 1);
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "web.env.TOKEN");
    publish(&store, &who, None).unwrap();
    let preview = history_preview(&store, &who, 1, ployz_store::HistoryAction::Restore).unwrap();
    assert!(
        !serde_json::to_string(&preview)
            .unwrap()
            .contains("plain-value")
    );
    let staged = stage_history(&store, &who, 1, ployz_store::HistoryAction::Restore);
    assert_eq!(not_set(&staged), ["web.TOKEN"]);
    assert_eq!(
        value(&store, &who, "web.env.TOKEN"),
        Some(json!({ "secret": false }))
    );
}

/// The value each History card shows for `path`, oldest first.
fn history_values(store: &ConfigStore, who: &Actor, path: &str) -> Vec<Option<Value>> {
    let mut history = store
        .read(who, &ployz_store::HistoryQuery::default())
        .unwrap()
        .revisions;
    history.reverse();
    history
        .iter()
        .map(|revision| {
            revision
                .changes
                .iter()
                .flat_map(|change| &change.settings)
                .find(|setting| setting.path == path)
                .map(|setting| setting.after.clone())
        })
        .collect()
}

#[test]
fn sealing_a_value_removes_its_earlier_plain_text_from_history() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", json!("plain-one"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", json!("plain-two"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.OTHER", json!("kept-plain"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    assert_eq!(
        history_values(&store, &who, "web.env.TOKEN"),
        vec![
            None,
            Some(json!({ "secret": true })),
            Some(json!({ "secret": true })),
            None,
            Some(json!({ "secret": true }))
        ]
    );
    let history = serde_json::to_string(
        &store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap(),
    )
    .unwrap();
    assert!(!history.contains("plain-one"));
    assert!(!history.contains("plain-two"));
    assert!(history.contains("kept-plain"));
}

#[test]
fn rotating_a_sealed_value_removes_the_older_ciphertext_from_history() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 2));
    publish(&store, &who, None).unwrap();
    assert_eq!(
        history_values(&store, &who, "web.env.TOKEN"),
        vec![
            None,
            Some(json!({ "secret": true })),
            Some(json!({ "secret": true }))
        ]
    );
}

/// A version that only re-entered a sealed value still names the variable it
/// changed, and shows neither value.
#[test]
fn history_names_a_version_that_only_changed_a_sealed_value() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 2));
    publish(&store, &who, None).unwrap();

    let history = store
        .read(&who, &ployz_store::HistoryQuery::default())
        .unwrap();
    let newest = &history.revisions[0];
    let [change] = newest.changes.as_slice() else {
        panic!("{:?}", newest.changes)
    };
    let [setting] = change.settings.as_slice() else {
        panic!("{:?}", change.settings)
    };
    assert_eq!(setting.path, "web.env.TOKEN");
    assert_eq!(setting.before, json!({ "secret": true }));
    assert_eq!(setting.after, json!({ "secret": true }));
    let text = history_text(&store, &who);
    assert!(!text.contains(&backend::ciphertext(1)) && !text.contains(&backend::ciphertext(2)));
}

#[test]
fn sealing_leaves_the_revision_a_queued_deployment_runs() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    let queued = backend::admit(&store, &who, "production", 1);
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 2));
    publish(&store, &who, None).unwrap();
    let deployed = backend::run(&store, &queued);
    assert!(deployed.to_string().contains(&backend::ciphertext(1)));
    assert!(!deployed.to_string().contains(&backend::ciphertext(2)));
}

fn history_text(store: &ConfigStore, who: &Actor) -> String {
    serde_json::to_string(
        &store
            .read(who, &ployz_store::HistoryQuery::default())
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn sealing_scrubs_a_deployed_revision_once_its_deployment_ends() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", json!("plain-private"));
    let queued = backend::admit(&store, &who, "production", 1);
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    assert!(history_text(&store, &who).contains("plain-private"));
    backend::run(&store, &queued);
    assert!(!history_text(&store, &who).contains("plain-private"));
}

#[test]
fn sealing_scrubs_the_revision_of_a_queued_deployment_another_supersedes() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", json!("plain-private"));
    backend::admit(&store, &who, "production", 1);
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    assert!(history_text(&store, &who).contains("plain-private"));
    backend::admit(&store, &who, "production", 2);
    assert!(!history_text(&store, &who).contains("plain-private"));
}

#[test]
fn sealing_a_renamed_value_scrubs_its_plain_text_under_the_old_name() {
    let (store, who) = shop();
    set(&store, &who, "web.env.OLD", json!("plain-private"));
    publish(&store, &who, None).unwrap();
    rename_variable(&store, &who, "web.env.OLD", "NEW");
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.NEW", sealed(3, "NEW", 1));
    publish(&store, &who, None).unwrap();
    assert!(!history_text(&store, &who).contains("plain-private"));
}

#[test]
fn sealing_scrubs_plain_text_of_a_value_deleted_since() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", json!("plain-private"));
    let queued = backend::admit(&store, &who, "production", 1);
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "web.env.TOKEN");
    publish(&store, &who, None).unwrap();
    assert!(history_text(&store, &who).contains("plain-private"));
    backend::run(&store, &queued);
    assert!(!history_text(&store, &who).contains("plain-private"));
}

#[test]
fn sealing_between_saves_scrubs_plain_text_of_a_value_deleted_since() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", json!("plain-private"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    unset(&store, &who, "web.env.TOKEN");
    publish(&store, &who, None).unwrap();
    assert!(!history_text(&store, &who).contains("plain-private"));
}

#[test]
fn sealing_scrubs_the_revision_of_a_deployment_whose_runner_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    set(&store, &who, "web.env.TOKEN", json!("plain-private"));
    let claimed = backend::admit(&store, &who, "production", 1);
    store
        .claim(&claimed, &ployz_store::RunnerId::parse("runner").unwrap())
        .unwrap();
    execute(&url, "UPDATE config_deployment SET lease = 0");
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    assert!(!history_text(&store, &who).contains("plain-private"));
}

/// Run `sql` on the database at `url`, beside the Store.
fn execute(url: &str, sql: &str) -> usize {
    match url.strip_prefix("sqlite:") {
        Some(path) => rusqlite::Connection::open(path)
            .unwrap()
            .execute(sql, [])
            .unwrap(),
        None => usize::try_from(
            postgres::Client::connect(url, postgres::NoTls)
                .unwrap()
                .execute(sql, &[])
                .unwrap(),
        )
        .unwrap(),
    }
}

#[test]
fn a_later_save_scrubs_what_sealing_left_behind() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", json!("plain-private"));
    backend::admit(&store, &who, "production", 1);
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    backend::admit(&store, &who, "production", 2);
    set(&store, &who, "api.replicas", json!(2));
    publish(&store, &who, None).unwrap();
    assert!(!history_text(&store, &who).contains("plain-private"));
}

fn create_service(store: &ConfigStore, who: &Actor, at: EnvironmentRef, n: u8, name: &str) {
    store
        .write(
            who,
            &CreateService {
                id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-0000000000{n:02}"))
                    .unwrap(),
                environment: at,
                name: ServiceName::parse(name).unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
}

fn route(store: &ConfigStore, who: &Actor, at: EnvironmentRef, service: &str, hostname: &str) {
    store
        .write_trusted(
            who,
            &AddDomain {
                environment: at,
                service: ServiceName::parse(service).unwrap(),
                hostname: Some(Hostname::parse(hostname).unwrap()),
                prefix: None,
                port: None,
            },
            &Trusted::default(),
        )
        .unwrap();
}

fn unroute(store: &ConfigStore, who: &Actor, hostname: &str) {
    store
        .write_trusted(
            who,
            &RemoveDomain {
                environment: EnvironmentRef::default(),
                domain: hostname.into(),
            },
            &Trusted::default(),
        )
        .unwrap();
}

fn create_volume(store: &ConfigStore, who: &Actor, n: u8) {
    store
        .write(
            who,
            &CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse(format!("00000000-0000-4000-8000-0000000000{n:02}")).unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("data").unwrap(),
                mounts: Vec::new(),
            },
        )
        .unwrap();
}

fn refused_undo(store: &ConfigStore, who: &Actor, revision: u64) -> String {
    let error =
        history_preview(store, who, revision, ployz_store::HistoryAction::Undo).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
    error.message
}

#[test]
fn history_refuses_a_service_name_taken_since() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    remove_service(&store, &who, "api");
    publish(&store, &who, None).unwrap();
    create_service(&store, &who, EnvironmentRef::default(), 31, "api");
    publish(&store, &who, None).unwrap();
    assert_eq!(
        refused_undo(&store, &who, 2),
        "api exists. Rename it first."
    );
}

#[test]
fn history_names_a_private_dns_name_taken_since() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    remove_service(&store, &who, "api");
    publish(&store, &who, None).unwrap();
    create_service(&store, &who, EnvironmentRef::default(), 31, "api");
    store
        .write(
            &who,
            &ployz_store::RenameService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("api").unwrap(),
                name: ServiceName::parse("api-two").unwrap(),
            },
        )
        .unwrap();
    publish(&store, &who, None).unwrap();
    assert_eq!(
        refused_undo(&store, &who, 2),
        "api is another Service's private DNS name. Change it first."
    );
}

#[test]
fn history_refuses_a_volume_name_taken_since() {
    let (store, who) = shop();
    create_volume(&store, &who, 31);
    publish(&store, &who, None).unwrap();
    store
        .write(
            &who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    publish(&store, &who, None).unwrap();
    create_volume(&store, &who, 32);
    publish(&store, &who, None).unwrap();
    assert_eq!(
        refused_undo(&store, &who, 2),
        "data exists. Rename it first."
    );
}

#[test]
fn history_refuses_a_hostname_another_service_of_the_environment_took_since() {
    let (store, who) = shop();
    route(
        &store,
        &who,
        EnvironmentRef::default(),
        "web",
        "a.example.com",
    );
    publish(&store, &who, None).unwrap();
    unroute(&store, &who, "a.example.com");
    publish(&store, &who, None).unwrap();
    route(
        &store,
        &who,
        EnvironmentRef::default(),
        "api",
        "a.example.com",
    );
    publish(&store, &who, None).unwrap();
    assert_eq!(
        refused_undo(&store, &who, 2),
        "Another Service already has a.example.com: api in shop/production"
    );
}

#[test]
fn history_refuses_a_generated_domain_another_service_took_since() {
    let (store, who) = shop();
    let generate = |service: &str| {
        store
            .write_trusted(
                &who,
                &AddDomain {
                    environment: EnvironmentRef::default(),
                    service: ServiceName::parse(service).unwrap(),
                    hostname: None,
                    prefix: Some(ployz_core::DomainPrefix::parse("shop").unwrap()),
                    port: None,
                },
                &Trusted::default(),
            )
            .unwrap();
    };
    generate("web");
    publish(&store, &who, None).unwrap();
    unroute(&store, &who, "shop");
    publish(&store, &who, None).unwrap();
    generate("api");
    publish(&store, &who, None).unwrap();
    assert_eq!(
        refused_undo(&store, &who, 2),
        "Another generated domain of this Organization is shop"
    );
}

#[test]
fn history_refuses_a_hostname_another_environment_took_since() {
    let (store, who) = shop();
    route(
        &store,
        &who,
        EnvironmentRef::default(),
        "web",
        "a.example.com",
    );
    publish(&store, &who, None).unwrap();
    unroute(&store, &who, "a.example.com");
    publish(&store, &who, None).unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateEnvironment {
                id: EnvironmentId::parse("00000000-0000-4000-8000-000000000030").unwrap(),
                project: None,
                name: ployz_store::EnvironmentName::parse("staging").unwrap(),
            },
        )
        .unwrap();
    let staging = || EnvironmentRef {
        project: None,
        environment: Some(ployz_store::EnvironmentName::parse("staging").unwrap()),
    };
    create_service(&store, &who, staging(), 31, "web");
    route(&store, &who, staging(), "web", "a.example.com");
    assert_eq!(
        refused_undo(&store, &who, 2),
        "Another Service already has a.example.com: web in shop/staging"
    );
}

#[test]
fn history_never_brings_back_plain_text_of_a_key_sealed_only_between_saves() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", json!("plain-value"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    unset(&store, &who, "web.env.TOKEN");
    publish(&store, &who, None).unwrap();
    for change in [None, Some("web.env.TOKEN")] {
        let query = ployz_store::HistoryPreviewQuery {
            environment: EnvironmentRef::default(),
            revision: Revision(if change.is_some() { 2 } else { 1 }),
            action: if change.is_some() {
                ployz_store::HistoryAction::Undo
            } else {
                ployz_store::HistoryAction::Restore
            },
            change: change.map(Into::into),
        };
        // Refused, or with the plain text scrubbed: never brought back.
        match store.read(&who, &query) {
            Err(error) => {
                assert_eq!(error.code, RpcErrorCode::Conflict, "{change:?}");
                assert_eq!(
                    error.message, "TOKEN was sealed. Set it again.",
                    "{change:?}"
                );
            }
            Ok(preview) => assert!(
                !serde_json::to_string(&preview)
                    .unwrap()
                    .contains("plain-value"),
                "{change:?}"
            ),
        }
    }
    assert_eq!(value(&store, &who, "web.env.TOKEN"), None);
}

#[test]
fn history_names_the_save_that_deleted_what_an_undo_changes() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.replicas", json!(2));
    publish(&store, &who, None).unwrap();
    remove_service(&store, &who, "api");
    assert_eq!(refused_undo(&store, &who, 2), "api was deleted");
    publish(&store, &who, None).unwrap();
    assert_eq!(refused_undo(&store, &who, 2), "api was deleted in #3");
}

#[test]
fn history_names_the_save_that_deleted_a_variable_an_undo_changes() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.env.FOO", json!("a"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.env.FOO", json!("b"));
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "api.env.FOO");
    publish(&store, &who, None).unwrap();
    assert_eq!(refused_undo(&store, &who, 3), "FOO was deleted in #4");
}

#[test]
fn history_names_the_save_that_last_deleted_what_an_undo_changes() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.env.FOO", json!("a"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.env.FOO", json!("b"));
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "api.env.FOO");
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.env.FOO", json!("c"));
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "api.env.FOO");
    publish(&store, &who, None).unwrap();
    assert_eq!(refused_undo(&store, &who, 3), "FOO was deleted in #6");
}

#[test]
fn history_follows_a_renamed_variable_to_the_save_that_deleted_it() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.env.FOO", json!("a"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.env.FOO", json!("b"));
    publish(&store, &who, None).unwrap();
    rename_variable(&store, &who, "api.env.FOO", "BAR");
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "api.env.BAR");
    publish(&store, &who, None).unwrap();
    assert_eq!(refused_undo(&store, &who, 3), "BAR was deleted in #5");
}

#[test]
fn history_names_a_renamed_variable_by_its_new_name_not_as_deleted() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.env.FOO", json!("a"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.env.FOO", json!("b"));
    publish(&store, &who, None).unwrap();
    rename_variable(&store, &who, "api.env.FOO", "BAR");
    publish(&store, &who, None).unwrap();
    assert_eq!(
        refused_undo(&store, &who, 3),
        "FOO was renamed to BAR in #4"
    );
}

#[test]
fn history_names_an_unsaved_deletion_over_an_earlier_rename() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.replicas", json!(2));
    publish(&store, &who, None).unwrap();
    store
        .write(
            &who,
            &ployz_store::RenameService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("api").unwrap(),
                name: ServiceName::parse("backend").unwrap(),
            },
        )
        .unwrap();
    publish(&store, &who, None).unwrap();
    remove_service(&store, &who, "backend");
    assert_eq!(refused_undo(&store, &who, 2), "backend was deleted");
}

#[test]
fn history_follows_a_recreated_variable_through_its_rename() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.env.FOO", json!("a"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.env.FOO", json!("b"));
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "api.env.FOO");
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.env.FOO", json!("c"));
    publish(&store, &who, None).unwrap();
    rename_variable(&store, &who, "api.env.FOO", "BAR");
    publish(&store, &who, None).unwrap();
    assert_eq!(
        refused_undo(&store, &who, 3),
        "FOO was renamed to BAR in #6"
    );
}

#[test]
fn history_names_what_was_deleted_by_the_name_it_had_then() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "api.replicas", json!(2));
    publish(&store, &who, None).unwrap();
    store
        .write(
            &who,
            &ployz_store::RenameService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("api").unwrap(),
                name: ServiceName::parse("backend").unwrap(),
            },
        )
        .unwrap();
    publish(&store, &who, None).unwrap();
    remove_service(&store, &who, "backend");
    publish(&store, &who, None).unwrap();
    assert_eq!(refused_undo(&store, &who, 2), "backend was deleted in #4");
}

#[test]
fn history_checks_the_rules_like_a_fresh_edit() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    create_service(&store, &who, EnvironmentRef::default(), 31, "worker");
    publish(&store, &who, None).unwrap();
    store
        .write(
            &who,
            &ployz_store::SetBranchSetup {
                environment: EnvironmentRef::default(),
                setup: vec![ployz_store::SetupCommand {
                    service: ServiceName::parse("worker").unwrap(),
                    command: "migrate".into(),
                }],
            },
        )
        .unwrap();
    let error = history_preview(&store, &who, 1, ployz_store::HistoryAction::Restore).unwrap_err();
    assert_eq!(
        error.message,
        "New Branches run a Setup Command in worker; set the Setup Commands without it first"
    );
}

#[test]
fn history_undo_validates_dependent_variables_after_the_complete_inverse() {
    let (store, who) = shop();
    set(&store, &who, "web.env.SOURCE", json!("old"));
    set(&store, &who, "api.env.USE", json!("${{ web.SOURCE }}"));
    publish(&store, &who, None).unwrap();
    store
        .write(
            &who,
            &Edit {
                changes: vec![
                    Change::Unset {
                        path: SettingPath::parse("api.env.USE").unwrap(),
                    },
                    Change::Unset {
                        path: SettingPath::parse("web.env.SOURCE").unwrap(),
                    },
                ],
                environment: EnvironmentRef::default(),
                expect: None,
            },
        )
        .unwrap();
    publish(&store, &who, None).unwrap();
    stage_history(&store, &who, 2, ployz_store::HistoryAction::Undo);
    assert_eq!(value(&store, &who, "web.env.SOURCE"), Some(json!("old")));
    assert_eq!(
        value(&store, &who, "api.env.USE"),
        Some(json!("${{ web.SOURCE }}"))
    );
}

#[test]
fn history_undo_restores_exact_config_files_and_preserves_later_mounts() {
    let (store, who) = shop();
    let config = ployz_core::ConfigName::parse("sentry").unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                environment: EnvironmentRef::default(),
                id: ployz_store::ConfigId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                name: config.clone(),
                mounts: vec![ployz_store::ConfigMountAt {
                    service: ServiceName::parse("web").unwrap(),
                    dir: "/etc/sentry".into(),
                }],
            },
        )
        .unwrap();
    let put = |mode, uid| {
        store
            .write(
                &who,
                &ployz_store::PutConfigFile {
                    environment: EnvironmentRef::default(),
                    config: config.clone(),
                    file: ployz_core::ConfigFileName::parse("conf.d/site.conf").unwrap(),
                    content: "same content".into(),
                    mode: Some(mode),
                    uid: Some(uid),
                    gid: Some(20),
                },
            )
            .unwrap()
    };
    put(ployz_core::config::FileMode::parse("0444").unwrap(), 0);
    let query = ployz_store::ConfigItemQuery {
        environment: EnvironmentRef::default(),
        config: config.clone().into(),
    };
    let before = store.read(&who, &query).unwrap();
    publish(&store, &who, None).unwrap();
    put(ployz_core::config::FileMode::parse("0555").unwrap(), 1000);
    publish(&store, &who, None).unwrap();
    store
        .write(
            &who,
            &ployz_store::AttachConfig {
                environment: EnvironmentRef::default(),
                config: config.clone(),
                service: ServiceName::parse("api").unwrap(),
                dir: "/later".into(),
            },
        )
        .unwrap();
    let preview = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                revision: Revision(2),
                action: ployz_store::HistoryAction::Undo,
                change: None,
                environment: EnvironmentRef::default(),
            },
        )
        .unwrap();
    let config_change = preview
        .changes
        .iter()
        .find(|change| change.node.node_type == ployz_core::config::EnvironmentNodeType::Config)
        .unwrap();
    assert_eq!(
        config_change
            .restarts
            .iter()
            .map(ServiceName::as_str)
            .collect::<Vec<_>>(),
        ["web", "api"]
    );
    let file = config_change.settings.first().unwrap();
    assert!(file.path.ends_with(".files.conf.d/site.conf"));
    assert_eq!(
        file.after,
        json!({ "content": "same content", "mode": "0444", "uid": 0, "gid": 20 })
    );
    stage_history(&store, &who, 2, ployz_store::HistoryAction::Undo);
    let restored = store.read(&who, &query).unwrap();
    assert_eq!(restored.contents, before.contents);
    assert_eq!(restored.config.config.files, before.config.config.files);
    assert_eq!(restored.config.config.id, before.config.config.id);
    assert_eq!(restored.config.mounts.len(), 2);
    assert_eq!(
        value(&store, &who, "api.configs.sentry"),
        Some(json!("/later"))
    );
}

#[test]
fn export_only_draft_is_reviewable_and_undo_preserves_a_later_export() {
    let (store, who) = shop();
    set(&store, &who, "web.env.KEY", json!("old"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.KEY", json!("new"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.KEY.exported", json!(true));
    assert_eq!(diff(&store, &who).draft_count, 1);
    assert_eq!(
        diff(&store, &who)
            .draft_changes
            .first()
            .unwrap()
            .settings
            .first()
            .unwrap()
            .path,
        "web.env.KEY.exported"
    );
    let preview = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: EnvironmentRef::default(),
                revision: Revision(2),
                action: ployz_store::HistoryAction::Undo,
                change: None,
            },
        )
        .unwrap();
    assert!(preview.overwritten.is_empty());
    stage_history(&store, &who, 2, ployz_store::HistoryAction::Undo);
    assert_eq!(value(&store, &who, "web.env.KEY"), Some(json!("old")));
    assert_eq!(
        value(&store, &who, "web.env.KEY.exported"),
        Some(json!(true))
    );
}

#[test]
fn saved_discard_cannot_downgrade_a_sealed_variable() {
    let (store, who) = shop();
    set(&store, &who, "web.env.KEY", json!("public"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.KEY", sealed(3, "KEY", 1));
    let result = store.write(
        &who,
        &Discard {
            target: Some(ployz_store::DiscardTarget::Saved),
            path: Some(SettingPath::parse("web.env.KEY").unwrap()),
            ..Discard::default()
        },
    );
    assert_eq!(result.unwrap_err().code, RpcErrorCode::Conflict);
    assert_eq!(
        value(&store, &who, "web.env.KEY"),
        Some(json!({ "secret": true }))
    );
}

#[test]
fn history_undo_volume_mount_named_name_and_shared_writes() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &CreateVolume {
                id: VolumeId::parse("00000000-0000-4000-8000-000000000007").unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("name").unwrap(),
                storage: ployz_core::config::VolumeKind::Docker {},
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                mounts: vec![Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: "/old".into(),
                }],
            },
        )
        .unwrap();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.mounts.name", json!("/new"));
    store
        .write(
            &who,
            &ployz_store::SetVolumeSharedWrites {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("name").unwrap(),
                shared_writes: true,
            },
        )
        .unwrap();
    publish(&store, &who, None).unwrap();
    let preview = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: EnvironmentRef::default(),
                revision: Revision(1),
                action: ployz_store::HistoryAction::Restore,
                change: None,
            },
        )
        .unwrap();
    assert!(
        !preview
            .changes
            .iter()
            .flat_map(|node| &node.settings)
            .any(|row| row.path == "volumes.name.sharedWrites")
    );
    stage_history(&store, &who, 2, ployz_store::HistoryAction::Undo);
    assert_eq!(value(&store, &who, "web.mounts.name"), Some(json!("/old")));
    assert!(
        !store
            .read(&who, &ployz_store::VolumesQuery::default())
            .unwrap()
            .volumes
            .first()
            .unwrap()
            .volume
            .shared_writes
    );
}

#[test]
fn history_undo_custom_and_generated_domains_uses_typed_settings() {
    for hostname in [
        None,
        Some(ployz_store::Hostname::parse("history.example.com").unwrap()),
    ] {
        let (store, who) = shop();
        publish(&store, &who, None).unwrap();
        store
            .write_trusted(
                &who,
                &ployz_store::AddDomain {
                    environment: EnvironmentRef::default(),
                    service: ServiceName::parse("web").unwrap(),
                    hostname,
                    prefix: None,
                    port: None,
                },
                &Trusted::default(),
            )
            .unwrap();
        publish(&store, &who, None).unwrap();
        stage_history(&store, &who, 2, ployz_store::HistoryAction::Undo);
        assert!(
            store
                .read(&who, &ployz_store::DomainsQuery::default())
                .unwrap()
                .domains
                .is_empty()
        );
        assert_eq!(diff(&store, &who).saved, Some(Revision(2)));
    }
}

#[test]
fn undo_export_only_revision_preserves_later_value_and_review_discards_both_fields() {
    let (store, who) = shop();
    set(&store, &who, "web.env.KEY", json!("old"));
    backend::deploy(&store, &who, "production", 1);
    set(&store, &who, "web.env.KEY.exported", json!(true));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.KEY", json!("later"));
    let preview = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: EnvironmentRef::default(),
                revision: Revision(2),
                action: ployz_store::HistoryAction::Undo,
                change: None,
            },
        )
        .unwrap();
    assert!(preview.overwritten.is_empty());
    assert_eq!(preview.total_count, 1);
    stage_history(&store, &who, 2, ployz_store::HistoryAction::Undo);
    assert_eq!(value(&store, &who, "web.env.KEY"), Some(json!("later")));
    assert_eq!(
        value(&store, &who, "web.env.KEY.exported"),
        Some(json!(false))
    );
    store
        .write(
            &who,
            &Discard {
                target: Some(ployz_store::DiscardTarget::Review),
                ..Discard::default()
            },
        )
        .unwrap();
    assert_eq!(value(&store, &who, "web.env.KEY"), Some(json!("old")));
    assert_eq!(
        value(&store, &who, "web.env.KEY.exported"),
        Some(json!(true))
    );
    assert_eq!(diff(&store, &who).saved, Some(Revision(2)));
}

#[test]
fn undo_domain_revision_preserves_later_unrelated_route_without_confirmation() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    let add = |hostname| {
        store
            .write_trusted(
                &who,
                &ployz_store::AddDomain {
                    environment: EnvironmentRef::default(),
                    service: ServiceName::parse("web").unwrap(),
                    hostname: Some(ployz_store::Hostname::parse(hostname).unwrap()),
                    prefix: None,
                    port: None,
                },
                &Trusted::default(),
            )
            .unwrap()
    };
    add("first.example.com");
    publish(&store, &who, None).unwrap();
    add("later.example.com");
    let preview = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: EnvironmentRef::default(),
                revision: Revision(2),
                action: ployz_store::HistoryAction::Undo,
                change: None,
            },
        )
        .unwrap();
    assert!(preview.overwritten.is_empty());
    assert_eq!(preview.total_count, 1);
    store
        .write(
            &who,
            &ployz_store::StageHistory {
                environment: EnvironmentRef::default(),
                revision: Revision(2),
                action: preview.action,
                change: None,
                version: preview.version,
                accept_overwrite: false,
            },
        )
        .unwrap();
    let domains = store
        .read(&who, &ployz_store::DomainsQuery::default())
        .unwrap();
    assert_eq!(domains.domains.len(), 1);
    assert_eq!(
        domains.domains.first().unwrap().domain.shown(),
        "later.example.com"
    );
}

#[test]
fn review_discard_restores_custom_and_generated_domain_changes() {
    for hostname in [
        None,
        Some(ployz_store::Hostname::parse("discard.example.com").unwrap()),
    ] {
        let (store, who) = shop();
        backend::deploy(&store, &who, "production", 1);
        store
            .write_trusted(
                &who,
                &ployz_store::AddDomain {
                    environment: EnvironmentRef::default(),
                    service: ServiceName::parse("web").unwrap(),
                    hostname,
                    prefix: None,
                    port: None,
                },
                &Trusted::default(),
            )
            .unwrap();
        let before = diff(&store, &who);
        assert_eq!(before.total_count, 1);
        store
            .write(
                &who,
                &Discard {
                    target: Some(ployz_store::DiscardTarget::Review),
                    version: Some(before.version),
                    ..Discard::default()
                },
            )
            .unwrap();
        assert!(
            store
                .read(&who, &ployz_store::DomainsQuery::default())
                .unwrap()
                .domains
                .is_empty()
        );
        assert_eq!(diff(&store, &who).saved, Some(Revision(1)));
    }
}

#[test]
fn template_metadata_is_reviewed_and_undone_without_deploying() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "web.template",
        json!({ "id": "redis", "version": 1 }),
    );
    publish(&store, &who, None).unwrap();
    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("web.template").unwrap(),
                }],
            },
        )
        .unwrap();
    assert_eq!(diff(&store, &who).draft_count, 1);
    publish(&store, &who, None).unwrap();
    stage_history(&store, &who, 2, ployz_store::HistoryAction::Undo);
    assert_eq!(
        value(&store, &who, "web.template"),
        Some(json!({ "id": "redis", "version": 1 }))
    );
    store
        .write(
            &who,
            &Discard {
                target: Some(ployz_store::DiscardTarget::Saved),
                path: Some(SettingPath::parse("web.template").unwrap()),
                ..Discard::default()
            },
        )
        .unwrap();
    assert_eq!(value(&store, &who, "web.template"), Some(Value::Null));
}

#[test]
fn restoring_a_deleted_draft_variable_requires_explicit_overwrite_consent() {
    let (store, who) = shop();
    set(&store, &who, "web.env.KEY", json!("saved"));
    publish(&store, &who, None).unwrap();
    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("web.env.KEY").unwrap(),
                }],
            },
        )
        .unwrap();
    let preview = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: EnvironmentRef::default(),
                revision: Revision(1),
                action: ployz_store::HistoryAction::Restore,
                change: None,
            },
        )
        .unwrap();
    assert_eq!(preview.overwritten, ["web.env.KEY"]);
    assert_eq!(
        store
            .write(
                &who,
                &ployz_store::StageHistory {
                    environment: EnvironmentRef::default(),
                    revision: Revision(1),
                    action: preview.action,
                    change: None,
                    version: preview.version,
                    accept_overwrite: false,
                }
            )
            .unwrap_err()
            .code,
        RpcErrorCode::ConfirmationRequired
    );
    assert_eq!(value(&store, &who, "web.env.KEY"), None);
}

#[test]
fn undo_variable_creation_requires_consent_for_later_export_and_value_edits() {
    for (field, later, overwritten) in [
        ("web.env.KEY.exported", json!(true), "web.env.KEY.exported"),
        ("web.env.KEY", json!("later"), "web.env.KEY"),
    ] {
        let (store, who) = shop();
        publish(&store, &who, None).unwrap();
        set(&store, &who, "web.env.KEY", json!("saved"));
        publish(&store, &who, None).unwrap();
        set(&store, &who, field, later.clone());
        let preview = store
            .read(
                &who,
                &ployz_store::HistoryPreviewQuery {
                    environment: EnvironmentRef::default(),
                    revision: Revision(2),
                    action: ployz_store::HistoryAction::Undo,
                    change: None,
                },
            )
            .unwrap();
        assert_eq!(preview.overwritten, [overwritten]);
        let command = ployz_store::StageHistory {
            environment: EnvironmentRef::default(),
            revision: Revision(2),
            action: preview.action,
            change: None,
            version: preview.version,
            accept_overwrite: false,
        };
        assert_eq!(
            store.write(&who, &command).unwrap_err().code,
            RpcErrorCode::ConfirmationRequired
        );
        assert_eq!(value(&store, &who, field), Some(later));
        store
            .write(
                &who,
                &ployz_store::StageHistory {
                    accept_overwrite: true,
                    ..command
                },
            )
            .unwrap();
        assert_eq!(value(&store, &who, "web.env.KEY"), None);
        assert_eq!(diff(&store, &who).saved, Some(Revision(2)));
    }
}

struct LegacyWorking {
    _dir: tempfile::TempDir,
    store: ConfigStore,
    who: Actor,
    sql: rusqlite::Connection,
}

impl LegacyWorking {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        let (store, who) = shop_in(
            ConfigStore::open(&format!("sqlite:{}", path.display()), backend::key()).unwrap(),
        );
        Self {
            _dir: dir,
            store,
            who,
            sql: rusqlite::Connection::open(path).unwrap(),
        }
    }

    fn intent(&self) -> ployz_core::config::SavedEnvironmentIntent {
        let text: String = self
            .sql
            .query_row(
                "SELECT working FROM config_environment WHERE id = ?1",
                [ENVIRONMENT],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn variable(&self, change: impl FnOnce(&mut ployz_core::config::SavedVariableIntent)) {
        let mut intent = self.intent();
        let variable = intent
            .services
            .iter_mut()
            .find(|service| service.slug == "web")
            .unwrap()
            .variables
            .iter_mut()
            .find(|variable| variable.key == "KEY")
            .unwrap();
        change(variable);
        self.sql.execute("UPDATE config_environment SET working = ?1, working_revision = working_revision + 1 WHERE id = ?2",
            rusqlite::params![serde_json::to_string(&intent).unwrap(), ENVIRONMENT]).unwrap();
    }

    fn key(&self) -> ployz_core::config::SavedVariableIntent {
        self.intent()
            .services
            .into_iter()
            .find(|service| service.slug == "web")
            .unwrap()
            .variables
            .into_iter()
            .find(|variable| variable.key == "KEY")
            .unwrap()
    }
}

#[test]
fn legacy_metadata_inverse_preserves_later_value_and_export_without_consent() {
    let fixture = LegacyWorking::new();
    let (store, who) = (&fixture.store, &fixture.who);
    set(store, who, "web.env.KEY", json!("saved"));
    fixture.variable(|variable| variable.description = Some("original description".into()));
    let original = fixture.key();
    publish(store, who, None).unwrap();
    fixture.variable(|variable| {
        variable.id = "00000000-0000-4000-8000-000000000091".into();
        variable.description = Some("new description".into());
    });
    publish(store, who, None).unwrap();
    set(
        store,
        who,
        "web.env.KEY",
        json!({ "value": "later", "exported": true }),
    );
    let preview = store
        .read(
            who,
            &ployz_store::HistoryPreviewQuery {
                revision: Revision(2),
                action: ployz_store::HistoryAction::Undo,
                change: None,
                environment: EnvironmentRef::default(),
            },
        )
        .unwrap();
    assert!(preview.overwritten.is_empty());
    store
        .write(
            who,
            &ployz_store::StageHistory {
                environment: EnvironmentRef::default(),
                revision: Revision(2),
                action: preview.action,
                change: None,
                version: preview.version,
                accept_overwrite: false,
            },
        )
        .unwrap();
    let restored = fixture.key();
    assert_eq!(restored.id, original.id);
    assert_eq!(restored.description, original.description);
    assert_eq!(
        restored.value,
        ployz_core::config::SavedVariableValue::Literal {
            value: "later".into()
        }
    );
    assert!(restored.exported);
}

#[test]
fn legacy_id_only_saved_and_review_discard_restore_the_visible_field() {
    for target in [
        ployz_store::DiscardTarget::Saved,
        ployz_store::DiscardTarget::Review,
    ] {
        let fixture = LegacyWorking::new();
        let (store, who) = (&fixture.store, &fixture.who);
        set(store, who, "web.env.KEY", json!("saved"));
        backend::deploy(store, who, "production", 1);
        let original = fixture.key();
        fixture.variable(|variable| variable.id = "00000000-0000-4000-8000-000000000091".into());
        let review = diff(store, who);
        assert!(
            review
                .draft_changes
                .iter()
                .flat_map(|node| &node.settings)
                .any(|row| row.path == "web.env.KEY" && row.can_restore)
        );
        let result = store
            .write(
                who,
                &Discard {
                    target: Some(target),
                    path: (target == ployz_store::DiscardTarget::Saved)
                        .then(|| SettingPath::parse("web.env.KEY").unwrap()),
                    version: Some(review.version),
                    ..Discard::default()
                },
            )
            .unwrap();
        assert!(result.changed);
        assert_eq!(fixture.key(), original);
        assert_eq!(diff(store, who).draft_count, 0);
    }
}

#[test]
fn legacy_description_discard_is_individual_and_not_an_editor() {
    let fixture = LegacyWorking::new();
    let (store, who) = (&fixture.store, &fixture.who);
    set(store, who, "web.env.KEY", json!("saved"));
    fixture.variable(|variable| variable.description = Some("saved description".into()));
    publish(store, who, None).unwrap();
    set(store, who, "web.env.KEY", json!("later"));
    fixture.variable(|variable| variable.description = Some("draft description".into()));
    let draft = fixture.key();
    let path = SettingPath::parse("web.env.KEY.description").unwrap();
    let rows = diff(store, who).draft_changes;
    assert!(
        rows.iter()
            .flat_map(|node| &node.settings)
            .any(|row| row.path == path.to_string() && row.can_restore)
    );
    assert_eq!(
        store
            .write(
                who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![Change::Set {
                        path: path.clone(),
                        value: json!("new description")
                    }]
                }
            )
            .unwrap_err()
            .code,
        RpcErrorCode::InvalidArgument
    );
    assert_eq!(fixture.key(), draft);
    store
        .write(
            who,
            &Discard {
                target: Some(ployz_store::DiscardTarget::Saved),
                path: Some(path.clone()),
                ..Discard::default()
            },
        )
        .unwrap();
    let restored = fixture.key();
    assert_eq!(restored.description.as_deref(), Some("saved description"));
    assert_eq!(restored.value, draft.value);
    assert_eq!(restored.id, draft.id);
    assert_eq!(
        store
            .read(
                who,
                &EnvironmentQuery {
                    path: Some(path),
                    ..EnvironmentQuery::default()
                }
            )
            .unwrap()
            .settings
            .first()
            .unwrap()
            .value,
        json!("saved description")
    );
}

#[test]
fn undo_variable_creation_requires_consent_for_legacy_description_edit() {
    let fixture = LegacyWorking::new();
    let (store, who) = (&fixture.store, &fixture.who);
    publish(store, who, None).unwrap();
    set(store, who, "web.env.KEY", json!("saved"));
    publish(store, who, None).unwrap();
    fixture.variable(|variable| variable.description = Some("draft description".into()));
    let draft = fixture.intent();
    let preview = store
        .read(
            who,
            &ployz_store::HistoryPreviewQuery {
                revision: Revision(2),
                action: ployz_store::HistoryAction::Undo,
                change: None,
                environment: EnvironmentRef::default(),
            },
        )
        .unwrap();
    assert_eq!(preview.overwritten, ["web.env.KEY.description"]);
    let result = store.write(
        who,
        &ployz_store::StageHistory {
            environment: EnvironmentRef::default(),
            revision: Revision(2),
            action: preview.action,
            change: None,
            version: preview.version,
            accept_overwrite: false,
        },
    );
    assert_eq!(result.unwrap_err().code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(fixture.intent(), draft);
}

fn git_edit(store: &ConfigStore, who: &Actor, path: &str, next: Value) {
    let trusted = Trusted {
        repositories: vec![ployz_store::AuthorizedRepository {
            repository: backend::repo_name("acme/web"),
            repository_id: backend::repo_id(11),
            access: ployz_core::config::ServiceGitAccess::Public,
            default_branch: backend::git_branch("main"),
            branches: vec![backend::git_branch("dev")],
        }],
        ..Trusted::default()
    };
    let mut changes = Vec::new();
    if path == "web.repository"
        && value(store, who, "web.image").is_some_and(|value| !value.is_null())
    {
        changes.push(Change::Unset {
            path: SettingPath::parse("web.image").unwrap(),
        });
    }
    changes.push(Change::Set {
        path: SettingPath::parse(path).unwrap(),
        value: next,
    });
    store
        .write_trusted(
            who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes,
            },
            &trusted,
        )
        .unwrap();
}

#[test]
fn undo_git_selection_preserves_a_later_branch_without_consent() {
    let (store, who) = shop();
    git_edit(&store, &who, "web.repository", json!("acme/web"));
    set(&store, &who, "web.rootDir", json!("/old"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.rootDir", json!("/new"));
    publish(&store, &who, None).unwrap();
    git_edit(&store, &who, "web.branch", json!("dev"));
    let preview = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: EnvironmentRef::default(),
                revision: Revision(2),
                action: ployz_store::HistoryAction::Undo,
                change: None,
            },
        )
        .unwrap();
    assert!(preview.overwritten.is_empty());
    assert_eq!(preview.total_count, 1);
    store
        .write(
            &who,
            &ployz_store::StageHistory {
                environment: EnvironmentRef::default(),
                revision: Revision(2),
                action: preview.action,
                change: None,
                version: preview.version,
                accept_overwrite: false,
            },
        )
        .unwrap();
    assert_eq!(value(&store, &who, "web.rootDir"), Some(json!("/old")));
    assert_eq!(value(&store, &who, "web.branch"), Some(json!("dev")));
}

#[test]
fn undo_source_kind_requires_consent_for_a_later_branch_edit() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    git_edit(&store, &who, "web.repository", json!("acme/web"));
    publish(&store, &who, None).unwrap();
    git_edit(&store, &who, "web.branch", json!("dev"));
    let preview = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: EnvironmentRef::default(),
                revision: Revision(2),
                action: ployz_store::HistoryAction::Undo,
                change: None,
            },
        )
        .unwrap();
    assert_eq!(preview.overwritten, ["web.branch"]);
    let source = preview
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|row| row.path == "web.source")
        .unwrap();
    assert_eq!(
        source.before.get("branch"),
        Some(&json!({ "type": "connected", "name": "dev" }))
    );
    let command = ployz_store::StageHistory {
        environment: EnvironmentRef::default(),
        revision: Revision(2),
        action: preview.action,
        change: None,
        version: preview.version,
        accept_overwrite: false,
    };
    assert_eq!(
        store.write(&who, &command).unwrap_err().code,
        RpcErrorCode::ConfirmationRequired
    );
    assert_eq!(value(&store, &who, "web.branch"), Some(json!("dev")));
    store
        .write(
            &who,
            &ployz_store::StageHistory {
                accept_overwrite: true,
                ..command
            },
        )
        .unwrap();
    assert_eq!(value(&store, &who, "web.image"), Some(json!("nginx:1")));
}

#[test]
fn discarding_new_variable_export_preserves_its_value_and_owner() {
    for target in [
        ployz_store::DiscardTarget::Head,
        ployz_store::DiscardTarget::Saved,
    ] {
        for new_service in [false, true] {
            let (store, who) = shop();
            backend::deploy(&store, &who, "production", 1);
            let service = if new_service {
                store
                    .write(
                        &who,
                        &CreateService {
                            id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000009")
                                .unwrap(),
                            environment: EnvironmentRef::default(),
                            name: ServiceName::parse("worker").unwrap(),
                            image: Some("alpine:3".into()),
                            template: None,
                        },
                    )
                    .unwrap();
                "worker"
            } else {
                "web"
            };
            let path = format!("{service}.env.NEW");
            set(&store, &who, &path, json!("keep draft value"));
            set(&store, &who, &format!("{path}.exported"), json!(true));
            let history = store
                .read(&who, &ployz_store::HistoryQuery::default())
                .unwrap()
                .revisions;
            let result = store
                .write(
                    &who,
                    &Discard {
                        target: Some(target),
                        path: Some(SettingPath::parse(&format!("{path}.exported")).unwrap()),
                        version: Some(diff(&store, &who).version),
                        ..Discard::default()
                    },
                )
                .unwrap();
            assert!(result.changed);
            assert_eq!(
                value(&store, &who, &format!("{path}.exported")),
                Some(json!(false))
            );
            assert_eq!(value(&store, &who, &path), Some(json!("keep draft value")));
            assert!(value(&store, &who, &format!("{service}.image")).is_some());
            assert_eq!(
                store
                    .read(&who, &ployz_store::HistoryQuery::default())
                    .unwrap()
                    .revisions,
                history
            );
            let result = store
                .write(
                    &who,
                    &Discard {
                        target: Some(target),
                        path: Some(SettingPath::parse(&path).unwrap()),
                        version: Some(diff(&store, &who).version),
                        ..Discard::default()
                    },
                )
                .unwrap();
            assert!(result.changed);
            assert_eq!(value(&store, &who, &path), None);
            assert!(value(&store, &who, &format!("{service}.image")).is_some());
        }
    }
}

#[test]
fn discarding_new_legacy_description_preserves_other_variable_fields() {
    for target in [
        ployz_store::DiscardTarget::Head,
        ployz_store::DiscardTarget::Saved,
    ] {
        let fixture = LegacyWorking::new();
        let (store, who) = (&fixture.store, &fixture.who);
        backend::deploy(store, who, "production", 1);
        set(
            store,
            who,
            "web.env.KEY",
            json!({ "value": "keep", "exported": true }),
        );
        fixture.variable(|variable| variable.description = Some("draft description".into()));
        let mut expected = fixture.key();
        expected.description = None;
        let result = store
            .write(
                who,
                &Discard {
                    target: Some(target),
                    path: Some(SettingPath::parse("web.env.KEY.description").unwrap()),
                    version: Some(diff(store, who).version),
                    ..Discard::default()
                },
            )
            .unwrap();
        assert!(result.changed);
        assert_eq!(fixture.key(), expected);
        assert_eq!(diff(store, who).saved, Some(Revision(1)));
    }
}

#[test]
fn discarding_missing_child_preserves_a_new_service_and_history() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("worker").unwrap(),
                image: Some("alpine:3".into()),
                template: None,
            },
        )
        .unwrap();
    let history = store
        .read(&who, &ployz_store::HistoryQuery::default())
        .unwrap()
        .revisions;
    let result = store
        .write(
            &who,
            &Discard {
                target: Some(ployz_store::DiscardTarget::Saved),
                path: Some(SettingPath::parse("worker.env.MISSING").unwrap()),
                version: Some(diff(&store, &who).version),
                ..Discard::default()
            },
        )
        .unwrap();
    assert!(!result.changed);
    assert_eq!(value(&store, &who, "worker.image"), Some(json!("alpine:3")));
    assert_eq!(
        store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap()
            .revisions,
        history
    );
}

#[test]
fn discarding_a_new_configs_child_never_deletes_its_owner() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    let id = "00000000-0000-4000-8000-000000000009";
    let name = ployz_core::ConfigName::parse("sentry").unwrap();
    store
        .write(
            &who,
            &ployz_store::CreateConfig {
                id: ployz_store::ConfigId::parse(id).unwrap(),
                environment: EnvironmentRef::default(),
                name: name.clone(),
                mounts: vec![],
            },
        )
        .unwrap();
    for file in ["present.conf", "unrelated.conf"] {
        store
            .write(
                &who,
                &ployz_store::PutConfigFile {
                    environment: EnvironmentRef::default(),
                    config: name.clone(),
                    file: ployz_core::ConfigFileName::parse(file).unwrap(),
                    content: file.into(),
                    mode: None,
                    uid: None,
                    gid: None,
                },
            )
            .unwrap();
    }
    let query = ployz_store::ConfigItemQuery {
        environment: EnvironmentRef::default(),
        config: name.into(),
    };
    let original = store.read(&who, &query).unwrap();
    let history = store
        .read(&who, &ployz_store::HistoryQuery::default())
        .unwrap()
        .revisions;
    for file in ["present.conf", "missing.conf"] {
        let result = store.write(
            &who,
            &Discard {
                target: Some(ployz_store::DiscardTarget::Saved),
                path: Some(SettingPath::parse(&format!("configs.@{id}.files.{file}")).unwrap()),
                version: Some(diff(&store, &who).version),
                ..Discard::default()
            },
        );
        if file == "present.conf" {
            assert_eq!(result.unwrap_err().code, RpcErrorCode::Conflict);
        } else {
            assert!(!result.unwrap().changed);
        }
        assert_eq!(store.read(&who, &query).unwrap(), original);
        assert_eq!(
            store
                .read(&who, &ployz_store::HistoryQuery::default())
                .unwrap()
                .revisions,
            history
        );
    }
}

#[test]
fn history_reads_only_the_newest_revisions_it_is_asked_for() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    for replicas in 2..5 {
        set(&store, &who, "web.replicas", json!(replicas));
        publish(&store, &who, None).unwrap();
    }
    let read = |limit| {
        store
            .read(
                &who,
                &ployz_store::HistoryQuery {
                    limit: Some(limit),
                    ..ployz_store::HistoryQuery::default()
                },
            )
            .unwrap()
    };
    let all = read(4);
    assert_eq!(all.revisions.len(), 4);
    assert!(!all.more);
    let page = read(2);
    assert!(page.more);
    assert_eq!(page.revisions, all.revisions.get(..2).unwrap());
    // The oldest shown revision still compares against its predecessor.
    let oldest = page.revisions.last().unwrap();
    assert!(oldest.predecessor.is_some());
    assert_eq!(oldest.total_count, 1);
}

fn add_domain(store: &ConfigStore, who: &Actor, hostname: &str) {
    store
        .write_trusted(
            who,
            &ployz_store::AddDomain {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
                hostname: Some(ployz_store::Hostname::parse(hostname).unwrap()),
                prefix: None,
                port: None,
            },
            &Trusted::default(),
        )
        .unwrap();
}

/// Removing `b.example.com` and adding it back moves it after `c.example.com`.
fn reorder_routes(store: &ConfigStore, who: &Actor) {
    store
        .write_trusted(
            who,
            &ployz_store::RemoveDomain {
                environment: EnvironmentRef::default(),
                domain: "b.example.com".into(),
            },
            &Trusted::default(),
        )
        .unwrap();
    add_domain(store, who, "b.example.com");
}

fn route_order(store: &ConfigStore, who: &Actor) -> Vec<String> {
    store
        .read(who, &ployz_store::DomainsQuery::default())
        .unwrap()
        .domains
        .into_iter()
        .map(|domain| domain.domain.shown().to_string())
        .collect()
}

#[test]
fn reordered_routes_review_publish_and_discard() {
    let (store, who) = shop();
    for hostname in ["a.example.com", "b.example.com", "c.example.com"] {
        add_domain(&store, &who, hostname);
    }
    backend::deploy(&store, &who, "production", 1);
    reorder_routes(&store, &who);
    let review = diff(&store, &who);
    store
        .write(
            &who,
            &Discard {
                target: Some(ployz_store::DiscardTarget::Review),
                version: Some(review.version),
                ..Discard::default()
            },
        )
        .unwrap();
    reorder_routes(&store, &who);
    publish(&store, &who, None).unwrap();
}

#[test]
fn history_undo_refuses_reordered_routes_but_restore_stages() {
    let (store, who) = shop();
    for hostname in ["a.example.com", "b.example.com", "c.example.com"] {
        add_domain(&store, &who, hostname);
    }
    publish(&store, &who, None).unwrap();
    let original = route_order(&store, &who);
    reorder_routes(&store, &who);
    publish(&store, &who, None).unwrap();
    assert_ne!(route_order(&store, &who), original);
    let error = store
        .read(
            &who,
            &ployz_store::HistoryPreviewQuery {
                environment: EnvironmentRef::default(),
                revision: Revision(2),
                action: ployz_store::HistoryAction::Undo,
                change: None,
            },
        )
        .unwrap_err();
    assert_eq!(
        error.message,
        "History cannot restore reordered custom routes"
    );
    // Restore stages the whole snapshot, so a reorder alone is not refused.
    stage_history(&store, &who, 1, ployz_store::HistoryAction::Restore);
    let mut restored = route_order(&store, &who);
    restored.sort();
    assert_eq!(restored, original);
}

#[test]
fn history_undo_of_one_change_ignores_a_route_reorder_in_its_save() {
    let (store, who) = shop();
    for hostname in ["a.example.com", "b.example.com", "c.example.com"] {
        add_domain(&store, &who, hostname);
    }
    publish(&store, &who, None).unwrap();
    reorder_routes(&store, &who);
    set(&store, &who, "api.replicas", json!(2));
    publish(&store, &who, None).unwrap();
    stage_change(&store, &who, 2, "api.replicas");
    assert_eq!(value(&store, &who, "api.replicas"), Some(json!(1)));
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
                prune: false,
                no_prune: false,
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
                storage_locked: false,
                restored_from: None,
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

fn effects(store: &ConfigStore, who: &Actor) -> Vec<(DestructiveKind, String, String)> {
    diff(store, who)
        .effects
        .into_iter()
        .map(|effect| (effect.kind, effect.name, effect.path))
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
            name: "web".into(),
            node: WEB.into(),
            path: "web".into(),
            lost_after: None,
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
        [(
            DestructiveKind::DeletesVolume,
            "data".to_owned(),
            "volumes.data".to_owned()
        )]
    );
}

/// Delete Volume `data`, then create a different, unmounted Volume named `data`.
fn replace_data_unmounted(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: VolumeName::parse("data").unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            who,
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
        )
        .unwrap();
}

fn data_of(changes: &[ployz_store::NodeChange], name: &str) -> Option<DataEffect> {
    changes
        .iter()
        .find(|change| change.name == name)
        .unwrap()
        .data
}

#[test]
fn a_deployed_volume_replaced_by_one_of_its_name_is_not_kept() {
    let (store, who) = shop();
    mount_data(&store, &who);
    apply_all(&store, &who, &Trusted::default());
    replace_data_unmounted(&store, &who);
    let view = diff(&store, &who);
    assert_eq!(data_of(&view.changes, "web"), None);
    assert!(
        view.effects
            .iter()
            .all(|effect| effect.kind != DestructiveKind::DetachesVolume)
    );
}

#[test]
fn a_saved_volume_replaced_by_one_of_its_name_is_not_kept_in_the_draft() {
    let (store, who) = shop();
    mount_data(&store, &who);
    publish(&store, &who, None).unwrap();
    replace_data_unmounted(&store, &who);
    assert_eq!(data_of(&diff(&store, &who).draft_changes, "web"), None);
}

#[test]
fn removing_a_deployed_service_keeps_the_data_of_a_volume_that_stays() {
    let (store, who) = shop();
    mount_data(&store, &who);
    apply_all(&store, &who, &Trusted::default());
    remove_service(&store, &who, "web");
    let web = diff(&store, &who)
        .changes
        .into_iter()
        .find(|change| change.name == "web")
        .unwrap();
    assert_eq!(web.data, Some(DataEffect::Kept));
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
            "data".to_owned(),
            "web.mounts.data".to_owned()
        )]
    );
}

const DATA: &str = "00000000-0000-4000-8000-000000000005";

/// Deploy `data` on `web`, then stage what Cloud's restore writes: one Batch that
/// moves `web` onto `data-1142`, restored from `data`'s point at `at`. Returns the
/// refused Publish and the review it refused.
fn restore_data_at(at: u64) -> (RpcError, DiffView) {
    let (store, who) = shop();
    mount_data(&store, &who);
    apply_all(&store, &who, &Trusted::default());
    let restore = Command::decode(json!({
        "command": "batch",
        "commands": [
            {"command": "edit", "changes": [{"op": "unset", "path": "web.mounts.data"}]},
            {
                "command": "create_volume",
                "id": "00000000-0000-4000-8000-000000000006",
                "name": "data-1142",
                "storage": {"kind": "docker"},
                "mounts": [{"service": "web", "path": "/data"}],
                "restored_from": {"volume": DATA, "at": at},
            },
        ],
    }))
    .unwrap();
    store.write(&who, &restore).unwrap();
    let refused = publish_as(&store, &who, None, Approval::Required).unwrap_err();
    (refused, diff(&store, &who))
}

#[test]
fn a_restore_says_which_changes_its_detach_loses() {
    let (refused, review) = restore_data_at(1_790_000_000);
    assert_eq!(refused.code.as_str(), "approval_required", "{refused:?}");
    assert_eq!(
        refused.details["effects"],
        json!([{
            "kind": "detaches_volume",
            "name": "data",
            "node": WEB,
            "path": "web.mounts.data",
            "lost_after": {"at": 1_790_000_000_u64, "restored": "data-1142"},
        }])
    );
    assert_eq!(json!(review.effects), refused.details["effects"]);
}

#[test]
fn an_approval_binds_the_restore_point_it_loses_after() {
    let (earlier, earlier_review) = restore_data_at(1_790_000_000);
    let (later, later_review) = restore_data_at(1_790_000_600);
    assert_eq!(earlier_review.version, later_review.version);
    assert_ne!(earlier.details["approval"], later.details["approval"]);
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
                prefix: None,
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
    assert_eq!(effects[0].1, "app.example.com");
    assert!(effects[0].2.starts_with("web.routes."), "{effects:?}");
    let refused = store
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
                approval: Approval::Required,
                ..Trusted::default()
            },
        )
        .unwrap_err();
    assert_eq!(
        refused.message,
        "A human must approve this first: remove domain app.example.com"
    );
}

#[test]
fn an_effect_from_a_cloud_that_names_nothing_still_reads() {
    let effect: DestructiveEffect = serde_json::from_value(json!({
        "kind": "removes_domain",
        "node": WEB,
        "path": "web.routes.e23290af-63ac-54c7-8368-5e4c6bcf804e",
    }))
    .unwrap();
    assert_eq!(effect.kind, DestructiveKind::RemovesDomain);
    assert_eq!(effect.name, "");
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
        json!([{"kind": "removes_service", "name": "web", "node": WEB, "path": "web"}])
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
fn approving_a_removal_needs_changes_destroy_but_a_plain_deploy_does_not() {
    let (store, who) = shop();
    apply_all(&store, &who, &Trusted::default());
    // Nothing runs that a prune removes.
    let holding = |permissions: &[Permission], approval: Approval, asks: Asks| Trusted {
        permissions: Some(permissions.iter().copied().collect()),
        approval,
        asks,
        running: Some(RunningObservation::default()),
        ..Trusted::default()
    };
    let stages = [Permission::ChangesStage, Permission::ChangesDeploy];
    let asked = store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: DeploymentId::parse("00000000-0000-4000-8000-000000000102").unwrap(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &holding(&stages, Approval::Required, Asks::EveryDeploy),
        )
        .unwrap_err();
    assert_eq!(asked.code.as_str(), "approval_required", "{asked:?}");
    remove_service(&store, &who, "web");
    let publish = |permissions: &[Permission]| {
        store.write_trusted(
            &who,
            &Publish {
                id: None,
                environment: EnvironmentRef::default(),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
            &holding(permissions, Approval::Required, Asks::Destroys),
        )
    };
    let refused = publish(&stages).unwrap_err();
    assert_eq!(refused.code.as_str(), "forbidden", "{refused:?}");
    assert_eq!(refused.message, "Needs changes:destroy");
    let asked = publish(&[Permission::ChangesStage, Permission::ChangesDestroy]).unwrap_err();
    assert_eq!(asked.code.as_str(), "approval_required", "{asked:?}");
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
fn a_published_removal_not_yet_deployed_asks_again_on_the_next_publication() {
    let (store, who) = shop();
    apply_all(&store, &who, &Trusted::default());
    remove_service(&store, &who, "web");
    let first = publish_as(&store, &who, None, Approval::Required).unwrap_err();
    let approved = Approval::Approved(
        ApprovalDigest::parse(first.details["approval"].as_str().unwrap()).unwrap(),
    );
    publish_as(&store, &who, None, approved).unwrap();
    set(&store, &who, "api.replicas", json!(2));
    let again = publish_as(&store, &who, None, Approval::Required).unwrap_err();
    assert_eq!(again.details["effects"], first.details["effects"]);
    assert_ne!(again.details["approval"], first.details["approval"]);
}

#[test]
fn a_new_publication_asks_again_for_a_removal_still_queued() {
    let (store, who) = shop();
    apply_all(&store, &who, &Trusted::default());
    remove_service(&store, &who, "web");
    let deploy = |approval| {
        store.write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: DeploymentId::parse("00000000-0000-4000-8000-000000000102").unwrap(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &Trusted {
                approval,
                ..Trusted::default()
            },
        )
    };
    let refused = deploy(Approval::Required).unwrap_err();
    let digest = refused.details["approval"].as_str().unwrap();
    deploy(Approval::Approved(ApprovalDigest::parse(digest).unwrap())).unwrap();
    set(&store, &who, "api.replicas", json!(2));
    let again = publish_as(&store, &who, None, Approval::Required).unwrap_err();
    assert_eq!(again.details["effects"], refused.details["effects"]);
}

#[test]
fn with_approvals_switched_off_a_removal_publishes_unasked() {
    let (store, who) = shop();
    apply_all(&store, &who, &Trusted::default());
    remove_service(&store, &who, "web");
    assert!(!diff(&store, &who).effects.is_empty());
    let published = publish_as(&store, &who, None, Approval::NotRequired).unwrap();
    assert!(published.created);
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
                    id: None,
                    environment: EnvironmentRef::default(),
                    version: None,
                    message: None,
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

fn volume_at(store: &ConfigStore, who: &Actor, path: &str) {
    store
        .write(
            who,
            &CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: VolumeName::parse("data").unwrap(),
                mounts: vec![Mount {
                    service: ServiceName::parse("web").unwrap(),
                    path: path.into(),
                }],
            },
        )
        .unwrap();
}

fn config_with(
    store: &ConfigStore,
    who: &Actor,
    mounts: Vec<ployz_store::ConfigMountAt>,
    file: &str,
) {
    let config = ployz_core::ConfigName::parse("sentry").unwrap();
    store
        .write(
            who,
            &ployz_store::CreateConfig {
                environment: EnvironmentRef::default(),
                id: ployz_store::ConfigId::parse("00000000-0000-4000-8000-000000000009").unwrap(),
                name: config.clone(),
                mounts,
            },
        )
        .unwrap();
    put_config_file(store, who, file);
}

fn put_config_file(store: &ConfigStore, who: &Actor, file: &str) {
    store
        .write(
            who,
            &ployz_store::PutConfigFile {
                environment: EnvironmentRef::default(),
                config: ployz_core::ConfigName::parse("sentry").unwrap(),
                file: ployz_core::ConfigFileName::parse(file).unwrap(),
                content: "content".into(),
                mode: None,
                uid: None,
                gid: None,
            },
        )
        .unwrap();
}

fn unmount_data(store: &ConfigStore, who: &Actor) {
    store
        .write(
            who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("web.mounts.data").unwrap(),
                }],
            },
        )
        .unwrap();
}

#[test]
fn history_undo_of_a_volume_mount_takes_back_the_config_file_now_at_its_path() {
    let (store, who) = shop();
    volume_at(&store, &who, "/etc/a");
    config_with(&store, &who, Vec::new(), "a");
    publish(&store, &who, None).unwrap();
    unmount_data(&store, &who);
    store
        .write(
            &who,
            &ployz_store::AttachConfig {
                environment: EnvironmentRef::default(),
                config: ployz_core::ConfigName::parse("sentry").unwrap(),
                service: ServiceName::parse("web").unwrap(),
                dir: "/etc".into(),
            },
        )
        .unwrap();
    publish(&store, &who, None).unwrap();
    stage_change(&store, &who, 2, "web.mounts.data");
    assert_eq!(
        value(&store, &who, "web.mounts.data"),
        Some(json!("/etc/a"))
    );
    assert_eq!(value(&store, &who, "web.mounts.sentry"), None);
}

#[test]
fn history_undo_of_a_volume_mount_takes_back_the_config_file_added_at_its_path() {
    let (store, who) = shop();
    config_with(
        &store,
        &who,
        vec![ployz_store::ConfigMountAt {
            service: ServiceName::parse("web").unwrap(),
            dir: "/etc".into(),
        }],
        "keep",
    );
    volume_at(&store, &who, "/etc/a/b");
    publish(&store, &who, None).unwrap();
    unmount_data(&store, &who);
    put_config_file(&store, &who, "a");
    publish(&store, &who, None).unwrap();
    stage_change(&store, &who, 2, "web.mounts.data");
    assert_eq!(
        value(&store, &who, "web.mounts.data"),
        Some(json!("/etc/a/b"))
    );
    let files = serde_json::to_string(
        &store
            .read(
                &who,
                &ployz_store::ConfigItemQuery {
                    environment: EnvironmentRef::default(),
                    config: ployz_core::ConfigName::parse("sentry").unwrap().into(),
                },
            )
            .unwrap(),
    )
    .unwrap();
    assert!(!files.contains("\"a\""), "{files}");
}

#[test]
fn sealing_scrubs_the_revision_of_a_running_deployment_the_next_claim_takes_over() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", json!("plain-private"));
    let first = backend::admit(&store, &who, "production", 1);
    store
        .claim(&first, &ployz_store::RunnerId::parse("one").unwrap())
        .unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    assert!(history_text(&store, &who).contains("plain-private"));
    let second = backend::admit(&store, &who, "production", 2);
    store
        .claim(&second, &ployz_store::RunnerId::parse("two").unwrap())
        .unwrap();
    assert!(!history_text(&store, &who).contains("plain-private"));
}

#[test]
fn history_hides_the_plain_text_a_lapsed_deployment_protected() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
    set(&store, &who, "web.env.TOKEN", json!("plain-private"));
    let claimed = backend::admit(&store, &who, "production", 1);
    store
        .claim(&claimed, &ployz_store::RunnerId::parse("runner").unwrap())
        .unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "web.env.TOKEN");
    publish(&store, &who, None).unwrap();
    assert!(history_text(&store, &who).contains("plain-private"));
    execute(&url, "UPDATE config_deployment SET lease = 0");
    assert!(!history_text(&store, &who).contains("plain-private"));
    for action in [
        ployz_store::HistoryAction::Restore,
        ployz_store::HistoryAction::Undo,
    ] {
        let query = ployz_store::HistoryPreviewQuery {
            environment: EnvironmentRef::default(),
            revision: Revision(2),
            action,
            change: None,
        };
        if let Ok(preview) = store.read(&who, &query) {
            assert!(
                !serde_json::to_string(&preview)
                    .unwrap()
                    .contains("plain-private")
            );
            let version = preview.version;
            store
                .write(
                    &who,
                    &ployz_store::StageHistory {
                        environment: EnvironmentRef::default(),
                        revision: query.revision,
                        action,
                        change: None,
                        version,
                        accept_overwrite: true,
                    },
                )
                .unwrap();
            assert_ne!(
                value(&store, &who, "web.env.TOKEN"),
                Some(json!("plain-private"))
            );
            discard_to_saved(&store, &who);
        }
    }
}

fn discard_review(store: &ConfigStore, who: &Actor) -> ployz_store::Discarded {
    store
        .write(
            who,
            &Discard {
                target: Some(ployz_store::DiscardTarget::Review),
                version: Some(diff(store, who).version),
                ..Discard::default()
            },
        )
        .unwrap()
}

#[test]
fn discard_review_abandons_a_staged_roll_back() {
    for running in [1, 2, 3] {
        let (store, who) = shop();
        publish(&store, &who, None).unwrap();
        for replicas in [2, 3] {
            if running == replicas - 1 {
                backend::deploy(&store, &who, "production", 1);
            }
            set(&store, &who, "web.replicas", json!(replicas));
            publish(&store, &who, None).unwrap();
        }
        if running == 3 {
            backend::deploy(&store, &who, "production", 1);
        }
        assert!(stage_history(&store, &who, 1, ployz_store::HistoryAction::Restore).changed);
        discard_review(&store, &who);
        let after = diff(&store, &who);
        assert_eq!(
            value(&store, &who, "web.replicas"),
            Some(json!(3)),
            "running #{running}"
        );
        assert_eq!(after.draft_count, 0, "running #{running}");
        assert_eq!(
            after.total_count,
            usize::from(running != 3),
            "running #{running}: only the Save that is not running stays listed"
        );
    }
}

#[test]
fn review_shows_a_sealed_value_entered_again_as_changed_and_never_shows_it() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    assert_eq!(diff(&store, &who).draft_count, 0);
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    assert_eq!(diff(&store, &who).draft_count, 0);
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 2));
    let review = diff(&store, &who);
    assert_eq!(review.draft_count, 1);
    assert!(changed(&store, &who, "web").contains(&"web.env.TOKEN".to_owned()));
    let row = review
        .draft_changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|row| row.path == "web.env.TOKEN")
        .unwrap();
    assert_eq!(row.before, json!({ "secret": true }));
    assert_eq!(row.after, json!({ "secret": true }));
    let public = serde_json::to_string(&review).unwrap();
    assert!(!public.contains(&backend::ciphertext(1)), "{public}");
    assert!(!public.contains(&backend::ciphertext(2)), "{public}");
}

#[test]
fn history_past_a_sealed_value_brings_the_variable_back_not_set_and_never_an_older_value() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", json!("plain-value"));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 2));
    publish(&store, &who, None).unwrap();
    // Back to the first sealed value: History keeps the one it has.
    for (revision, action) in [
        (2, ployz_store::HistoryAction::Restore),
        (3, ployz_store::HistoryAction::Undo),
    ] {
        let preview = history_preview(&store, &who, revision, action).unwrap();
        assert_eq!(preview.total_count, 0, "#{revision} {action:?}");
        stage_history(&store, &who, revision, action);
        assert_eq!(diff(&store, &who).draft_count, 0);
    }
    let deployed = backend::deploy(&store, &who, "production", 1).to_string();
    assert!(deployed.contains(&backend::ciphertext(2)));
    assert!(!deployed.contains(&backend::ciphertext(1)));
    // Once the key is gone, History past a sealed value brings it back Not set:
    // never the plain text before it, never a ciphertext.
    unset(&store, &who, "web.env.TOKEN");
    publish(&store, &who, None).unwrap();
    for revision in [1, 2] {
        store
            .write(
                &who,
                &Discard {
                    target: Some(ployz_store::DiscardTarget::Saved),
                    ..Discard::default()
                },
            )
            .unwrap();
        let preview =
            history_preview(&store, &who, revision, ployz_store::HistoryAction::Restore).unwrap();
        let public = serde_json::to_string(&preview).unwrap();
        assert!(!public.contains("plain-value"), "#{revision} {public}");
        assert!(
            !public.contains(&backend::ciphertext(1)),
            "#{revision} {public}"
        );
        assert!(
            !public.contains(&backend::ciphertext(2)),
            "#{revision} {public}"
        );
        let staged = stage_history(&store, &who, revision, ployz_store::HistoryAction::Restore);
        assert_eq!(not_set(&staged), ["web.TOKEN"], "#{revision}");
        assert_eq!(
            value(&store, &who, "web.env.TOKEN"),
            Some(json!({ "secret": false })),
            "#{revision}"
        );
        assert!(
            !serde_json::to_string(&working(&store, &who))
                .unwrap()
                .contains("plain-value")
        );
    }
}

#[test]
fn history_never_brings_a_sealed_value_back_as_port() {
    for (revision, action) in [
        (2, ployz_store::HistoryAction::Undo),
        (1, ployz_store::HistoryAction::Restore),
    ] {
        let (store, who) = shop();
        set(&store, &who, "web.env.PORT", json!("8080"));
        publish(&store, &who, None).unwrap();
        rename_variable(&store, &who, "web.env.PORT", "TOKEN");
        publish(&store, &who, None).unwrap();
        set(&store, &who, "web.env.TOKEN", sealed(3, "TOKEN", 1));
        publish(&store, &who, None).unwrap();
        let query = ployz_store::HistoryPreviewQuery {
            environment: EnvironmentRef::default(),
            revision: Revision(revision),
            action,
            change: None,
        };
        let case = format!("#{revision} {action:?}");
        let previewed = store.read(&who, &query).unwrap_err();
        let staged = store
            .write(
                &who,
                &ployz_store::StageHistory {
                    environment: EnvironmentRef::default(),
                    revision: query.revision,
                    action,
                    change: None,
                    version: diff(&store, &who).version,
                    accept_overwrite: true,
                },
            )
            .unwrap_err();
        for error in [previewed, staged] {
            assert_eq!(
                error.code,
                RpcErrorCode::InvalidArgument,
                "{case} {error:?}"
            );
            assert!(error.message.contains("route traffic"), "{case} {error:?}");
            assert!(
                !format!("{error:?}").contains(&backend::ciphertext(1)),
                "{case} {error:?}"
            );
        }
        assert_eq!(value(&store, &who, "web.env.PORT"), None, "{case}");
        assert_eq!(diff(&store, &who).draft_count, 0, "{case}");
    }
}
