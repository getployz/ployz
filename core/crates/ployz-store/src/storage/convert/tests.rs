//! The 0009 rehearsal: a Store from before it, seeded by raw SQL with Conditional
//! Syncs a P2 Store wrote (`legacy.json`, sealed with the test key), opened again.
//! On SQLite, and on Postgres when `PLOYZ_STORE_TEST_POSTGRES` names a server.
//!
//! Sealed values are compared, never printed: asserts here name what differs only.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{Converted, convert, fault};
use crate::conditional_sync::Offer;
use crate::id::ProposalId;
use crate::storage::{Storage, Tx};

const BEFORE: &str = "0007_proposal_carried";
const LEGACY: &str = include_str!("legacy.json");
const ORG: &str = "org";
const PRODUCTION: &str = "00000000-0000-4000-8000-000000000002";
const STAGING: &str = "00000000-0000-4000-8000-000000000005";
const FROZEN: &str = "00000000-0000-4000-8000-000000000060";
const LANDED: &str = "00000000-0000-4000-8000-000000000061";
const LEGACY_TABLES: &[&str] = &[
    "config_conditional_sync",
    "config_held_secret",
    "config_waiting_deploy",
    "config_pull_request",
    "config_proposal",
    "config_sync_receipt",
    "config_migration",
];

struct Fixture {
    standing: Value,
    held: Value,
    pull_request: Value,
}

impl Fixture {
    fn load() -> Self {
        let legacy: Value = serde_json::from_str(LEGACY).unwrap();
        Self {
            standing: legacy["syncs"][0].clone(),
            held: legacy["held"][0].clone(),
            pull_request: legacy["pull_requests"][0].clone(),
        }
    }

    fn standing_id(&self) -> &str {
        self.standing["id"].as_str().unwrap()
    }

    fn pr_environment(&self) -> &str {
        self.standing["pr_environment_id"].as_str().unwrap()
    }

    fn stored(&self) -> &str {
        self.standing["stored"].as_str().unwrap()
    }

    fn held_key(&self) -> String {
        format!(
            "{}/{}",
            self.held["lineage"].as_str().unwrap(),
            self.held["at"].as_str().unwrap()
        )
    }

    fn held_value(&self) -> &str {
        self.held["value"].as_str().unwrap()
    }
}

/// A new, empty database.
fn fresh(dir: &tempfile::TempDir, name: &str) -> String {
    let Ok(server) = std::env::var("PLOYZ_STORE_TEST_POSTGRES") else {
        return format!("sqlite:{}", dir.path().join(format!("{name}.db")).display());
    };
    let database = format!("convert_{}", uuid::Uuid::new_v4().simple());
    postgres::Client::connect(&server, postgres::NoTls)
        .unwrap()
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    format!("{server}/{database}")
}

fn postgres() -> bool {
    std::env::var_os("PLOYZ_STORE_TEST_POSTGRES").is_some()
}

/// A Store at v0007 holding, from P2: C1 standing on production with a held value,
/// C2 frozen on staging with a held value and a waiting push carrying it, C3 landed,
/// and a held value no Conditional Sync takes.
fn legacy(dir: &tempfile::TempDir, name: &str, fixture: &Fixture) -> String {
    let url = fresh(dir, name);
    let storage = Storage::open_through(&url, BEFORE).unwrap();
    storage.write(|tx| seed(tx, fixture)).unwrap();
    url
}

