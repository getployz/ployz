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

/// The schema as it was before `0008_proposal_offer`, with no Conditional Syncs.
const BEFORE_OFFERS: &str = "
    ALTER TABLE config_pull_request DROP COLUMN merged;
    DROP TABLE config_merge_value;
    DROP TABLE config_merge_deploy;
    ALTER TABLE config_branch DROP COLUMN recent;
    ALTER TABLE config_pull_request DROP COLUMN unlanded;
    CREATE TABLE config_conditional_sync (
        id TEXT PRIMARY KEY,
        organization_id TEXT NOT NULL,
        environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
        state TEXT NOT NULL,
        pr_environment_id TEXT REFERENCES config_environment (id) ON DELETE CASCADE,
        repository_id BIGINT NOT NULL,
        number BIGINT NOT NULL,
        target_branch TEXT NOT NULL,
        working_revision BIGINT NOT NULL,
        merge_commit TEXT,
        synced_at BIGINT NOT NULL,
        stored TEXT NOT NULL
    );
    CREATE TABLE config_held_secret (
        environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
        repository_id BIGINT NOT NULL,
        number BIGINT NOT NULL,
        lineage TEXT NOT NULL,
        at TEXT NOT NULL,
        organization_id TEXT NOT NULL,
        value TEXT NOT NULL,
        PRIMARY KEY (environment_id, repository_id, number, lineage, at)
    );
    CREATE TABLE config_waiting_deploy (
        environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
        repository_id BIGINT NOT NULL,
        branch TEXT NOT NULL,
        organization_id TEXT NOT NULL,
        head TEXT NOT NULL,
        services TEXT NOT NULL,
        syncs TEXT NOT NULL DEFAULT '[]',
        PRIMARY KEY (environment_id, repository_id, branch)
    );
    DELETE FROM config_migration WHERE name IN
    ('0008_proposal_offer', '0009_offer_conditional_syncs', '0010_drop_conditional_sync',
    '0011_retire_offers', '0012_on_merge', '0014_merge_removed_services');
";

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
                    close_after: false,
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

    run(&url, &format!("{BEFORE_OFFERS}{BEFORE_SYNC}"));
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
    run(&url, &format!("{BEFORE_OFFERS}{BEFORE_PROPOSAL}"));
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

/// The arrival table as `0005_proposal` left it, before `0037_arrival_checks`, with
/// `rows` stored in it.
fn before_arrival_checks(rows: &str) -> String {
    format!(
        "
    CREATE TABLE config_sync_arrival_before (
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
        proposal_id TEXT,
        source TEXT,
        PRIMARY KEY (environment_id, other_id, lineage, at),
        CHECK (
            proposal_id IS NULL
            OR (
                how = 'sync' AND state = 'pending' AND prior IS NOT NULL AND was IS NOT NULL
                AND sync_id IS NOT NULL AND source IS NOT NULL
            )
        ),
        FOREIGN KEY (environment_id, proposal_id, other_id)
            REFERENCES config_proposal (environment_id, id, source_environment_id)
            ON UPDATE CASCADE
    );
    INSERT INTO config_sync_arrival_before SELECT environment_id, other_id, lineage, at,
    organization_id, how, state, value, prior, was, sync_id, proposal_id, source
    FROM config_sync_arrival;
    DROP TABLE config_sync_arrival;
    ALTER TABLE config_sync_arrival_before RENAME TO config_sync_arrival;
    CREATE UNIQUE INDEX config_sync_arrival_owner
        ON config_sync_arrival (environment_id, lineage, at)
        WHERE proposal_id IS NOT NULL;
    DELETE FROM config_migration WHERE name = '0037_arrival_checks';
    INSERT INTO config_sync_arrival
    (environment_id, other_id, lineage, at, how, state, value, prior, was, sync_id, source,
     organization_id) VALUES {rows};
"
    )
}

/// A shop's Store at `url` from before `0037_arrival_checks`, holding arrivals in
/// production `rows`, each `(at, how, state, value, prior, was, sync_id, source)`.
fn stored_before_arrival_checks(url: &str, rows: &[[&str; 8]]) {
    drop(shop_at(url));
    let rows: Vec<String> = rows
        .iter()
        .map(|row| {
            let cells: Vec<String> = row
                .iter()
                .map(|cell| match *cell {
                    "NULL" => "NULL".to_owned(),
                    cell => format!("'{cell}'"),
                })
                .collect();
            format!(
                "('{}', 'other', 'lineage', {}, 'org')",
                uuid(2),
                cells.join(", ")
            )
        })
        .collect();
    run(url, &before_arrival_checks(&rows.join(", ")));
}

/// Each arrival at `url`, by `at`: its `how`, `state`, `prior`, `was`, `sync_id`
/// and `source`.
fn arrival_columns(url: &str) -> Vec<Vec<String>> {
    select(
        url,
        "SELECT at, how, state, prior, was, sync_id, source FROM config_sync_arrival ORDER BY at",
    )
}

