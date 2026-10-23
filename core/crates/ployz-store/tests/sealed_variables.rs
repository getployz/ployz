//! The `sealedVariables` flag through the Store's interface only, on SQLite and on
//! Postgres (see `backend`): off for every Organization until a row exists, which
//! nothing in the Store inserts, and gone when the Organization is torn down.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed JSON results use indexing; missing entries must fail the test."
)]

use ployz_core::ServiceName;
use ployz_store::{
    Actor, Admit, Change, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId,
    DiffQuery, Edit, EnvironmentId, EnvironmentQuery, EnvironmentRef, OrganizationId, ProjectId,
    ProjectName, RunnerId, ServiceLineageId, SettingPath,
};
use serde_json::{Value, json};

mod backend;

const SECRET: &str = "s3cr3t-pässwörd 🔑";

fn org() -> OrganizationId {
    OrganizationId::parse("org").unwrap()
}

fn who() -> Actor {
    Actor::system(org())
}

/// Project `shop` with Service `web` in the Store at `store`.
fn shop(store: &ConfigStore) {
    store
        .write(
            &who(),
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
            &who(),
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
}

fn set(store: &ConfigStore, path: &str, value: Value) {
    store
        .write(
            &who(),
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

fn value(store: &ConfigStore, path: &str) -> Value {
    store
        .read(
            &who(),
            &EnvironmentQuery {
                path: Some(SettingPath::parse(path).unwrap()),
                ..EnvironmentQuery::default()
            },
        )
        .unwrap()
        .settings
        .remove(0)
        .value
}

/// Turn the flag on for `organization` beside the Store, the way no Store code does.
fn turn_on(url: &str, organization: &str) {
    backend::execute(
        url,
        &format!("INSERT INTO config_sealed_variables (organization_id) VALUES ('{organization}')"),
    );
}

#[test]
fn a_fresh_store_has_no_flag_row_and_the_legacy_path_runs_unchanged_with_none() {
    let store = backend::open();
    assert!(!store.sealed_variables(&org()).unwrap());
    shop(&store);
    set(&store, "web.env.PASSWORD", json!({ "secret": SECRET }));
    set(&store, "web.env.URL", json!("pg://${{ PASSWORD }}@db"));
    assert_eq!(value(&store, "web.env.PASSWORD"), json!({ "secret": true }));
    let diff = store.read(&who(), &DiffQuery::default()).unwrap();
    let password = diff
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|row| row.path.as_str() == "web.env.PASSWORD")
        .unwrap();
    assert_eq!(password.after, json!({ "secret": true }));
    let id = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
    store
        .write_trusted(
            &who(),
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    let claimed = store
        .claim(&id, &RunnerId::parse("runner").unwrap())
        .unwrap();
    let web = claimed
        .intent
        .target
        .iter()
        .find(|spec| spec.name.as_str() == "web")
        .unwrap();
    let environment = json!(web.container.environment);
    assert_eq!(environment["PASSWORD"], SECRET);
    assert_eq!(environment["URL"], format!("pg://{SECRET}@db"));
    assert!(web.container.sealed_identity.is_none());
    assert!(!store.sealed_variables(&org()).unwrap());
}

#[test]
fn the_flag_is_on_only_with_a_row_and_teardown_of_the_organization_deletes_it() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    let other = OrganizationId::parse("other").unwrap();
    assert!(!store.sealed_variables(&org()).unwrap());
    turn_on(&url, "org");
    turn_on(&url, "other");
    assert!(store.sealed_variables(&org()).unwrap());
    assert!(store.sealed_variables(&other).unwrap());
    store.remove_organization(&who()).unwrap();
    assert!(!store.sealed_variables(&org()).unwrap());
    assert!(store.sealed_variables(&other).unwrap());
}
