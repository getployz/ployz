//! Migration steps that call into the Store's feature code.

use ployz_core::RpcError;

use crate::branch::{UNKEPT, json_of};
use crate::storage::Tx;

/// Repair, before `0037_arrival_checks` adds its CHECKs, the stored arrivals it can
/// without inventing a value, each statement touching only the rows it changes: a
/// pending row no Sync claims keeps [`UNKEPT`] for a missing `was`; a Use hint or a
/// settled row keeps no `prior` or `was`, which only a pending row is read for; and
/// a Use hint is a Follow's. A row of another `state` keeps what it holds.
pub(crate) fn repair_arrivals(tx: &mut dyn Tx) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_sync_arrival SET was = ?1 WHERE state = 'pending' \
         AND sync_id IS NULL AND proposal_id IS NULL AND was IS NULL",
        &[json_of(&UNKEPT).as_str().into()],
    )?;
    tx.execute(
        "UPDATE config_sync_arrival SET prior = NULL, was = NULL \
         WHERE state IN ('hint', 'settled') AND (prior IS NOT NULL OR was IS NOT NULL)",
        &[],
    )?;
    tx.execute(
        "UPDATE config_sync_arrival SET how = 'follow' WHERE state = 'hint' AND how <> 'follow'",
        &[],
    )?;
    Ok(())
}

/// Migration `0035_build_interrupted`'s statement: each Building build under a
/// Deployment that left flight and shows an end, Failed and marked interrupted.
/// `in_flight_at` is the clock the in-flight test read and `now` the clock the
/// statement's parameters read, two reads that may differ. The in-flight test is the
/// text `deployment::in_flight_sql` gave when the migration shipped, as the migrated
/// index spells it: a later status joins that function and the index, never this
/// statement.
pub(super) fn interrupt_builds(
    tx: &mut dyn Tx,
    in_flight_at: i64,
    now: i64,
) -> Result<usize, RpcError> {
    tx.execute(
        "UPDATE config_build SET status = 'failed', interrupted = 1, message = ?2, \
         finished = COALESCE(finished, (SELECT CASE WHEN d.ended IS NOT NULL \
           THEN d.ended WHEN d.lease < ?1 THEN d.lease ELSE ?1 END \
           FROM config_deployment d WHERE d.id = config_build.deployment_id)) \
         WHERE status = 'building' AND github IS NULL AND EXISTS (\
           SELECT 1 FROM config_deployment \
           WHERE config_deployment.id = config_build.deployment_id \
           AND NOT (status IN ('queued', 'running', 'cancelling') \
             AND (status = 'queued' OR lease > ?3)) \
           AND (ended IS NOT NULL OR (runner IS NOT NULL \
             AND started IS NOT NULL AND (status = 'unknown' \
               OR (status IN ('running', 'cancelling') AND number IS NOT NULL)))))",
        &[
            now.into(),
            crate::build::INTERRUPTED.into(),
            in_flight_at.into(),
        ],
    )
}

#[cfg(test)]
mod interrupt_builds_tests {
    use super::interrupt_builds;
    use crate::storage::Storage;

    const SEEDED: &str = "INSERT INTO config_deployment (id, organization_id, environment_id, \
        number, status, saved_revision, services, nodes, namespace, run, credentials, \
        admitted, runner, lease, started, listed) VALUES ('d', 'o', 'e', 1, 'running', 1, '', '', \
        '', '', '', 1, 'r', 101, 1, 1); \
        INSERT INTO config_build (deployment_id, service, organization_id, status, log) \
        VALUES ('d', 'web', 'o', 'building', '')";

    /// What the build under a numbered Running Deployment holding a lease to 101, with
    /// a runner and a start and no end, reads after the statement ran with these clocks.
    fn read(in_flight_at: i64, now: i64) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}", dir.path().join("store.db").display());
        let storage = Storage::open_through(&url, "0035_build_interrupted").unwrap();
        let Storage::Sqlite(sqlite) = &storage else {
            unreachable!("a sqlite url opens sqlite")
        };
        sqlite.foreign_keys(false).unwrap();
        storage
            .write(|tx| {
                tx.batch(SEEDED)?;
                interrupt_builds(tx, in_flight_at, now)?;
                let rows = tx.query(
                    "SELECT status, CAST(interrupted AS TEXT), CAST(finished AS TEXT) \
                     FROM config_build",
                    &[],
                )?;
                Ok(rows
                    .first()
                    .map(|row| (0..3).map(|at| row.text(at).unwrap_or("NULL").to_owned()))
                    .unwrap()
                    .collect())
            })
            .unwrap()
    }

    #[test]
    fn the_in_flight_test_reads_its_own_clock_not_the_parameters() {
        // The lease holds at the in-flight clock 100: the Deployment is in flight, and
        // its build stays Building, whatever the parameter clock then reads.
        assert_eq!(read(100, 101), ["building", "0", "NULL"]);
        assert_eq!(read(100, 100), ["building", "0", "NULL"]);
        // The lease has lapsed at the in-flight clock: Failed, finished at the lease
        // (`CASE ... WHEN d.lease < ?1`) or at the parameter clock.
        assert_eq!(read(101, 101), ["failed", "1", "101"]);
        assert_eq!(read(101, 102), ["failed", "1", "101"]);
    }
}