fn seed(tx: &mut dyn Tx, fixture: &Fixture) -> Result<(), ployz_core::RpcError> {
    tx.execute(
        "INSERT INTO config_project (id, organization_id, name, default_environment_id) \
         VALUES ('00000000-0000-4000-8000-000000000001', ?1, 'shop', ?2)",
        &[ORG.into(), PRODUCTION.into()],
    )?;
    for (id, name) in [
        (PRODUCTION, "production"),
        (STAGING, "staging"),
        (fixture.pr_environment(), "pr-5"),
    ] {
        tx.execute(
            "INSERT INTO config_environment (id, organization_id, project_id, name, \
             working_revision, working) \
             VALUES (?1, ?2, '00000000-0000-4000-8000-000000000001', ?3, 1, '{}')",
            &[id.into(), ORG.into(), name.into()],
        )?;
    }
    let pr = &fixture.pull_request;
    let facts: Value = serde_json::from_str(pr["facts"].as_str().unwrap()).unwrap();
    // #6 closed before its facts named the merge commit: only its frozen Conditional
    // Sync knows it, and the conversion takes `merged` from there.
    for (number, open, merge) in [(5, true, None), (6, false, None), (7, false, Some("4"))] {
        let mut facts = facts.clone();
        facts["number"] = json!(number);
        facts["open"] = json!(open);
        if let Some(digit) = merge {
            facts["merge_commit"] = json!(digit.repeat(40));
            facts["merge_reached"] = json!(true);
        }
        tx.execute(
            "INSERT INTO config_pull_request (organization_id, repository_id, number, \
             facts, updated) VALUES (?1, ?2, ?3, ?4, ?5)",
            &[
                ORG.into(),
                pr["repository_id"].as_i64().unwrap().into(),
                number.into(),
                facts.to_string().as_str().into(),
                pr["updated"].as_str().unwrap().into(),
            ],
        )?;
    }
    let c1 = &fixture.standing;
    let row = |id: &str,
               into: &str,
               state: &str,
               pr: Option<&str>,
               number: i64,
               merge: Option<String>| {
        (
            id.to_owned(),
            into.to_owned(),
            state.to_owned(),
            pr.map(str::to_owned),
            number,
            merge,
        )
    };
    let rows = [
        row(
            fixture.standing_id(),
            c1["environment_id"].as_str().unwrap(),
            "standing",
            Some(fixture.pr_environment()),
            c1["number"].as_i64().unwrap(),
            None,
        ),
        row(FROZEN, STAGING, "frozen", None, 6, Some("3".repeat(40))),
        row(LANDED, PRODUCTION, "landed", None, 7, Some("4".repeat(40))),
    ];
    for (id, into, state, pr, number, merge) in &rows {
        tx.execute(
            "INSERT INTO config_conditional_sync (id, organization_id, environment_id, state, \
             pr_environment_id, repository_id, number, target_branch, working_revision, \
             merge_commit, synced_at, stored) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            &[
                id.as_str().into(),
                ORG.into(),
                into.as_str().into(),
                state.as_str().into(),
                pr.as_deref().into(),
                c1["repository_id"].as_i64().unwrap().into(),
                (*number).into(),
                c1["target_branch"].as_str().unwrap().into(),
                c1["working_revision"].as_i64().unwrap().into(),
                merge.as_deref().into(),
                c1["synced_at"].as_i64().unwrap().into(),
                fixture.stored().into(),
            ],
        )?;
    }
    let held = &fixture.held;
    for (into, number, at) in [
        (
            held["environment_id"].as_str().unwrap(),
            held["number"].as_i64().unwrap(),
            held["at"].as_str().unwrap(),
        ),
        (STAGING, 6, held["at"].as_str().unwrap()),
        (PRODUCTION, 9, "variables.ORPHAN"),
    ] {
        tx.execute(
            "INSERT INTO config_held_secret (environment_id, repository_id, number, lineage, \
             at, organization_id, value) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            &[
                into.into(),
                held["repository_id"].as_i64().unwrap().into(),
                number.into(),
                held["lineage"].as_str().unwrap().into(),
                at.into(),
                ORG.into(),
                fixture.held_value().into(),
            ],
        )?;
    }
    for (into, syncs) in [(STAGING, json!([FROZEN])), (PRODUCTION, json!([]))] {
        tx.execute(
            "INSERT INTO config_waiting_deploy (environment_id, repository_id, branch, \
             organization_id, head, services, syncs) VALUES (?1, 11, 'main', ?2, ?3, '[]', ?4)",
            &[
                into.into(),
                ORG.into(),
                "a".repeat(40).as_str().into(),
                syncs.to_string().as_str().into(),
            ],
        )?;
    }
    Ok(())
}

type Dump = BTreeMap<String, (Vec<String>, Vec<String>)>;

/// Every named table's columns and rows, rows sorted.
fn dump(url: &str, tables: &[&str]) -> Dump {
    let storage = Storage::open_through(url, BEFORE).unwrap();
    storage
        .read(|tx| {
            let mut dump = Dump::new();
            for table in tables {
                let columns = columns(tx, table)?;
                if columns.is_empty() {
                    continue;
                }
                let mut rows: Vec<String> = tx
                    .query(&format!("SELECT * FROM {table}"), &[])?
                    .into_iter()
                    .map(|row| format!("{:?}", row.0))
                    .collect();
                rows.sort();
                dump.insert((*table).to_owned(), (columns, rows));
            }
            Ok(dump)
        })
        .unwrap()
}

