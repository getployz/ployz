//! Store migrations from the schema before them, on SQLite and on Postgres (see
//! `backend`): a database is written through the Store, put back to the older
//! schema by hand, and opened again.

use ployz_core::ServiceName;
use ployz_store::{
    Actor, BranchQuery, BranchView, Change, ConfigStore, CreateBranch, CreateProject,
    CreateService, Edit, EnvironmentId, EnvironmentName, EnvironmentRef, Move, MovePick, MoveQuery,
    MoveView, OrganizationId, ProjectId, ProjectName, Save, ServiceLineageId, SettingPath,
    SyncQuery, SyncView,
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
fn comparisons(store: &ConfigStore, who: &Actor) -> (SyncView, MoveView, MoveView, BranchView) {
    let save = MoveQuery::Save {
        from: at("fix-web"),
        into: None,
        when: None,
    };
    let update = MoveQuery::Update {
        into: at("fix-web"),
    };
    (
        store
            .read(
                who,
                &SyncQuery {
                    from: at("fix-web"),
                    into: None,
                },
            )
            .unwrap(),
        store.read(who, &save).unwrap(),
        store.read(who, &update).unwrap(),
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
        WHERE s.environment_id = config_environment_branch.environment_id
          AND s.other_id = config_environment_branch.parent_id
    );
    ALTER TABLE config_environment_branch RENAME COLUMN made_with TO base;
    DROP TABLE config_sync_pending;
    DROP TABLE config_sync_base;
    DROP TABLE config_never_sync;
    DROP TABLE config_followed;
    ALTER TABLE config_conditional_sync RENAME TO config_conditional_save;
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
        // A Save moves the base past what the Branch was made with.
        store
            .write(
                &who,
                &Move::Save(Save {
                    from: at("fix-web"),
                    picks: Some(vec![MovePick {
                        row: "web.image".into(),
                        choice: None,
                    }]),
                    ..Save::default()
                }),
            )
            .unwrap();
        set(&store, &who, "production", &[("web.env.NEW", json!("0"))]);
        comparisons(&store, &who)
    };
    assert_eq!(before.0.rows.len(), 2);

    run(&url, BEFORE_SYNC);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    assert_eq!(comparisons(&store, &who), before);
}
