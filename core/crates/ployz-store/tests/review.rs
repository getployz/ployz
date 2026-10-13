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
                storage_locked: false,
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
    let revision = before.revisions.first().unwrap();
    assert_eq!(revision.message.as_deref(), Some("First version"));
    assert_eq!(revision.saved_by, who.principal);
    assert!(revision.saved_at.is_some());
    assert_eq!(revision.predecessor, None);
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
    assert_eq!(before.review_changes().first().unwrap().settings.len(), 2);
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

fn stage_history(
    store: &ConfigStore,
    who: &Actor,
    revision: u64,
    action: ployz_store::HistoryAction,
) {
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
        .unwrap();
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
                target: ployz_store::DiscardTarget::Saved,
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
    set(
        &store,
        &who,
        "web.env.TOKEN",
        json!({ "secret": "first-private-value" }),
    );
    publish(&store, &who, None).unwrap();
    set(
        &store,
        &who,
        "web.env.TOKEN",
        json!({ "secret": "second-private-value" }),
    );
    publish(&store, &who, None).unwrap();
    let query = |revision| ployz_store::HistoryPreviewQuery {
        environment: EnvironmentRef::default(),
        revision: Revision(revision),
        action: ployz_store::HistoryAction::Restore,
        change: None,
    };
    let preview = store.read(&who, &query(2)).unwrap();
    let public = serde_json::to_string(&preview).unwrap();
    assert!(!public.contains("private-value"));
    assert!(!public.contains("sealed"));
    let history = serde_json::to_string(
        &store
            .read(&who, &ployz_store::HistoryQuery::default())
            .unwrap(),
    )
    .unwrap();
    assert!(!history.contains("private-value"));
    for (revision, action) in [
        (1, ployz_store::HistoryAction::Restore),
        (2, ployz_store::HistoryAction::Restore),
        (3, ployz_store::HistoryAction::Undo),
        (2, ployz_store::HistoryAction::Undo),
    ] {
        let preview = history_preview(&store, &who, revision, action).unwrap();
        assert_eq!(preview.total_count, 0, "#{revision} {action:?}");
        assert!(
            !serde_json::to_string(&preview)
                .unwrap()
                .contains("private-value")
        );
        stage_history(&store, &who, revision, action);
        assert_eq!(diff(&store, &who).draft_count, 0);
    }
    let deployed = backend::deploy(&store, &who, "production", 1);
    assert!(deployed.to_string().contains("second-private-value"));
    assert!(!deployed.to_string().contains("first-private-value"));
}

#[test]
fn history_undo_of_a_sealed_rotation_by_delete_and_add_keeps_the_new_value() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "web.env.ROT_KEY",
        json!({ "secret": "old-private" }),
    );
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "web.env.ROT_KEY");
    set(
        &store,
        &who,
        "web.env.ROT_KEY",
        json!({ "secret": "new-private" }),
    );
    publish(&store, &who, None).unwrap();
    let preview = history_preview(&store, &who, 2, ployz_store::HistoryAction::Undo).unwrap();
    assert_eq!(preview.total_count, 0);
    stage_history(&store, &who, 2, ployz_store::HistoryAction::Undo);
    assert_eq!(diff(&store, &who).draft_count, 0);
    let deployed = backend::deploy(&store, &who, "production", 1);
    assert!(deployed.to_string().contains("new-private"));
    assert!(!deployed.to_string().contains("old-private"));
}

#[test]
fn history_keeps_a_sealed_key_deleted_since_deleted() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "web.env.TOKEN",
        json!({ "secret": "private-value" }),
    );
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "web.env.TOKEN");
    publish(&store, &who, None).unwrap();
    for (revision, action) in [
        (1, ployz_store::HistoryAction::Restore),
        (2, ployz_store::HistoryAction::Undo),
    ] {
        let preview = history_preview(&store, &who, revision, action).unwrap();
        assert_eq!(preview.total_count, 0, "#{revision} {action:?}");
        stage_history(&store, &who, revision, action);
        assert_eq!(value(&store, &who, "web.env.TOKEN"), None);
    }
}