fn columns(tx: &mut dyn Tx, table: &str) -> Result<Vec<String>, ployz_core::RpcError> {
    let sql = if postgres() {
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = ?1 ORDER BY ordinal_position"
    } else {
        "SELECT name FROM pragma_table_info(?1) ORDER BY cid"
    };
    tx.query(sql, &[table.into()])?
        .iter()
        .map(|row| row.text(0).map(str::to_owned))
        .collect()
}

/// Every table the Store has.
fn all_tables(url: &str) -> Vec<String> {
    let storage = Storage::open_through(url, BEFORE).unwrap();
    let sql = if postgres() {
        "SELECT table_name::text FROM information_schema.tables \
         WHERE table_schema = current_schema() ORDER BY table_name"
    } else {
        "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name"
    };
    storage
        .read(|tx| {
            tx.query(sql, &[])?
                .iter()
                .map(|row| row.text(0).map(str::to_owned))
                .collect()
        })
        .unwrap()
}

fn dump_all(url: &str) -> Dump {
    let tables = all_tables(url);
    let tables: Vec<&str> = tables.iter().map(String::as_str).collect();
    dump(url, &tables)
}

fn run(url: &str, sql: &str) {
    let storage = Storage::open_through(url, BEFORE).unwrap();
    storage.write(|tx| tx.batch(sql)).unwrap();
}

/// The dump of a clean, never-failed conversion of the fixture.
fn clean(dir: &tempfile::TempDir, fixture: &Fixture) -> Dump {
    let url = legacy(dir, "clean", fixture);
    Storage::open(&url).unwrap();
    dump_all(&url)
}

#[test]
fn a_stop_after_any_row_leaves_the_legacy_store_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::load();
    let url = legacy(&dir, "store", &fixture);
    let before = dump(&url, LEGACY_TABLES);
    assert!(before["config_conditional_sync"].1.len() == 3);
    for k in 0..=3 {
        fault::after(Some(k));
        let failed = Storage::open(&url);
        fault::after(None);
        let error = failed.err().expect("the conversion stops");
        assert!(error.message.contains("injected stop"), "{}", error.message);
        assert!(
            dump(&url, LEGACY_TABLES) == before,
            "the legacy Store changed after a stop at row {k}"
        );
    }
    Storage::open(&url).unwrap();
    let converted = dump_all(&url);
    assert!(
        converted == clean(&dir, &fixture),
        "a repaired conversion differs from a clean one"
    );
    Storage::open(&url).unwrap();
    assert!(
        dump_all(&url) == converted,
        "a third open changed the Store"
    );
}

