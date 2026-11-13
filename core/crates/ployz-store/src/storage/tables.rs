//! The tables the Store empties by `organization_id` when it removes an Organization
//! or forgets a Cluster, because no foreign key does it for them.
//!
//! A new `config_*` table that holds an Organization's rows and is not reached by a
//! cascade from its Project or Environment belongs in one of these lists; the test
//! below fails until it is listed, or named as one that needs no sweep.

/// Rows an Organization leaves behind once its Projects are gone: none of them hangs
/// off a Project or an Environment by a cascading key.
pub(crate) const ORGANIZATION_TABLES: [&str; 7] = [
    "config_create",
    "config_branch",
    "config_check_suite",
    "config_build_order",
    "config_pull_request",
    "config_sync_receipt",
    "config_removal_mark",
];

/// Applied State and Namespaces: what a Cluster's Servers hold, which forgetting the
/// Cluster clears while the Environments stay.
pub(crate) const CLUSTER_TABLES: [&str; 2] = ["config_applied", "config_namespace"];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{CLUSTER_TABLES, ORGANIZATION_TABLES};
    use crate::storage::Storage;

    /// Tables neither list names, and why they need no sweep.
    const NO_SWEEP: [&str; 4] = [
        // Removing an Organization refuses while it has Projects, so none is left to delete.
        "config_project",
        // The Store's own record of applied migrations, not an Organization's rows.
        "config_migration",
        // Keyed by an Environment no Organization removal can reach: none is left once
        // its Projects are gone.
        "config_auto_deploy_rule",
        "config_preview_rule",
    ];

    /// Every `config_*` table is swept by a list, reached by a cascade from the
    /// Project, Environment or Deployment it hangs off, or named in `NO_SWEEP`.
    fn assert_every_table_is_accounted_for(tables: &[String], cascading: &BTreeSet<String>) {
        let listed: BTreeSet<&str> = ORGANIZATION_TABLES
            .iter()
            .chain(&CLUSTER_TABLES)
            .chain(&NO_SWEEP)
            .copied()
            .collect();
        let unaccounted: Vec<&String> = tables
            .iter()
            .filter(|table| !listed.contains(table.as_str()) && !cascading.contains(*table))
            .collect();
        assert!(
            unaccounted.is_empty(),
            "no cascade or list sweeps {unaccounted:?} when an Organization is removed"
        );
        for table in ORGANIZATION_TABLES.iter().chain(&CLUSTER_TABLES) {
            assert!(
                tables.iter().any(|name| name == table),
                "{table} is listed but is not a table"
            );
        }
    }

    fn names(rows: &[crate::storage::Row]) -> Vec<String> {
        rows.iter()
            .map(|row| row.text(0).unwrap().to_owned())
            .collect()
    }

    #[test]
    fn every_sqlite_table_is_swept_or_cascaded() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}", dir.path().join("store.db").display());
        let (storage, _) = Storage::open(&url).unwrap();
        storage
            .read(|tx| {
                let tables = names(&tx.query(
                    "SELECT name FROM sqlite_master \
                     WHERE type = 'table' AND name LIKE 'config\\_%' ESCAPE '\\' ORDER BY name",
                    &[],
                )?);
                let mut cascading = BTreeSet::new();
                for table in &tables {
                    let keys = tx.query(
                        &format!(
                            "SELECT on_delete FROM pragma_foreign_key_list('{table}') \
                             WHERE on_delete = 'CASCADE'"
                        ),
                        &[],
                    )?;
                    if !keys.is_empty() {
                        cascading.insert(table.clone());
                    }
                }
                assert_every_table_is_accounted_for(&tables, &cascading);
                Ok(())
            })
            .unwrap();
    }

    /// On Postgres when `PLOYZ_STORE_TEST_POSTGRES` names a server.
    #[test]
    fn every_postgres_table_is_swept_or_cascaded() {
        let Ok(server) = std::env::var("PLOYZ_STORE_TEST_POSTGRES") else {
            return;
        };
        let database = format!("tables_{}", uuid::Uuid::new_v4().simple());
        postgres::Client::connect(&server, postgres::NoTls)
            .unwrap()
            .batch_execute(&format!("CREATE DATABASE {database}"))
            .unwrap();
        let (storage, _) = Storage::open(&format!("{server}/{database}")).unwrap();
        storage
            .read(|tx| {
                let tables = names(&tx.query(
                    "SELECT table_name::text FROM information_schema.tables \
                     WHERE table_schema = current_schema() AND table_name LIKE 'config\\_%' \
                     ORDER BY table_name",
                    &[],
                )?);
                let cascading = names(&tx.query(
                    "SELECT DISTINCT constraints.table_name::text \
                     FROM information_schema.referential_constraints keys \
                     JOIN information_schema.table_constraints constraints \
                       ON constraints.constraint_name = keys.constraint_name \
                      AND constraints.constraint_schema = keys.constraint_schema \
                     WHERE keys.delete_rule = 'CASCADE' \
                       AND constraints.table_schema = current_schema()",
                    &[],
                )?)
                .into_iter()
                .collect();
                assert_every_table_is_accounted_for(&tables, &cascading);
                Ok(())
            })
            .unwrap();
    }
}