#[test]
fn arrival_checks_repair_what_a_store_could_have_stored_without_inventing_a_value() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    stored_before_arrival_checks(
        &url,
        &[
            // Pending and unclaimed, with no `was` kept.
            [
                "env.A", "sync", "pending", "\"1\"", "\"0\"", "NULL", "NULL", "\"1\"",
            ],
            // A Use hint with a `prior`, a `was` and a Sync's `how`.
            [
                "env.B", "sync", "hint", "\"2\"", "\"0\"", "\"0\"", "NULL", "\"2\"",
            ],
            // Settled, with a `prior`, its Sync and its source.
            [
                "env.C", "sync", "settled", "\"3\"", "\"0\"", "NULL", "s", "\"3\"",
            ],
        ],
    );

    ConfigStore::open(&url, backend::key()).unwrap();
    let unkept = serde_json::to_string(&ployz_core::config::SealedCell::Cell(
        ployz_core::config::Cell::Absent,
    ))
    .unwrap();
    let row = |cells: [&str; 7]| cells.map(str::to_owned).to_vec();
    assert_eq!(
        arrival_columns(&url),
        [
            row([
                "env.A", "sync", "pending", "\"0\"", &unkept, "NULL", "\"1\""
            ]),
            row(["env.B", "follow", "hint", "NULL", "NULL", "NULL", "\"2\""]),
            row(["env.C", "sync", "settled", "NULL", "NULL", "s", "\"3\""]),
        ]
    );
}

#[test]
fn arrival_checks_keep_a_row_they_cannot_repair_and_refuse_a_write_that_leaves_it_so() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let rows = [
        // Pending and Released by a Sync, with no `prior`.
        [
            "env.D", "sync", "pending", "\"4\"", "NULL", "\"0\"", "s", "NULL",
        ],
        // A state no Store writes.
        [
            "env.E", "follow", "gone", "\"5\"", "\"0\"", "\"0\"", "NULL", "NULL",
        ],
    ];
    stored_before_arrival_checks(&url, &rows);
    let before = arrival_columns(&url);

    ConfigStore::open(&url, backend::key()).unwrap();
    assert_eq!(
        select(
            &url,
            "SELECT name FROM config_migration WHERE name = '0037_arrival_checks'"
        ),
        [["0037_arrival_checks".to_owned()]]
    );
    assert_eq!(arrival_columns(&url), before);
    let refused = try_execute(
        &url,
        "UPDATE config_sync_arrival SET was = '\"9\"' WHERE at = 'env.D'",
    )
    .unwrap_err();
    assert!(refused.contains("config_sync_arrival_pending"), "{refused}");
    try_execute(&url, "DELETE FROM config_sync_arrival WHERE at = 'env.D'").unwrap();
}

/// Run `sql` on the database at `url`, beside the Store: Ok, or the database's
/// message.
fn try_execute(url: &str, sql: &str) -> Result<(), String> {
    match url.strip_prefix("sqlite:") {
        Some(path) => rusqlite::Connection::open(path)
            .unwrap()
            .execute(sql, [])
            .map(drop)
            .map_err(|error| error.to_string()),
        None => postgres::Client::connect(url, postgres::NoTls)
            .unwrap()
            .execute(sql, &[])
            .map(drop)
            .map_err(|error| {
                error
                    .as_db_error()
                    .map_or_else(|| error.to_string(), |db| db.message().to_owned())
            }),
    }
}

#[test]
fn arrival_checks_refuse_each_illegal_row_and_allow_its_nearest_legal_one() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    drop(shop_at(&url));
    let insert = |at: &str, columns: &str, values: &str| {
        try_execute(
            &url,
            &format!(
                "INSERT INTO config_sync_arrival \
                 (environment_id, other_id, lineage, at, organization_id, {columns}) \
                 VALUES ('{}', 'other', 'lineage', '{at}', 'org', {values})",
                uuid(2)
            ),
        )
    };
    // Each CHECK, a row that breaks only it, and the legal row nearest it.
    let checks = [
        (
            "config_sync_arrival_state",
            ("how, state, value", "'follow', 'gone', '1'"),
            ("how, state, value", "'follow', 'settled', '1'"),
        ),
        (
            "config_sync_arrival_how",
            ("how, state, value", "'merge', 'settled', '1'"),
            ("how, state, value", "'sync', 'settled', '1'"),
        ),
        (
            "config_sync_arrival_pending",
            (
                "how, state, value, was, sync_id",
                "'sync', 'pending', '1', '0', 's'",
            ),
            (
                "how, state, value, prior, was, sync_id",
                "'sync', 'pending', '1', '0', '0', 's'",
            ),
        ),
        (
            "config_sync_arrival_done",
            ("how, state, value, prior", "'sync', 'settled', '1', '0'"),
            (
                "how, state, value, sync_id, source",
                "'sync', 'settled', '1', 's', '1'",
            ),
        ),
        (
            "config_sync_arrival_hint",
            ("how, state, value", "'sync', 'hint', '1'"),
            ("how, state, value, sync_id", "'follow', 'hint', '1', 's'"),
        ),
    ];
    let mut seen = Vec::new();
    for (n, (check, (refused, refused_values), (allowed, allowed_values))) in
        checks.iter().enumerate()
    {
        let refusal = insert(&format!("env.R{n}"), refused, refused_values);
        let names = refusal.as_ref().is_err_and(|error| error.contains(check));
        let allowed = insert(&format!("env.A{n}"), allowed, allowed_values);
        seen.push((*check, names, allowed));
    }
    let expected: Vec<(&str, bool, Result<(), String>)> = checks
        .iter()
        .map(|(check, _, _)| (*check, true, Ok(())))
        .collect();
    assert_eq!(seen, expected);
}