#[test]
fn every_pending_conditional_sync_becomes_an_offer_of_what_it_stored() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::load();
    let url = legacy(&dir, "store", &fixture);
    Storage::open(&url).unwrap();
    let tables = all_tables(&url);
    for gone in ["config_conditional_sync", "config_held_secret"] {
        assert!(
            !tables.iter().any(|table| table == gone),
            "{gone} is still there"
        );
    }
    let storage = Storage::open(&url).unwrap();
    storage
        .read(|tx| {
            let waiting = columns(tx, "config_waiting_deploy")?;
            assert!(!waiting.iter().any(|column| column == "syncs"));
            assert_eq!(
                tx.query("SELECT COUNT(*) FROM config_waiting_deploy", &[])?[0].int(0)?,
                2
            );
            let offers = tx.query(
                "SELECT id, environment_id, source_environment_id, number, source_name, \
                 source_revision, first_sync, last_sync, offered \
                 FROM config_proposal ORDER BY id",
                &[],
            )?;
            let ids: Vec<&str> = offers.iter().map(|row| row.text(0).unwrap()).collect();
            assert_eq!(ids, [FROZEN, fixture.standing_id()]);
            let stored: Value = serde_json::from_str(fixture.stored()).unwrap();
            let source = stored["environment"]["id"].as_str().unwrap();
            for (row, (into, from, number, held)) in offers.iter().zip([
                (STAGING, source, 6, 1),
                (PRODUCTION, fixture.pr_environment(), 5, 1),
            ]) {
                let id = row.text(0)?;
                assert_eq!(row.text(1)?, into);
                assert_eq!(row.text(2)?, from);
                assert_eq!(row.int(3)?, number);
                assert_eq!(
                    row.text(4)?,
                    stored["environment"]["name"].as_str().unwrap()
                );
                assert_eq!(
                    row.int(5)?,
                    fixture.standing["working_revision"].as_i64().unwrap()
                );
                assert_eq!((row.text(6)?, row.text(7)?), (id, id));
                let offered: Value = serde_json::from_str(row.text(8)?).unwrap();
                assert!(
                    offered["stored"].as_str() == Some(fixture.stored()),
                    "offer {id} doesn't hold what its Conditional Sync stored"
                );
                let values = offered["held"].as_object().unwrap();
                assert_eq!(values.len(), held);
                assert!(
                    values.get(&fixture.held_key()).and_then(Value::as_str)
                        == Some(fixture.held_value()),
                    "offer {id} doesn't hold its held value as sealed"
                );
                let offer = Offer::decode(&ProposalId::parse(id).unwrap(), row.text(8)?)?;
                assert_eq!(offer.held.len(), held);
                assert!(!offer.stored.picks.is_empty());
            }
            let receipts = tx.query(
                "SELECT sync_id, environment_id, proposal_id FROM config_sync_receipt \
                 ORDER BY sync_id",
                &[],
            )?;
            let receipts: Vec<(&str, &str, &str)> = receipts
                .iter()
                .map(|row| {
                    (
                        row.text(0).unwrap(),
                        row.text(1).unwrap(),
                        row.text(2).unwrap(),
                    )
                })
                .collect();
            assert_eq!(
                receipts,
                [
                    (FROZEN, STAGING, FROZEN),
                    (fixture.standing_id(), PRODUCTION, fixture.standing_id())
                ]
            );
            let prs = tx.query(
                "SELECT number, facts, merged FROM config_pull_request ORDER BY number",
                &[],
            )?;
            let merged: Vec<(i64, Option<Value>)> = prs
                .iter()
                .map(|row| {
                    let facts: Value = serde_json::from_str(row.text(1).unwrap()).unwrap();
                    assert!(facts.get("merge_reached").is_none());
                    let merged = row
                        .optional_text(2)
                        .unwrap()
                        .map(|merged| serde_json::from_str(merged).unwrap());
                    (row.int(0).unwrap(), merged)
                })
                .collect();
            let main = fixture.standing["target_branch"].as_str().unwrap();
            assert_eq!(
                merged,
                [
                    (5, None),
                    (6, Some(json!({ "commit": "3".repeat(40), "into": main }))),
                    (7, Some(json!({ "commit": "4".repeat(40), "into": main }))),
                ]
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn the_conversion_counts_what_it_drops() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::load();
    let url = legacy(&dir, "store", &fixture);
    let storage = Storage::open_through(&url, "0008_proposal_offer").unwrap();
    let converted = storage.write(|tx| convert(tx)).unwrap();
    assert_eq!(
        converted,
        Converted {
            standing: 1,
            frozen: 1,
            landed_dropped: 1,
            orphan_held: 1,
            retired_attachments: 1,
        }
    );
}

#[test]
fn an_unreadable_conditional_sync_stops_the_open_naming_it_until_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::load();
    for broken in [
        format!("UPDATE config_conditional_sync SET stored = 'not json' WHERE id = '{FROZEN}'"),
        format!("UPDATE config_conditional_sync SET state = 'gone' WHERE id = '{FROZEN}'"),
    ] {
        let url = legacy(&dir, "store", &fixture);
        run(&url, &broken);
        let before = dump(&url, LEGACY_TABLES);
        let error = Storage::open(&url).err().expect("the open fails closed");
        assert!(error.message.contains(FROZEN), "{}", error.message);
        assert!(
            dump(&url, LEGACY_TABLES) == before,
            "a failed open changed the Store"
        );
        let mut repaired = String::new();
        if broken.contains("stored") {
            repaired = format!(
                "UPDATE config_conditional_sync SET stored = '{}' WHERE id = '{FROZEN}'",
                fixture.stored().replace('\'', "''")
            );
        } else {
            repaired.push_str(&format!(
                "UPDATE config_conditional_sync SET state = 'frozen' WHERE id = '{FROZEN}'"
            ));
        }
        run(&url, &repaired);
        Storage::open(&url).unwrap();
        assert!(
            dump_all(&url) == clean(&dir, &fixture),
            "a repaired open differs from a clean one"
        );
        if !postgres() {
            std::fs::remove_file(url.strip_prefix("sqlite:").unwrap()).unwrap();
            std::fs::remove_file(dir.path().join("clean.db")).unwrap();
        }
    }
}

#[test]
fn a_proposal_already_there_stops_the_open_naming_both() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::load();
    let url = legacy(&dir, "store", &fixture);
    let included = "00000000-0000-4000-8000-000000000070";
    run(
        &url,
        &format!(
            "INSERT INTO config_proposal (id, organization_id, environment_id, \
             source_environment_id, repository_id, number, source_name, source_revision, \
             first_sync, last_sync) VALUES ('{included}', '{ORG}', '{PRODUCTION}', \
             '{}', 11, 5, 'pr-5', 1, '{included}', '{included}')",
            fixture.pr_environment()
        ),
    );
    let before = dump(&url, LEGACY_TABLES);
    let error = Storage::open(&url).err().expect("the open fails closed");
    assert!(
        error.message.contains(fixture.standing_id()),
        "{}",
        error.message
    );
    assert!(error.message.contains(included), "{}", error.message);
    assert!(
        dump(&url, LEGACY_TABLES) == before,
        "a failed open changed the Store"
    );
}

