//! The two walks empty a plain copy of a sealed value from every place a Branch and
//! its Parent keep one, and take the tables in one order.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::walk::{empty_in_bases, empty_sealed_below};
use crate::branch::row_id;
use crate::storage::{Param, Row, Tx};
use crate::{
    Actor, Change, ConfigStore, CreateBranch, CreateProject, CreateService, Edit, EnvironmentId,
    EnvironmentName, EnvironmentRef, NodeName, OrganizationId, ProjectId, ProjectName, SealingKey,
    ServiceLineageId, SettingPath, Trusted,
};
use ployz_core::RpcError;
use ployz_core::ServiceName;

const TEXT: &str = "hunter2-plain";

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

/// Every statement a transaction ran, in order, over a real one.
struct Recording<'tx> {
    tx: &'tx mut dyn Tx,
    statements: Vec<String>,
}

impl Tx for Recording<'_> {
    fn execute(&mut self, sql: &str, params: &[Param<'_>]) -> Result<usize, RpcError> {
        self.statements.push(sql.to_owned());
        self.tx.execute(sql, params)
    }

    fn query(&mut self, sql: &str, params: &[Param<'_>]) -> Result<Vec<Row>, RpcError> {
        self.tx.query(sql, params)
    }

    fn batch(&mut self, sql: &str) -> Result<(), RpcError> {
        self.tx.batch(sql)
    }
}

/// What `planted` hands back.
struct Planted {
    store: ConfigStore,
    parent: EnvironmentId,
    branch: EnvironmentId,
    /// The variable's Service lineage and key.
    row: (String, String),
}

/// `production` holds `web.env.KEY` as plain text, and its Branch `feature` was made
/// with and shares it, with an arrival whose three cells hold it too. `production` then
/// drops the variable, as a seal that held it elsewhere would leave it.
fn planted() -> Planted {
    let store = ConfigStore::open("sqlite::memory:", SealingKey::new(b"test").unwrap()).unwrap();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    let parent = EnvironmentId::parse(uuid(2)).unwrap();
    let branch = EnvironmentId::parse(uuid(9)).unwrap();
    let lineage = uuid(3);
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: parent.clone(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse(lineage.clone()).unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    let edit = |change: Change| {
        store
            .write_trusted(
                &who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![change],
                },
                &Trusted::default(),
            )
            .unwrap();
    };
    edit(Change::Set {
        path: SettingPath::parse("web.env.KEY").unwrap(),
        value: json!(TEXT),
    });
    store
        .write(
            &who,
            &CreateBranch {
                id: branch.clone(),
                from: EnvironmentRef::default(),
                name: EnvironmentName::parse("feature").unwrap(),
                copy: vec![NodeName::parse("web").unwrap()],
                live: Vec::new(),
                setup: Vec::new(),
                keep: false,
                fix: None,
            },
        )
        .unwrap();
    edit(Change::Unset {
        path: SettingPath::parse("web.env.KEY").unwrap(),
    });
    let row = row_id(&lineage, "variables.KEY").unwrap();
    store
        .storage
        .write(|tx| {
            let made_with = tx.query(
                "SELECT made_with FROM config_environment_branch WHERE environment_id = ?1",
                &[branch.as_str().into()],
            )?;
            let fingerprint = made_with
                .first()
                .unwrap()
                .intent(0, "Branch")?
                .services
                .iter()
                .flat_map(|service| &service.variables)
                .find(|variable| variable.key == "KEY")
                .map(|variable| variable.value.stored_fingerprint().to_owned())
                .unwrap();
            let cell = json!({
                "value": { "kind": "literal", "value": TEXT, "fingerprint": fingerprint }
            })
            .to_string();
            tx.execute(
                "INSERT INTO config_sync_arrival (environment_id, other_id, lineage, at, \
                 organization_id, how, state, value, prior, was, sync_id, source) \
                 VALUES (?1, ?2, ?3, ?4, 'org', 'sync', 'pending', ?5, ?5, ?6, ?7, ?5)",
                &[
                    branch.as_str().into(),
                    parent.as_str().into(),
                    row.lineage().into(),
                    row.at().to_string().as_str().into(),
                    cell.as_str().into(),
                    json!({ "cell": "absent" }).to_string().as_str().into(),
                    uuid(30).as_str().into(),
                ],
            )?;
            Ok(())
        })
        .unwrap();
    Planted {
        store,
        parent,
        branch,
        row: (lineage, "KEY".to_owned()),
    }
}

/// The text of each store a walk reaches, by name.
fn stores(planted: &Planted) -> Vec<(&'static str, String)> {
    planted
        .store
        .storage
        .write(|tx| {
            let text = |rows: Vec<Row>, columns: &[usize]| -> Result<String, RpcError> {
                let mut all = String::new();
                for row in &rows {
                    for column in columns {
                        all.push_str(row.optional_text(*column)?.unwrap_or_default());
                    }
                }
                Ok(all)
            };
            let branch = planted.branch.as_str();
            Ok(vec![
                (
                    "a Branch's made-with document",
                    text(
                        tx.query(
                            "SELECT made_with FROM config_environment_branch WHERE environment_id = ?1",
                            &[branch.into()],
                        )?,
                        &[0],
                    )?,
                ),
                (
                    "an arrival's cells",
                    text(
                        tx.query(
                            "SELECT value, source, prior FROM config_sync_arrival WHERE environment_id = ?1",
                            &[branch.into()],
                        )?,
                        &[0, 1, 2],
                    )?,
                ),
                (
                    "a Sync base",
                    text(
                        tx.query(
                            "SELECT base FROM config_sync_base WHERE environment_id = ?1 OR other_id = ?1",
                            &[branch.into()],
                        )?,
                        &[0],
                    )?,
                ),
            ])
        })
        .unwrap()
}

fn rows_of(planted: &Planted) -> BTreeSet<(String, String)> {
    BTreeSet::from([planted.row.clone()])
}

/// The tables a statement list updates, in the order each is first updated.
fn tables_updated(statements: &[String]) -> Vec<&'static str> {
    let mut order = Vec::new();
    for statement in statements {
        for table in [
            "config_environment_branch",
            "config_sync_arrival",
            "config_sync_base",
        ] {
            if statement.starts_with(&format!("UPDATE {table} ")) && !order.contains(&table) {
                order.push(table);
            }
        }
    }
    order
}