/// The schema as it was before `0011_retire_offers`, with an offered proposal and a
/// plain one, each with a receipt.
const BEFORE_RETIRE: &str = "
    DROP TABLE config_merge_value;
    DROP TABLE config_merge_deploy;
    ALTER TABLE config_branch DROP COLUMN recent;
    ALTER TABLE config_pull_request DROP COLUMN unlanded;
    ALTER TABLE config_proposal ADD COLUMN offered TEXT
        CHECK (offered IS NULL OR number IS NOT NULL);
    DELETE FROM config_migration
    WHERE name IN ('0011_retire_offers', '0012_on_merge', '0014_merge_removed_services');
    INSERT INTO config_proposal (id, organization_id, environment_id, source_environment_id,
        repository_id, number, source_name, source_revision, first_sync, last_sync, offered)
    VALUES
    ('00000000-0000-4000-8000-000000000201', 'org', '00000000-0000-4000-8000-000000000002',
     '00000000-0000-4000-8000-000000000004', NULL, NULL, 'fix-web', 1,
     '00000000-0000-4000-8000-000000000101', '00000000-0000-4000-8000-000000000101', NULL),
    ('00000000-0000-4000-8000-000000000202', 'org', '00000000-0000-4000-8000-000000000002',
     '00000000-0000-4000-8000-000000000005', 11, 5, 'pr-5', 1,
     '00000000-0000-4000-8000-000000000102', '00000000-0000-4000-8000-000000000102', '{}');
    INSERT INTO config_sync_receipt
        (organization_id, sync_id, environment_id, source_environment_id, proposal_id)
    VALUES
    ('org', '00000000-0000-4000-8000-000000000101', '00000000-0000-4000-8000-000000000002',
     '00000000-0000-4000-8000-000000000004', '00000000-0000-4000-8000-000000000201'),
    ('org', '00000000-0000-4000-8000-000000000102', '00000000-0000-4000-8000-000000000002',
     '00000000-0000-4000-8000-000000000005', '00000000-0000-4000-8000-000000000202');
";

#[test]
fn an_offered_proposal_goes_with_its_receipt_when_offers_retire() {
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
    }
    run(&url, BEFORE_RETIRE);

    ConfigStore::open(&url, backend::key()).unwrap();
    assert_eq!(
        select(&url, "SELECT id FROM config_proposal"),
        [["00000000-0000-4000-8000-000000000201".to_owned()]]
    );
    assert_eq!(
        select(&url, "SELECT proposal_id FROM config_sync_receipt"),
        [["00000000-0000-4000-8000-000000000201".to_owned()]]
    );
}

/// The schema as it was before `0015_sealed_variable`: no ledger of seals.
const BEFORE_SEALED_LEDGER: &str = "
    DROP TABLE config_sealed_variable;
    DELETE FROM config_migration WHERE name = '0015_sealed_variable';
";

#[test]
fn a_seal_only_working_state_held_at_upgrade_keeps_history_from_plain_text() {
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
        set(
            &store,
            &who,
            "production",
            &[("web.env.TOKEN", json!("plain-value"))],
        );
        publish(&store, &who);
        set(
            &store,
            &who,
            "production",
            &[(
                "web.env.TOKEN",
                backend::sealed_secret(&uuid(2), &uuid(3), "TOKEN", 1),
            )],
        );
    }
    run(&url, BEFORE_SEALED_LEDGER);
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    store
        .write(
            &who,
            &Edit {
                environment: at("production"),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("web.env.TOKEN").unwrap(),
                }],
            },
        )
        .unwrap();
    publish(&store, &who);
    let restore = store.read(
        &who,
        &ployz_store::HistoryPreviewQuery {
            environment: at("production"),
            revision: ployz_store::Revision(1),
            action: ployz_store::HistoryAction::Restore,
            change: None,
        },
    );
    match restore {
        Err(error) => assert_eq!(error.message, "TOKEN was sealed. Set it again."),
        Ok(preview) => assert!(
            !serde_json::to_string(&preview)
                .unwrap()
                .contains("plain-value"),
            "Restoring the plain revision brought back the value sealed at upgrade"
        ),
    }
}

/// Project shop, its production holding Service web.
fn shop_at(url: &str) -> (ConfigStore, Actor) {
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    let store = ConfigStore::open(url, backend::key()).unwrap();
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
    (store, who)
}

fn unset(store: &ConfigStore, who: &Actor, path: &str) {
    store
        .write(
            who,
            &Edit {
                environment: at("production"),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse(path).unwrap(),
                }],
            },
        )
        .unwrap();
}

/// TOKEN sealed in production's web under a new variable ID: the Save seals what
/// History holds of the TOKEN before it by key, under that variable's own ID.
fn seal_token_again(store: &ConfigStore, who: &Actor) {
    unset(store, who, "web.env.TOKEN");
    set(
        store,
        who,
        "production",
        &[(
            "web.env.TOKEN",
            backend::sealed_secret(&uuid(2), &uuid(3), "TOKEN", 1),
        )],
    );
    publish(store, who);
}

/// [`shop_at`], with web holding TOKEN plain at revision 1, then sealed under a new
/// variable ID at revision 2, which seals revision 1's TOKEN by key.
fn sealed_by_key(url: &str) -> ConfigStore {
    let (store, who) = shop_at(url);
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-value"))],
    );
    publish(&store, &who);
    seal_token_again(&store, &who);
    store
}

/// A scrub that seals an earlier version's variable by key, under that variable's
/// own ID, records the seal: the ledger alone answers whether it was ever sealed.
#[test]
fn a_seal_a_scrub_writes_into_history_is_in_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let _store = sealed_by_key(&url);
    assert_eq!(backend::unrecorded_seals(&url), Vec::<Vec<String>>::new());
}

