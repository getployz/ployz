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
    store.write_trusted(
        who,
        &Publish {
            environment: EnvironmentRef::default(),
            version: version.map(Into::into),
            message: None,
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
            target: ployz_store::DiscardTarget::Head,
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
    // A saved edit is reversed only in Working State until the next Save.
    set(&store, &who, "web.replicas", json!(4));
    publish(&store, &who, None).unwrap();
    assert_eq!(changed(&store, &who, "web"), ["web.replicas"]);
    let discarded = discard(&store, &who, Some("web.replicas"), None).unwrap();
    assert_eq!(discarded.saved, Some(Revision(2)));
    assert_eq!(value(&store, &who, "web.replicas"), Some(json!(1)));
    let view = diff(&store, &who);
    assert!(!view.published);
    assert!(changed(&store, &who, "web").is_empty());
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
    assert_eq!(view.draft_count, 1);
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

#[test]
fn saved_history_keeps_message_actor_and_no_op_save_immutable() {
    let (store, mut who) = shop();
    who.principal = Some(ployz_store::Principal::parse("Ada Lovelace").unwrap());
    let save = |message: &str| {
        store
            .write_trusted(
                &who,
                &Publish {
                    message: Some(message.into()),
                    ..Publish::default()
                },
                &Trusted::default(),
            )
            .unwrap()
    };
    assert!(save("  First version  ").created);
    let before = store
        .read(&who, &ployz_store::HistoryQuery::default())
        .unwrap();
    assert_eq!(
        before.revisions[0].message.as_deref(),
        Some("First version")
    );
    assert_eq!(before.revisions[0].saved_by, who.principal);
    assert!(before.revisions[0].saved_at.is_some());
    assert_eq!(before.revisions[0].predecessor, None);
    assert!(!save("A duplicate must not replace the message").created);
    assert_eq!(
        store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap(),
        before
    );
    discard(&store, &who, Some("web"), None).unwrap();
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
    };
    let preview = store.read(&who, &query).unwrap();
    assert_eq!(preview.overwritten, ["api.startCommand", "web.replicas"]);
    assert_eq!(value(&store, &who, "web.replicas"), Some(json!(5)));
    let command = ployz_store::StageHistory {
        environment: EnvironmentRef::default(),
        revision: query.revision,
        action: query.action,
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
        reversed.review_changes()[0].settings[0].path,
        "web.replicas"
    );
    store
        .write(
            &who,
            &Discard {
                environment: EnvironmentRef::default(),
                target: ployz_store::DiscardTarget::Saved,
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
    assert_eq!(before.review_changes()[0].settings.len(), 2);
    let history = store
        .read(&who, &ployz_store::HistoryQuery::default())
        .unwrap();
    let written = store
        .write(
            &who,
            &Discard {
                target: ployz_store::DiscardTarget::Review,
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
fn discard_review_whole_runtime_node_dominates_draft_children() {
    let (store, who) = shop();
    publish(&store, &who, None).unwrap();
    set(&store, &who, "web.replicas", json!(3));
    let before = diff(&store, &who);
    assert_eq!(before.review_changes().len(), 2);
    assert_eq!(
        before.review_changes()[0].lifecycle,
        ReviewLifecycleKind::Create
    );
    let history = store
        .read(&who, &ployz_store::HistoryQuery::default())
        .unwrap();
    store
        .write(
            &who,
            &Discard {
                target: ployz_store::DiscardTarget::Review,
                version: Some(before.version),
                ..Discard::default()
            },
        )
        .unwrap();
    assert!(working(&store, &who).is_empty());
    assert_eq!(diff(&store, &who).total_count, 0);
    assert!(diff(&store, &who).draft_count > 0);
    assert_eq!(
        store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap()
            .revisions,
        history.revisions
    );
}