fn walk_over(
    planted: &Planted,
    walk: impl FnOnce(&mut dyn Tx) -> Result<(), RpcError>,
) -> Vec<String> {
    planted
        .store
        .storage
        .write(|tx| {
            let mut recording = Recording {
                tx,
                statements: Vec::new(),
            };
            walk(&mut recording)?;
            Ok(recording.statements)
        })
        .unwrap()
}

#[test]
fn every_store_holds_the_plain_copy_before_a_walk() {
    for (name, text) in stores(&planted()) {
        assert!(text.contains(TEXT), "{name}: {text}");
    }
}

#[test]
fn the_by_key_walk_empties_the_copy_in_every_store_and_takes_them_in_order() {
    let planted = planted();
    let rows = rows_of(&planted);
    let statements = walk_over(&planted, |tx| {
        empty_in_bases(tx, planted.parent.as_str(), &rows)
    });
    for (name, text) in stores(&planted) {
        assert!(!text.contains(TEXT), "{name}: {text}");
        assert!(text.contains("v1:"), "{name} keeps the fingerprint: {text}");
    }
    assert_eq!(
        tables_updated(&statements),
        [
            "config_environment_branch",
            "config_sync_arrival",
            "config_sync_base"
        ]
    );
}

#[test]
fn the_by_fingerprint_walk_empties_the_copy_in_every_store_and_takes_them_in_order() {
    let planted = planted();
    let rows = rows_of(&planted);
    let statements = walk_over(&planted, |tx| {
        empty_sealed_below(tx, planted.parent.as_str(), &rows, None)
    });
    for (name, text) in stores(&planted) {
        assert!(!text.contains(TEXT), "{name}: {text}");
        assert!(text.contains("v1:"), "{name} keeps the fingerprint: {text}");
    }
    assert_eq!(
        tables_updated(&statements),
        [
            "config_environment_branch",
            "config_sync_arrival",
            "config_sync_base"
        ]
    );
}

#[test]
fn the_by_fingerprint_walk_leaves_a_value_with_another_fingerprint() {
    let planted = planted();
    let other = json!({
        "value": { "kind": "literal", "value": "someone-else", "fingerprint": "v1:other" }
    });
    planted
        .store
        .storage
        .write(|tx| {
            tx.execute(
                "UPDATE config_sync_arrival SET value = ?2 WHERE environment_id = ?1",
                &[
                    planted.branch.as_str().into(),
                    other.to_string().as_str().into(),
                ],
            )?;
            Ok(())
        })
        .unwrap();
    let rows = rows_of(&planted);
    walk_over(&planted, |tx| {
        empty_sealed_below(tx, planted.parent.as_str(), &rows, None)
    });
    let kept: Value = planted
        .store
        .storage
        .write(|tx| {
            let rows = tx.query(
                "SELECT value FROM config_sync_arrival WHERE environment_id = ?1",
                &[planted.branch.as_str().into()],
            )?;
            rows.first().unwrap().json(0, "Sync")
        })
        .unwrap();
    assert_eq!(kept, other);
}