/// A database whose saves hold seals the ledger lacks, as one an older Store wrote,
/// has every one of them in the ledger once `0033_record_saved_seals` runs.
#[test]
fn every_seal_a_save_holds_is_in_the_ledger_after_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    drop(sealed_by_key(&url));
    run(
        &url,
        "DELETE FROM config_sealed_variable;
         DELETE FROM config_migration WHERE name = '0033_record_saved_seals';",
    );
    assert_ne!(backend::unrecorded_seals(&url), Vec::<Vec<String>>::new());
    drop(ConfigStore::open(&url, backend::key()).unwrap());
    assert_eq!(backend::unrecorded_seals(&url), Vec::<Vec<String>>::new());
}

/// A Store from before 0033, still running beside one that migrated their shared
/// database, seals a value by key without recording it. A plain copy a Deployment
/// kept still leaves History when that Deployment ends: the saves are read too.
#[test]
fn a_seal_a_store_from_before_0033_left_unrecorded_still_scrubs_history() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let (store, who) = shop_at(&url);
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-private"))],
    );
    publish(&store, &who);
    set(
        &store,
        &who,
        "production",
        &[("web.env.OTHER", json!("other"))],
    );
    let queued = backend::admit(&store, &who, "production", 1);
    seal_token_again(&store, &who);
    assert_eq!(backend::unrecorded_seals(&url), Vec::<Vec<String>>::new());
    // The older Store records only what Working State holds: TOKEN's newest ID.
    let working: ployz_core::config::SavedEnvironmentIntent = serde_json::from_str(
        &select(&url, "SELECT working FROM config_environment")
            .concat()
            .concat(),
    )
    .unwrap();
    let token = working
        .services
        .iter()
        .flat_map(|service| &service.variables)
        .find(|variable| variable.key == "TOKEN")
        .unwrap();
    run(
        &url,
        &format!(
            "DELETE FROM config_sealed_variable WHERE variable_id <> '{}';",
            token.id
        ),
    );
    assert_eq!(backend::unrecorded_seals(&url).len(), 1);
    unset(&store, &who, "web.env.TOKEN");
    set(
        &store,
        &who,
        "production",
        &[("web.env.TOKEN", json!("plain-again"))],
    );
    publish(&store, &who);
    assert!(backend::tables_holding(&url, "plain-private").contains(&"config_saved".to_owned()));

    backend::run(&store, &queued);
    assert!(!backend::tables_holding(&url, "plain-private").contains(&"config_saved".to_owned()));
}

#[test]
fn a_push_held_for_ci_lives_on_its_deployment_after_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    drop(ConfigStore::open(&url, backend::key()).unwrap());
    let (tables, columns, indexes, required) = if url.starts_with("sqlite:") {
        (
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'config_%'",
            "SELECT name FROM pragma_table_info('config_deployment') WHERE name LIKE 'push_%'",
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name IN \
             ('config_deployment_hold_branch', 'config_deployment_hold_head', \
             'config_check_suite_head') ORDER BY name",
            "SELECT CAST(\"notnull\" AS TEXT) FROM pragma_table_info('config_deployment') \
             WHERE name = 'number'",
        )
    } else {
        (
            "SELECT table_name::text FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name LIKE 'config_%'",
            "SELECT column_name::text FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = 'config_deployment' \
             AND column_name LIKE 'push_%'",
            "SELECT indexname::text FROM pg_indexes WHERE schemaname = current_schema() \
             AND indexname IN ('config_deployment_hold_branch', 'config_deployment_hold_head', \
             'config_check_suite_head') ORDER BY indexname",
            "SELECT (is_nullable = 'NO')::int::text FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = 'config_deployment' \
             AND column_name = 'number'",
        )
    };
    let tables: Vec<String> = select(&url, tables).into_iter().flatten().collect();
    assert!(tables.iter().any(|table| table == "config_deployment"));
    assert!(!tables.iter().any(|table| table == "config_waiting_deploy"));
    let mut columns: Vec<String> = select(&url, columns).into_iter().flatten().collect();
    columns.sort();
    assert_eq!(
        columns,
        [
            "push_branch",
            "push_head",
            "push_repository_id",
            "push_services"
        ]
    );
    // A Deployment waiting for CI has no number until it queues.
    let required: Vec<String> = select(&url, required).into_iter().flatten().collect();
    assert_eq!(required, ["0"]);
    let indexes: Vec<String> = select(&url, indexes).into_iter().flatten().collect();
    assert_eq!(
        indexes,
        [
            "config_check_suite_head",
            "config_deployment_hold_branch",
            "config_deployment_hold_head"
        ]
    );
}

fn publish(store: &ConfigStore, who: &Actor) {
    store
        .write_trusted(
            who,
            &ployz_store::Publish {
                id: None,
                environment: at("production"),
                version: None,
                message: None,
                accept_volume_loss: Vec::new(),
            },
            &ployz_store::Trusted {
                approval: ployz_store::Approval::NotRequired,
                ..ployz_store::Trusted::default()
            },
        )
        .unwrap();
}

