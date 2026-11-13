use super::*;
use crate::storage::Storage;

/// Every status, each matched without a wildcard so a new one stops this compiling.
const ALL: [DeploymentStatus; 9] = [
    DeploymentStatus::WaitingForCi,
    DeploymentStatus::Queued,
    DeploymentStatus::Superseded,
    DeploymentStatus::Running,
    DeploymentStatus::Applied,
    DeploymentStatus::Failed,
    DeploymentStatus::Unknown,
    DeploymentStatus::Cancelling,
    DeploymentStatus::Cancelled,
];

#[test]
fn every_status_is_in_the_list_the_sets_below_are_read_from() {
    for status in ALL {
        match status {
            DeploymentStatus::WaitingForCi
            | DeploymentStatus::Queued
            | DeploymentStatus::Superseded
            | DeploymentStatus::Running
            | DeploymentStatus::Applied
            | DeploymentStatus::Failed
            | DeploymentStatus::Unknown
            | DeploymentStatus::Cancelling
            | DeploymentStatus::Cancelled => assert!(ALL.contains(&status)),
        }
    }
}

fn named(want: impl Fn(DeploymentStatus) -> bool) -> Vec<String> {
    ALL.into_iter()
        .filter(|status| want(*status))
        .map(name_of)
        .collect()
}

#[test]
fn the_statuses_that_may_still_run_and_that_ended_keep_their_names() {
    assert_eq!(
        named(DeploymentStatus::in_flight),
        ["queued", "running", "cancelling"]
    );
    assert_eq!(
        named(DeploymentStatus::ended),
        ["applied", "failed", "unknown", "cancelled"]
    );
}

#[test]
fn the_in_flight_predicate_reads_as_it_did() {
    let sql = in_flight_sql();
    let (set, lease) = sql
        .split_once(" AND (status = 'queued' OR lease > ")
        .expect("the lease test follows the status set");
    assert_eq!(set, "(status IN ('queued', 'running', 'cancelling')");
    let now: i64 = lease.strip_suffix("))").unwrap().parse().unwrap();
    assert!((now - super::now()).abs() <= 1);
}

/// The status set a freshly migrated SQLite Store's `config_deployment_in_flight`
/// index keeps its rows to.
fn index_predicate() -> String {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("store.db").display());
    let (storage, _) = Storage::open(&url).unwrap();
    let rows = storage
        .read(|tx| {
            tx.query(
                "SELECT sql FROM sqlite_master WHERE name = 'config_deployment_in_flight'",
                &[],
            )
        })
        .unwrap();
    let created = rows.first().unwrap().text(0).unwrap().to_owned();
    let (_, predicate) = created.split_once("WHERE").unwrap();
    predicate.trim().to_owned()
}

/// SQLite's planner takes a partial index only when a query spells its predicate
/// the way the index does.
#[test]
fn the_in_flight_predicate_starts_with_the_status_set_the_index_keeps() {
    assert_eq!(
        index_predicate(),
        "status IN ('queued', 'running', 'cancelling')"
    );
    let sql = in_flight_sql();
    assert!(sql.starts_with(&format!("({} AND ", index_predicate())));
}