#[test]
fn history_keeps_a_sealed_key_added_since() {
    let (store, who) = shop();
    set(&store, &who, "web.env.PLAIN", json!("plain-value"));
    publish(&store, &who, None).unwrap();
    set(
        &store,
        &who,
        "web.env.TOKEN",
        json!({ "secret": "private-value" }),
    );
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
    set(
        &store,
        &who,
        "web.env.TOKEN",
        json!({ "secret": "private-value" }),
    );
    publish(&store, &who, None).unwrap();
    unset(&store, &who, "web.env.TOKEN");
    publish(&store, &who, None).unwrap();
    let error = history_preview(&store, &who, 1, ployz_store::HistoryAction::Restore).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(error.message, "TOKEN was sealed. Set it again.");
    assert_eq!(value(&store, &who, "web.env.TOKEN"), None);
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
    set(
        &store,
        &who,
        "web.env.TOKEN",
        json!({ "secret": "private-value" }),
    );
    publish(&store, &who, None).unwrap();
    assert_eq!(
        history_values(&store, &who, "web.env.TOKEN"),
        vec![None, Some(json!({ "secret": true })), None, None, None]
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
    set(
        &store,
        &who,
        "web.env.TOKEN",
        json!({ "secret": "old-private" }),
    );
    publish(&store, &who, None).unwrap();
    set(
        &store,
        &who,
        "web.env.TOKEN",
        json!({ "secret": "new-private" }),
    );
    publish(&store, &who, None).unwrap();
    assert_eq!(
        history_values(&store, &who, "web.env.TOKEN"),
        vec![None, Some(json!({ "secret": true })), None]
    );
}

#[test]
fn sealing_leaves_the_revision_a_queued_deployment_runs() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "web.env.TOKEN",
        json!({ "secret": "old-private" }),
    );
    let queued = backend::admit(&store, &who, "production", 1);
    set(
        &store,
        &who,
        "web.env.TOKEN",
        json!({ "secret": "new-private" }),
    );
    publish(&store, &who, None).unwrap();
    let deployed = backend::run(&store, &queued);
    assert!(deployed.to_string().contains("old-private"));
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
    set(&store, &who, "web.env.TOKEN", json!({ "secret": "sealed" }));
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
    set(&store, &who, "web.env.TOKEN", json!({ "secret": "sealed" }));
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
    set(&store, &who, "web.env.NEW", json!({ "secret": "sealed" }));
    publish(&store, &who, None).unwrap();
    assert!(!history_text(&store, &who).contains("plain-private"));
}

#[test]
fn sealing_scrubs_plain_text_of_a_value_deleted_since() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", json!("plain-private"));
    let queued = backend::admit(&store, &who, "production", 1);
    set(&store, &who, "web.env.TOKEN", json!({ "secret": "sealed" }));
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
    set(&store, &who, "web.env.TOKEN", json!({ "secret": "sealed" }));
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
    set(&store, &who, "web.env.TOKEN", json!({ "secret": "sealed" }));
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
    set(&store, &who, "web.env.TOKEN", json!({ "secret": "sealed" }));
    publish(&store, &who, None).unwrap();
    backend::admit(&store, &who, "production", 2);
    set(&store, &who, "api.replicas", json!(2));
    publish(&store, &who, None).unwrap();
    assert!(!history_text(&store, &who).contains("plain-private"));
}

#[test]
fn history_never_brings_back_plain_text_of_a_key_sealed_only_between_saves() {
    let (store, who) = shop();
    set(&store, &who, "web.env.TOKEN", json!("plain-value"));
    publish(&store, &who, None).unwrap();
    set(
        &store,
        &who,
        "web.env.TOKEN",
        json!({ "secret": "private-value" }),
    );
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
    set(&store, &who, "web.env.KEY", json!({ "secret": "private" }));
    let result = store.write(
        &who,
        &Discard {
            target: ployz_store::DiscardTarget::Saved,
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
        preview
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
                target: ployz_store::DiscardTarget::Review,
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
                    target: ployz_store::DiscardTarget::Review,
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
                target: ployz_store::DiscardTarget::Saved,
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
                    target,
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
                target: ployz_store::DiscardTarget::Saved,
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
                        target,
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
                        target,
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
                    target,
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
                target: ployz_store::DiscardTarget::Saved,
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
                target: ployz_store::DiscardTarget::Saved,
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
                target: ployz_store::DiscardTarget::Review,
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
    set(&store, &who, "web.env.TOKEN", json!({ "secret": "sealed" }));
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
    set(&store, &who, "web.env.TOKEN", json!({ "secret": "sealed" }));
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