/// The receiver's own an older Store kept in a Follow's arrival after the Parent
/// sealed it.
#[test]
fn a_plain_value_kept_in_a_follows_arrival_is_gone_after_upgrade() {
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
        backend::deploy(&store, &who, "production", 1);
        store
            .write(
                &who,
                &CreateBranch {
                    id: EnvironmentId::parse(uuid(9)).unwrap(),
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
            "production",
            &[("web.env.TOKEN", json!("plain-private"))],
        );
        backend::deploy(&store, &who, "production", 2);
        set(
            &store,
            &who,
            "production",
            &[(
                "web.env.TOKEN",
                backend::sealed_secret(&uuid(2), &uuid(3), "TOKEN", 1),
            )],
        );
        backend::deploy(&store, &who, "production", 3);
        run(
            &url,
            &format!(
                "UPDATE config_sync_arrival \
                 SET was = '{{\"cell\":{{\"value\":{{\"kind\":\"literal\",\"value\":\"plain-private\"}}}}}}' \
                 WHERE environment_id = '{}' AND state = 'pending';
                 DELETE FROM config_migration WHERE name = '0028_unkept_arrival_was';",
                uuid(9),
            ),
        );
        assert_eq!(
            backend::tables_holding(&url, "plain-private"),
            ["config_sync_arrival"]
        );
    }
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    assert!(backend::tables_holding(&url, "plain-private").is_empty());
    let incoming: Vec<String> = store
        .read(
            &who,
            &ployz_store::DiffQuery {
                environment: at("fix-web"),
            },
        )
        .unwrap()
        .incoming
        .iter()
        .map(|change| format!("{} {}", change.at, change.from))
        .collect();
    assert_eq!(incoming, ["web.env.TOKEN production"]);
}

/// What landed that an older Store kept plain in an arrival after its sender sealed it.
#[test]
fn a_plain_value_landed_in_an_arrival_is_gone_after_upgrade() {
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
        backend::deploy(&store, &who, "production", 1);
        store
            .write(
                &who,
                &CreateBranch {
                    id: EnvironmentId::parse(uuid(9)).unwrap(),
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
            "production",
            &[("web.env.TOKEN", json!("plain-private"))],
        );
        backend::deploy(&store, &who, "production", 2);
        set(
            &store,
            &who,
            "production",
            &[(
                "web.env.TOKEN",
                backend::sealed_secret(&uuid(2), &uuid(3), "TOKEN", 1),
            )],
        );
        let (empty, plain) = (
            r#""kind":"literal","value":"""#,
            r#""kind":"literal","value":"plain-private""#,
        );
        run(
            &url,
            &format!(
                "UPDATE config_sync_arrival SET value = replace(value, '{empty}', '{plain}');
                 DELETE FROM config_migration WHERE name = '0030_scrub_sealed_arrivals';",
            ),
        );
        assert!(
            backend::tables_holding(&url, "plain-private")
                .contains(&"config_sync_arrival".to_owned())
        );
    }
    let _store = ConfigStore::open(&url, backend::key()).unwrap();
    assert!(
        !backend::tables_holding(&url, "plain-private").contains(&"config_sync_arrival".to_owned())
    );
}

/// The plain value an older Store kept in a Branch's Sync bases after its Parent
/// sealed it.
#[test]
fn a_plain_value_kept_in_a_branchs_sync_bases_is_gone_after_upgrade() {
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
        set(
            &store,
            &who,
            "production",
            &[("web.env.TOKEN", json!("plain-private"))],
        );
        backend::deploy(&store, &who, "production", 1);
        store
            .write(
                &who,
                &CreateBranch {
                    id: EnvironmentId::parse(uuid(9)).unwrap(),
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
            "production",
            &[(
                "web.env.TOKEN",
                backend::sealed_secret(&uuid(2), &uuid(3), "TOKEN", 1),
            )],
        );
        backend::deploy(&store, &who, "production", 2);
        let (empty, plain) = (
            r#""kind":"literal","value":"""#,
            r#""kind":"literal","value":"plain-private""#,
        );
        run(
            &url,
            &format!(
                "UPDATE config_environment_branch SET made_with = replace(made_with, '{empty}', '{plain}');
                 UPDATE config_sync_arrival SET prior = replace(prior, '{empty}', '{plain}');
                 DELETE FROM config_migration WHERE name = '0029_scrub_sealed_bases';",
            ),
        );
        assert_eq!(
            backend::tables_holding(&url, "plain-private"),
            [
                "config_environment_branch",
                "config_node_introduction",
                "config_sync_arrival"
            ]
        );
    }
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    assert_eq!(
        backend::tables_holding(&url, "plain-private"),
        ["config_node_introduction"]
    );
    let incoming: Vec<String> = store
        .read(
            &who,
            &ployz_store::DiffQuery {
                environment: at("fix-web"),
            },
        )
        .unwrap()
        .incoming
        .iter()
        .map(|change| format!("{} {}", change.at, change.from))
        .collect();
    assert_eq!(incoming, ["web.env.TOKEN production"]);
}