/// The legacy table keys no pull request: a row standing beside a frozen one for the
/// same Destination and pull request stops the open, naming both, until one goes.
#[test]
fn two_conditional_syncs_on_one_pull_request_stop_the_open_naming_both() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::load();
    let url = legacy(&dir, "store", &fixture);
    let twin = "00000000-0000-4000-8000-000000000062";
    run(
        &url,
        &format!(
            "INSERT INTO config_conditional_sync (id, organization_id, environment_id, state, \
             pr_environment_id, repository_id, number, target_branch, working_revision, \
             merge_commit, synced_at, stored) \
             SELECT '{twin}', organization_id, environment_id, 'standing', NULL, repository_id, \
             number, target_branch, working_revision, NULL, synced_at, stored \
             FROM config_conditional_sync WHERE id = '{FROZEN}'"
        ),
    );
    let before = dump(&url, LEGACY_TABLES);
    let error = Storage::open(&url).err().expect("the open fails closed");
    assert!(error.message.contains(FROZEN), "{}", error.message);
    assert!(error.message.contains(twin), "{}", error.message);
    assert!(error.message.contains("delete one"), "{}", error.message);
    assert!(
        dump(&url, LEGACY_TABLES) == before,
        "a failed open changed the Store"
    );
    run(
        &url,
        &format!("DELETE FROM config_conditional_sync WHERE id = '{twin}'"),
    );
    Storage::open(&url).unwrap();
    assert!(
        dump_all(&url) == clean(&dir, &fixture),
        "a repaired open differs from a clean one"
    );
}

/// Only a pull request's proposal is offered: the Store refuses an offer without one.
#[test]
fn an_offer_without_a_pull_request_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::load();
    let url = legacy(&dir, "store", &fixture);
    let storage = Storage::open(&url).unwrap();
    let insert = |number: &str| {
        storage.write(|tx| {
            tx.execute(
                &format!(
                    "INSERT INTO config_proposal (id, organization_id, environment_id, \
                     source_environment_id, repository_id, number, source_name, \
                     source_revision, first_sync, last_sync, offered) \
                     VALUES ('00000000-0000-4000-8000-000000000071', ?1, ?2, ?3, \
                     {number}, {number}, 'staging', 1, 'a', 'a', '{{}}')"
                ),
                &[ORG.into(), PRODUCTION.into(), STAGING.into()],
            )
        })
    };
    assert!(
        insert("NULL").is_err(),
        "an offer without a pull request was stored"
    );
    insert("8").unwrap();
}

#[test]
fn opens_at_once_convert_once() {
    if !postgres() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::load();
    let url = legacy(&dir, "store", &fixture);
    let opens: Vec<_> = (0..4)
        .map(|_| {
            let url = url.clone();
            std::thread::spawn(move || Storage::open(&url).map(|_| ()))
        })
        .collect();
    for open in opens {
        open.join().unwrap().unwrap();
    }
    assert!(
        dump_all(&url) == clean(&dir, &fixture),
        "concurrent opens differ from a clean one"
    );
}
