//! Review, Publish and Discard through `read` and `write` only, on in-memory SQLite.

use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, Change, Command, ConfigStore, CreateProject, CreateService, DiffQuery, DiffView,
    Discard, Discarded, Edit, EnvironmentId, EnvironmentQuery, EnvironmentRef, OrganizationId,
    ProjectId, ProjectName, Publish, Published, Query, Revision, ServiceId, View, Written,
};
use serde_json::{Value, json};

const PROJECT: &str = "00000000-0000-4000-8000-000000000001";
const ENVIRONMENT: &str = "00000000-0000-4000-8000-000000000002";

/// A store with Project `shop` and new Services `web` (nginx) and `api` (caddy).
fn shop() -> (ConfigStore, Actor) {
    let store = ConfigStore::open("sqlite::memory:").unwrap();
    let who = Actor {
        organization: OrganizationId::parse("org").unwrap(),
    };
    store
        .write(
            &who,
            Command::CreateProject(CreateProject {
                id: ProjectId::parse(PROJECT).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(ENVIRONMENT).unwrap(),
            }),
        )
        .unwrap();
    for (n, name, image) in [(3, "web", "nginx:1"), (4, "api", "caddy:2")] {
        store
            .write(
                &who,
                Command::CreateService(CreateService {
                    id: ServiceId::parse(format!("00000000-0000-4000-8000-00000000000{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: image.into(),
                }),
            )
            .unwrap();
    }
    (store, who)
}

fn set(store: &ConfigStore, who: &Actor, path: &str, value: Value) {
    store
        .write(
            who,
            Command::Edit(Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: path.into(),
                    value,
                }],
            }),
        )
        .unwrap();
}

fn diff(store: &ConfigStore, who: &Actor) -> DiffView {
    let View::Diff(view) = store.read(who, &Query::Diff(DiffQuery::default())).unwrap() else {
        unreachable!("a diff reads a diff")
    };
    view
}

/// Every Setting value in Working State, by path.
fn working(store: &ConfigStore, who: &Actor) -> Vec<(String, Value)> {
    let View::Environment(view) = store
        .read(who, &Query::Environment(EnvironmentQuery::default()))
        .unwrap()
    else {
        unreachable!("an Environment query reads an Environment")
    };
    view.settings
        .into_iter()
        .map(|row| (row.path, row.value))
        .collect()
}

fn value(store: &ConfigStore, who: &Actor, path: &str) -> Option<Value> {
    working(store, who)
        .into_iter()
        .find(|(at, _)| at == path)
        .map(|(_, value)| value)
}

fn publish(store: &ConfigStore, who: &Actor, version: Option<&str>) -> Result<Published, RpcError> {
    store
        .write(
            who,
            Command::Publish(Publish {
                environment: EnvironmentRef::default(),
                version: version.map(Into::into),
            }),
        )
        .map(|written| match written {
            Written::Published(published) => published,
            other => unreachable!("publish wrote {other:?}"),
        })
}

fn discard(
    store: &ConfigStore,
    who: &Actor,
    path: Option<&str>,
    version: Option<&str>,
) -> Result<Discarded, RpcError> {
    store
        .write(
            who,
            Command::Discard(Discard {
                environment: EnvironmentRef::default(),
                path: path.map(Into::into),
                version: version.map(Into::into),
            }),
        )
        .map(|written| match written {
            Written::Discarded(discarded) => discarded,
            other => unreachable!("discard wrote {other:?}"),
        })
}

#[test]
fn diff_groups_new_services_and_compares_edits_with_their_introduction() {
    let (store, who) = shop();
    set(&store, &who, "web.replicas", json!(3));
    let view = diff(&store, &who);
    assert_eq!(view.version, "4:0:none");
    assert_eq!(view.saved, None);
    assert!(!view.published);
    assert_eq!(
        serde_json::to_value(&view.changes).unwrap(),
        json!([
            {
                "type": "service", "id": "00000000-0000-4000-8000-000000000003", "name": "web",
                "lifecycle": "create", "comparison": "introduction",
                "settings": [{ "path": "web.replicas", "kind": "update", "before": 1, "after": 3, "canRestore": true }],
            },
            {
                "type": "service", "id": "00000000-0000-4000-8000-000000000004", "name": "api",
                "lifecycle": "create", "comparison": "introduction", "settings": [],
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
        view.changes[0]
            .settings
            .iter()
            .map(|row| row.path.as_str())
            .collect::<Vec<_>>(),
        ["web.image"]
    );
}

#[test]
fn a_published_new_nodes_setting_has_no_discard_baseline() {
    let (store, who) = shop();
    set(&store, &who, "web.replicas", json!(4));
    publish(&store, &who, None).unwrap();
    let error = discard(&store, &who, Some("web.replicas"), None).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(value(&store, &who, "web.replicas"), Some(json!(4)));
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
    assert_eq!(error.details, json!({ "services": ["web", "api"] }));
    let error = discard(&store, &who, Some("web.nope"), None).unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
}