/// The plain value an older Store kept in a Branch's History after the Branch took
/// its Parent's seal of it and added it again plain.
#[test]
fn a_plain_value_sealed_then_added_back_is_gone_from_history_after_upgrade() {
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
        set(
            &store,
            &who,
            "production",
            &[("web.env.TOKEN", json!("plain-private"))],
        );
        backend::deploy(&store, &who, "production", 1);
        store
            .write(
                &who,
                &CreateBranch {
                    id: EnvironmentId::parse(uuid(9)).unwrap(),
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
        backend::deploy(&store, &who, "fix-web", 2);
        set(
            &store,
            &who,
            "production",
            &[(
                "web.env.TOKEN",
                backend::sealed_secret(&uuid(2), &uuid(3), "TOKEN", 1),
            )],
        );
        backend::deploy(&store, &who, "production", 3);
        store
            .write(
                &who,
                &Edit {
                    environment: at("fix-web"),
                    expect: None,
                    changes: vec![Change::Unset {
                        path: SettingPath::parse("web.env.TOKEN").unwrap(),
                    }],
                },
            )
            .unwrap();
        set(
            &store,
            &who,
            "fix-web",
            &[("web.env.TOKEN", json!("plain-new"))],
        );
        run(
            &url,
            "CREATE TABLE kept_saved AS SELECT environment_id, revision, intent FROM config_saved;",
        );
        backend::deploy(&store, &who, "fix-web", 4);
        run(
            &url,
            "UPDATE config_saved SET intent = (SELECT intent FROM kept_saved \
               WHERE kept_saved.environment_id = config_saved.environment_id \
               AND kept_saved.revision = config_saved.revision) \
             WHERE EXISTS (SELECT 1 FROM kept_saved \
               WHERE kept_saved.environment_id = config_saved.environment_id \
               AND kept_saved.revision = config_saved.revision);
             DROP TABLE kept_saved;
             DELETE FROM config_migration WHERE name = '0031_scrub_sealed_history';",
        );
        assert!(
            backend::tables_holding(&url, "plain-private").contains(&"config_saved".to_owned())
        );
    }
    let _store = ConfigStore::open(&url, backend::key()).unwrap();
    assert!(!backend::tables_holding(&url, "plain-private").contains(&"config_saved".to_owned()));
    assert!(backend::tables_holding(&url, "plain-new").contains(&"config_saved".to_owned()));
}

/// A Branch made before `0032_node_copied_from` makes its Own Copy after it, and
/// the owner's later seal of a variable it deleted from the copy reads sealed.
#[test]
fn an_own_copy_made_after_upgrade_never_shows_a_value_its_owner_sealed_since() {
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
        for (n, name) in [(3, "web"), (4, "db")] {
            store
                .write(
                    &who,
                    &CreateService {
                        id: ServiceLineageId::parse(uuid(n)).unwrap(),
                        environment: EnvironmentRef::default(),
                        name: ServiceName::parse(name).unwrap(),
                        image: Some(format!("{name}:1")),
                        template: None,
                    },
                )
                .unwrap();
        }
        set(
            &store,
            &who,
            "production",
            &[
                ("web.env.DB_URL", json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}")),
                ("db.env.TOKEN", json!("plain-private")),
            ],
        );
        backend::deploy(&store, &who, "production", 1);
        store
            .write(
                &who,
                &CreateBranch {
                    id: EnvironmentId::parse(uuid(9)).unwrap(),
                    from: at("production"),
                    name: EnvironmentName::parse("fix-web").unwrap(),
                    copy: vec![ployz_store::NodeName::parse("web").unwrap()],
                    live: vec![ployz_store::NodeName::parse("db").unwrap()],
                    setup: Vec::new(),
                    keep: false,
                    fix: None,
                },
            )
            .unwrap();
        backend::deploy(&store, &who, "fix-web", 2);
        run(
            &url,
            "DROP TABLE config_node_copied_from;
             DELETE FROM config_migration WHERE name = '0032_node_copied_from';",
        );
    }
    let store = ConfigStore::open(&url, backend::key()).unwrap();
    store
        .write(
            &who,
            &ployz_store::CopyNode {
                environment: at("fix-web"),
                node: ServiceName::parse("db").unwrap(),
                expect: None,
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &Edit {
                environment: at("fix-web"),
                expect: None,
                changes: vec![Change::Unset {
                    path: SettingPath::parse("db.env.TOKEN").unwrap(),
                }],
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "production",
        &[(
            "db.env.TOKEN",
            backend::sealed_secret(&uuid(2), &uuid(4), "TOKEN", 1),
        )],
    );
    backend::deploy(&store, &who, "production", 3);
    let review = store
        .read(
            &who,
            &ployz_store::DiffQuery {
                environment: at("fix-web"),
            },
        )
        .unwrap();
    let token = review
        .changes
        .iter()
        .flat_map(|change| &change.settings)
        .find(|setting| setting.path == "db.env.TOKEN")
        .unwrap();
    assert_eq!(
        (token.before.clone(), token.after.clone()),
        (json!({ "secret": true }), Value::Null)
    );
}

/// Each Deployment a Store from before `0034_deployment_ended` took out of flight
/// without stamping its end gets one when the Store upgrades, and no other row
/// changes.
#[test]
fn every_deployment_that_left_flight_records_when_it_ended_after_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let runner = ployz_store::RunnerId::parse("runner").unwrap();
    let id = |n: u8| format!("00000000-0000-4000-8000-0000000001{n:02}");
    // Deployment `n`'s status, ended, admitted, lease and started columns.
    let row = |n: u8| -> [String; 5] {
        select(
            &url,
            &format!(
                "SELECT status, CAST(ended AS TEXT), CAST(admitted AS TEXT), \
                 CAST(lease AS TEXT), CAST(started AS TEXT) FROM config_deployment \
                 WHERE id = '{}'",
                id(n)
            ),
        )
        .remove(0)
        .try_into()
        .unwrap()
    };
    let kept = {
        let (store, who) = shop_at(&url);
        // #1 superseded from the queue by #2, which applies.
        backend::admit(&store, &who, "production", 1);
        backend::deploy(&store, &who, "production", 2);
        // #3 abandoned after preparing.
        let three = backend::admit(&store, &who, "production", 3);
        let claimed = store.claim(&three, &runner).unwrap();
        backend::confirm(&store, &three, &claimed, &[], 0);
        store
            .record(&three, &runner, ployz_store::RunEvidence::Abandoned)
            .unwrap();
        // #4 prepared, taken over by #5, then records it was interrupted.
        let four = backend::admit(&store, &who, "production", 4);
        let claimed = store.claim(&four, &runner).unwrap();
        backend::confirm(&store, &four, &claimed, &[], 0);
        let five = backend::admit(&store, &who, "production", 5);
        store.claim(&five, &runner).unwrap();
        let interrupted: ployz_core::DeployOutcome<ployz_core::ExecutionError> =
            serde_json::from_value(json!({
                "type": "interrupted",
                // The one operation `backend::confirm` prepared, for web.
                "completed": [{"type": "remove_container", "machine_id": "a".repeat(32),
                               "container_id": format!("{:064x}", 0)}],
                "unexecuted": []
            }))
            .unwrap();
        store
            .record(
                &four,
                &runner,
                ployz_store::RunEvidence::Executed {
                    progress: Vec::new(),
                    outcome: Box::new(interrupted),
                    removed: Vec::new(),
                },
            )
            .unwrap();
        // #5 taken over live by #6, whose runner lease then lapses unwritten.
        let six = backend::admit(&store, &who, "production", 6);
        store.claim(&six, &runner).unwrap();
        // #7: a hold a newer push superseded.
        run(
            &url,
            &format!(
                "INSERT INTO config_deployment (id, organization_id, environment_id, status, \
                 saved_revision, services, nodes, namespace, run, credentials, admitted, \
                 ended, listed) \
                 SELECT '{}', organization_id, environment_id, 'superseded', saved_revision, \
                 services, '[]', namespace, run, credentials, admitted, admitted + 5, 100 \
                 FROM config_deployment WHERE id = '{}'",
                id(7),
                id(1)
            ),
        );
        // What a Store from before 0034 left.
        run(
            &url,
            &format!(
                "UPDATE config_deployment SET lease = lease - 3600 WHERE id = '{three}';
                 UPDATE config_deployment SET started = started - 3600, lease = started - 3000 \
                 WHERE id = '{six}';
                 UPDATE config_deployment SET ended = NULL \
                 WHERE id IN ('{}', '{three}', '{five}');
                 DELETE FROM config_migration WHERE name = '0034_deployment_ended';",
                id(1)
            ),
        );
        [2, 4, 6, 7].map(|n| (n, row(n)))
    };
    let [_, ended, ..] = row(5);
    assert_eq!(ended, "NULL");
    drop(ConfigStore::open(&url, backend::key()).unwrap());
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let int = |cell: &str| cell.parse::<i64>().unwrap();
    // Superseded from the queue: its admission.
    let [status, ended, admitted, ..] = row(1);
    assert_eq!((status.as_str(), ended), ("superseded", admitted));
    // Unknown, its runner lease past: that lease.
    let [status, ended, _, lease, _] = row(3);
    assert_eq!((status.as_str(), ended), ("unknown", lease));
    // Unknown, taken over while its runner lease held: when the Store upgraded.
    let [status, ended, _, lease, started] = row(5);
    assert_eq!(status, "unknown");
    let (ended, lease, started) = (int(&ended), int(&lease), int(&started));
    assert!(
        started <= ended && ended <= now && ended < lease,
        "{ended} {lease} {started}"
    );
    // Applied, interrupted after a takeover, lapsed but still `running`, and a
    // superseded hold: as they were.
    let [(_, applied), (_, interrupted), (_, lapsed), (_, hold)] = &kept;
    assert_eq!(
        [applied, interrupted, lapsed, hold]
            .map(|[status, ended, ..]| (status.as_str(), ended == "NULL")),
        [
            ("applied", false),
            ("unknown", false),
            ("running", true),
            ("superseded", false)
        ]
    );
    assert_eq!(kept.clone().map(|(n, _)| (n, row(n))), kept);
}

/// Each build its Servers held Building under a Deployment a Store from before
/// `0035_build_interrupted` took out of flight, and that shows an end, reads Failed
/// and interrupted when the Store upgrades, finished at that end. No other build
/// changes, opening the Store again changes nothing, and an older Store's writes and
/// reads still work on the table.
#[test]
fn every_build_its_deployment_ended_under_is_interrupted_after_upgrade() {
    const MESSAGE: &str =
        "The deployment ended or lost its runner before this build reported how it ended.";
    let dir = tempfile::tempdir().unwrap();
    let url = backend::fresh_url(&dir);
    let id = |n: u8| format!("00000000-0000-4000-8000-0000000001{n:02}");
    let now = || {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap()
    };
    let before = now();
    let (claimed, live) = (
        format!("runner = 'cloud-1', started = {}", before - 7200),
        before + 3600,
    );
    // Deployment `n`'s columns, and the build of web under it: status, message,
    // GitHub run and finished, as SQL.
    type Build = (&'static str, &'static str, &'static str, String);
    let cases: [(u8, String, Build); 12] = [
        (
            1,
            format!(
                "status = 'unknown', {claimed}, lease = {}, ended = NULL",
                before - 3600
            ),
            ("building", "NULL", "NULL", "NULL".into()),
        ),
        (
            2,
            format!(
                "status = 'running', {claimed}, lease = {}, ended = NULL",
                before - 3600
            ),
            ("building", "NULL", "NULL", "NULL".into()),
        ),
        (
            3,
            format!("status = 'failed', {claimed}, ended = {}", before - 100),
            ("building", "NULL", "NULL", "NULL".into()),
        ),
        (
            4,
            format!("status = 'cancelled', {claimed}, ended = {}", before - 50),
            ("building", "NULL", "NULL", "NULL".into()),
        ),
        (
            5,
            format!("status = 'failed', {claimed}, ended = {}", before - 100),
            ("building", "NULL", "NULL", (before - 500).to_string()),
        ),
        (
            6,
            format!("status = 'unknown', {claimed}, lease = {live}, ended = NULL"),
            ("building", "NULL", "NULL", "NULL".into()),
        ),
        (
            7,
            format!("status = 'running', {claimed}, lease = {live}, ended = NULL"),
            ("building", "NULL", "NULL", "NULL".into()),
        ),
        (
            8,
            format!("status = 'cancelling', {claimed}, lease = {live}, ended = NULL"),
            ("building", "NULL", "NULL", "NULL".into()),
        ),
        (
            9,
            format!("status = 'cancelled', {claimed}, ended = {}", before - 50),
            ("building", "NULL", "'{\"run\": 9001}'", "NULL".into()),
        ),
        (
            10,
            format!("status = 'failed', {claimed}, ended = {}", before - 100),
            ("pending", "NULL", "NULL", "NULL".into()),
        ),
        (
            11,
            format!("status = 'failed', {claimed}, ended = {}", before - 100),
            (
                "failed",
                "'exit status 1'",
                "NULL",
                (before - 200).to_string(),
            ),
        ),
        (
            12,
            "status = 'failed', runner = NULL, started = NULL, lease = 0, ended = NULL".into(),
            ("building", "NULL", "NULL", "NULL".into()),
        ),
    ];
    {
        let (store, who) = shop_at(&url);
        for (n, ..) in &cases {
            backend::admit(&store, &who, "production", *n);
        }
    }
    // The schema before 0035, and the rows a Store from before it left.
    run(
        &url,
        "ALTER TABLE config_build DROP COLUMN interrupted;
         DELETE FROM config_migration WHERE name = '0035_build_interrupted';",
    );
    for (n, deployment, (status, message, github, finished)) in &cases {
        run(
            &url,
            &format!(
                "UPDATE config_deployment SET number = {n}, {deployment} WHERE id = '{id}';
                 INSERT INTO config_build (deployment_id, service, organization_id, commit_sha, \
                 status, message, log, github, started, finished) \
                 SELECT id, 'web', organization_id, NULL, '{status}', {message}, '', {github}, \
                 {}, {finished} FROM config_deployment WHERE id = '{id}';",
                before - 7000,
                id = id(*n),
            ),
        );
    }
    drop(ConfigStore::open(&url, backend::key()).unwrap());
    let after = now();
    let builds = || {
        select(
            &url,
            "SELECT deployment_id, service, status, CAST(interrupted AS TEXT), message, \
             github, CAST(started AS TEXT), CAST(finished AS TEXT) FROM config_build \
             ORDER BY deployment_id, service",
        )
    };
    let seen: Vec<_> = builds()
        .into_iter()
        .map(|row| {
            let [_, _, status, interrupted, message, _, _, finished]: [String; 8] =
                row.try_into().unwrap();
            (status, interrupted, message, finished)
        })
        .collect();
    let settled = |finished: i64| {
        (
            "failed".to_string(),
            "1".to_string(),
            MESSAGE.to_string(),
            finished.to_string(),
        )
    };
    let kept = |status: &str, message: &str, finished: &str| {
        (
            status.to_string(),
            "0".to_string(),
            message.to_string(),
            finished.to_string(),
        )
    };
    let upgraded: i64 = seen
        .get(5)
        .and_then(|(.., finished)| finished.parse().ok())
        .unwrap_or(0);
    assert_eq!(
        seen,
        [
            // Unknown, its runner lease past: that lease.
            settled(before - 3600),
            // Still `running` past its runner lease: that lease.
            settled(before - 3600),
            // Failed and Cancelled: when they ended.
            settled(before - 100),
            settled(before - 50),
            // Finished already: when it did.
            settled(before - 500),
            // Unknown, taken over while its runner lease held: when the Store upgraded.
            settled(upgraded),
            // Live, held by GitHub, never started, failed on its own, and under a row
            // no Store writes that shows no end: as they were.
            kept("building", "NULL", "NULL"),
            kept("building", "NULL", "NULL"),
            kept("building", "NULL", "NULL"),
            kept("pending", "NULL", "NULL"),
            kept("failed", "exit status 1", &(before - 200).to_string()),
            kept("building", "NULL", "NULL"),
        ]
    );
    assert!(
        (before..=after).contains(&upgraded),
        "{before} {upgraded} {after}"
    );
    // A Store from before 0035 names the columns it writes and reads (`build.rs`
    // `pin`, `record` and `rows`), so it adds a build as before, which the new
    // column's default leaves unmarked, and still reads the table.
    run(
        &url,
        &format!(
            "INSERT INTO config_build \
             (deployment_id, service, organization_id, commit_sha, status, message, log) \
             SELECT id, 'api', organization_id, NULL, 'pending', NULL, '' \
             FROM config_deployment WHERE id = '{}' \
             ON CONFLICT (deployment_id, service) DO NOTHING",
            id(7)
        ),
    );
    let older = select(
        &url,
        &format!(
            "SELECT service, commit_sha, status, message, log, github, organization_id, \
             CAST(started AS TEXT), CAST(finished AS TEXT), server, github_skipped \
             FROM config_build WHERE deployment_id = '{}' ORDER BY service",
            id(7)
        ),
    );
    assert_eq!(
        older
            .iter()
            .filter_map(|row| row.get(2).map(String::as_str))
            .collect::<Vec<_>>(),
        ["pending", "building"]
    );
    let all = builds();
    assert_eq!(
        all.iter()
            .filter(|row| row.get(1).is_some_and(|service| service == "api"))
            .filter_map(|row| row.get(3).map(String::as_str))
            .collect::<Vec<_>>(),
        ["0"]
    );
    // Opening the Store again runs nothing more.
    drop(ConfigStore::open(&url, backend::key()).unwrap());
    assert_eq!(builds(), all);
}
