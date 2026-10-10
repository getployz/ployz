//! Store migrations from the schema before them, on SQLite and on Postgres (see
//! `backend`): a database is written through the Store, put back to the older
//! schema by hand, and opened again.

use ployz_core::ServiceName;
use ployz_store::{
    Actor, BranchQuery, BranchView, Change, ConfigStore, CreateBranch, CreateProject,
    CreateService, Edit, EnvironmentId, EnvironmentName, EnvironmentRef, OrganizationId, ProjectId,
    ProjectName, ServiceLineageId, SettingPath, SyncChanges, SyncQuery, SyncView,
};
use serde_json::{Value, json};

mod backend;

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

fn at(environment: &str) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse(environment).unwrap()),
    }
}

fn set(store: &ConfigStore, who: &Actor, environment: &str, changes: &[(&str, Value)]) {
    store
        .write(
            who,
            &Edit {
                environment: at(environment),
                expect: None,
                changes: changes
                    .iter()
                    .map(|(path, value)| Change::Set {
                        path: SettingPath::parse(path).unwrap(),
                        value: value.clone(),
                    })
                    .collect(),
            },
        )
        .unwrap();
}

/// How Branch `fix-web` compares with its Parent, every way it can be read.
fn comparisons(store: &ConfigStore, who: &Actor) -> (SyncView, BranchView) {
    (
        store
            .read(
                who,
                &SyncQuery {
                    from: at("fix-web"),
                    into: None,
                    when: None,
                },
            )
            .unwrap(),
        store
            .read(
                who,
                &BranchQuery {
                    environment: at("fix-web"),
                },
            )
            .unwrap(),
    )
}

/// The schema as it was before `0002_sync`: one base per Branch.
const BEFORE_SYNC: &str = "
    UPDATE config_environment_branch SET made_with = (
        SELECT s.base FROM config_sync_base s
        WHERE (s.environment_id = config_environment_branch.environment_id
          AND s.other_id = config_environment_branch.parent_id)
          OR (s.environment_id = config_environment_branch.parent_id
          AND s.other_id = config_environment_branch.environment_id)
    );
    ALTER TABLE config_environment_branch RENAME COLUMN made_with TO base;
    DROP TABLE config_sync_arrival;
    DROP TABLE config_proposal;
    DELETE FROM config_migration WHERE name = '0005_proposal';
    DROP TABLE config_sync_base;
    DROP TABLE config_never_sync;
    DROP TABLE config_held_secret;
    ALTER TABLE config_conditional_sync RENAME COLUMN stored TO saved;
    ALTER TABLE config_conditional_sync RENAME COLUMN synced_at TO saved_at;
    ALTER TABLE config_conditional_sync RENAME TO config_conditional_save;
    ALTER TABLE config_waiting_deploy RENAME COLUMN syncs TO saves;
    DELETE FROM config_migration WHERE name = '0002_sync';
";

fn run(url: &str, script: &str) {
    match url.strip_prefix("sqlite:") {
        Some(path) => rusqlite::Connection::open(path)
            .unwrap()
            .execute_batch(script)
            .unwrap(),
        None => postgres::Client::connect(url, postgres::NoTls)
            .unwrap()
            .batch_execute(script)
            .unwrap(),
    }
}

#[test]
fn a_branch_compares_with_its_parent_as_before_the_sync_migration() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    let before = {
        let store = ConfigStore::open(&url, backend::key()).unwrap();
        store
            .write(
                &who,
                &CreateProject {
                    id: ProjectId::parse(uuid(1)).unwrap(),
                    name: ProjectName::parse("shop").unwrap(),
                    default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
                },
            )
            .unwrap();
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(uuid(3)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse("web").unwrap(),
                    image: Some("web:1".into()),
                    template: None,
                },
            )
            .unwrap();
        store
            .write(
                &who,
                &CreateBranch {
                    id: EnvironmentId::parse(uuid(4)).unwrap(),
                    from: at("production"),
                    name: EnvironmentName::parse("fix-web").unwrap(),
                    copy: vec![ployz_store::NodeName::parse("web").unwrap()],
                    live: Vec::new(),
                    setup: Vec::new(),
                    keep: false,
                    fix: None,
                },
            )
            .unwrap();
        set(
            &store,
            &who,
            "fix-web",
            &[
                ("web.image", json!("web:2")),
                ("web.env.NEW", json!("1")),
                ("web.env.MORE", json!("2")),
            ],
        );
        // A Sync moves the base past what the Branch was made with.
        let (view, _) = comparisons(&store, &who);
        let image = view
            .rows
            .iter()
            .find(|row| row.at.to_string() == "web.source")
            .unwrap();
        store
            .write(
                &who,
                &SyncChanges {
                    from: at("fix-web"),
                    into: None,
                    when: None,
                    version: view.version.clone(),
                    picks: Some(vec![image.at.row().clone().into()]),
                    skip: Vec::new(),
                    values: std::collections::BTreeMap::new(),
                    id: None,
                },
            )
            .unwrap();
        set(&store, &who, "production", &[("web.env.NEW", json!("0"))]);
        comparisons(&store, &who)
    };
    assert_eq!(before.0.rows.len(), 2);

    run(&url, BEFORE_SYNC);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    // A database from before Sync has no proposals: what arrived is unowned.
    let mut before = before;
    before.0.proposal = None;
    assert_eq!(comparisons(&store, &who), before);
}

#[test]
fn a_legacy_configs_service_can_be_renamed_without_losing_data_or_references() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
            },
        )
        .unwrap();
    for (n, name) in [(3, "web"), (4, "worker")] {
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(uuid(n)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some("nginx:1".into()),
                    template: None,
                },
            )
            .unwrap();
    }
    store
        .write(
            &who,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse("worker.env.SERVER").unwrap(),
                    value: json!("${{ web.PLOYZ_PRIVATE_DOMAIN }}"),
                }],
            },
        )
        .unwrap();
    drop(store);
    // An older Store admitted this name; only the display slug changes in its persisted document.
    run(
        &url,
        r#"UPDATE config_environment SET working = replace(working, '"slug":"web"', '"slug":"configs"')"#,
    );
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    let inspect = |name: &str| {
        store
            .read(
                &who,
                &ployz_store::ServiceQuery {
                    environment: EnvironmentRef::default(),
                    service: ServiceName::parse(name).unwrap(),
                },
            )
            .unwrap()
    };
    let before = inspect("configs");
    let mut worker = inspect("worker");
    store
        .write(
            &who,
            &ployz_store::RenameService {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("configs").unwrap(),
                name: ServiceName::parse("settings-service").unwrap(),
            },
        )
        .unwrap();
    let after = inspect("settings-service");
    assert_eq!(after.service.service.id, before.service.service.id);
    assert_eq!(
        after.service.service.private_dns,
        before.service.service.private_dns
    );
    assert_eq!(after.values, before.values);
    worker.values.insert(
        "env".into(),
        json!({ "SERVER": "${{ settings-service.PLOYZ_PRIVATE_DOMAIN }}" }),
    );
    assert_eq!(inspect("worker").values, worker.values);
}
