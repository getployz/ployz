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
    DROP TABLE config_sync_receipt;
    DELETE FROM config_migration WHERE name = '0006_sync_receipt';
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

/// Every row `sql` selects at `url`, each column as text (`NULL` for none).
fn select(url: &str, sql: &str) -> Vec<Vec<String>> {
    match url.strip_prefix("sqlite:") {
        Some(path) => {
            let connection = rusqlite::Connection::open(path).unwrap();
            let mut statement = connection.prepare(sql).unwrap();
            let columns = statement.column_count();
            statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| {
                            row.get::<_, Option<String>>(index)
                                .map(|cell| cell.unwrap_or_else(|| "NULL".into()))
                        })
                        .collect()
                })
                .unwrap()
                .map(Result::unwrap)
                .collect()
        }
        None => postgres::Client::connect(url, postgres::NoTls)
            .unwrap()
            .query(sql, &[])
            .unwrap()
            .iter()
            .map(|row| {
                (0..row.len())
                    .map(|index| {
                        row.get::<_, Option<String>>(index)
                            .unwrap_or_else(|| "NULL".into())
                    })
                    .collect()
            })
            .collect(),
    }
}

/// The schema as it was before `0005_proposal`, with arrivals a Store of then wrote:
/// pending and settled, with and without a Sync, two of one Sync.
const BEFORE_PROPOSAL: &str = "
    DROP TABLE config_sync_receipt;
    DROP TABLE config_sync_arrival;
    DROP TABLE config_proposal;
    DELETE FROM config_migration
    WHERE name IN ('0005_proposal', '0006_sync_receipt', '0007_proposal_carried');
    CREATE TABLE config_sync_arrival (
        environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
        other_id TEXT NOT NULL,
        lineage TEXT NOT NULL,
        at TEXT NOT NULL,
        organization_id TEXT NOT NULL,
        how TEXT NOT NULL,
        state TEXT NOT NULL,
        value TEXT NOT NULL,
        prior TEXT,
        was TEXT,
        sync_id TEXT,
        PRIMARY KEY (environment_id, other_id, lineage, at)
    );
    INSERT INTO config_sync_arrival VALUES
    ('00000000-0000-4000-8000-000000000002', '00000000-0000-4000-8000-000000000004',
     '00000000-0000-4000-8000-000000000003', 'env.A', 'org', 'sync', 'pending',
     '\"1\"', '\"0\"', '\"0\"', '00000000-0000-4000-8000-000000000101'),
    ('00000000-0000-4000-8000-000000000002', '00000000-0000-4000-8000-000000000004',
     '00000000-0000-4000-8000-000000000003', 'env.B', 'org', 'sync', 'pending',
     '\"2\"', '\"0\"', '\"0\"', '00000000-0000-4000-8000-000000000101'),
    ('00000000-0000-4000-8000-000000000002', '00000000-0000-4000-8000-000000000004',
     '00000000-0000-4000-8000-000000000003', 'env.C', 'org', 'sync', 'settled',
     '\"3\"', NULL, NULL, '00000000-0000-4000-8000-000000000102'),
    ('00000000-0000-4000-8000-000000000004', '00000000-0000-4000-8000-000000000002',
     '00000000-0000-4000-8000-000000000003', 'env.D', 'org', 'follow', 'settled',
     '\"4\"', NULL, NULL, NULL),
    ('00000000-0000-4000-8000-000000000004', '00000000-0000-4000-8000-000000000002',
     '00000000-0000-4000-8000-000000000003', 'env.E', 'org', 'sync', 'pending',
     '\"5\"', '\"0\"', NULL, NULL);
";

#[test]
fn legacy_arrivals_migrate_unowned_with_their_syncs_receipted() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    {
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
    }
    run(&url, BEFORE_PROPOSAL);
    let legacy = "SELECT environment_id, other_id, lineage, at, organization_id, how, state, \
                  value, prior, was, sync_id FROM config_sync_arrival ORDER BY at";
    let before = select(&url, legacy);
    assert_eq!(before.len(), 5);

    ConfigStore::open(&url, backend::key()).unwrap();
    assert_eq!(select(&url, legacy), before);
    let owners = select(
        &url,
        "SELECT proposal_id, source FROM config_sync_arrival ORDER BY at",
    );
    assert_eq!(owners, vec![vec!["NULL".to_owned(), "NULL".to_owned()]; 5]);
    let receipts = select(
        &url,
        "SELECT organization_id, sync_id, environment_id, source_environment_id, proposal_id \
         FROM config_sync_receipt ORDER BY sync_id",
    );
    let receipt = |sync: u8| {
        vec![
            "org".to_owned(),
            format!("00000000-0000-4000-8000-000000000{sync}"),
            uuid(2),
            uuid(4),
            "NULL".to_owned(),
        ]
    };
    assert_eq!(receipts, [receipt(101), receipt(102)]);
}
