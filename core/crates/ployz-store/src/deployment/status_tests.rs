use super::*;
use crate::storage::Storage;

/// A match with no wildcard, so a new status stops this compiling until
/// [`DeploymentStatus::ALL`] lists it.
fn listed(status: DeploymentStatus) -> bool {
    match status {
        DeploymentStatus::WaitingForCi
        | DeploymentStatus::Queued
        | DeploymentStatus::Superseded
        | DeploymentStatus::Running
        | DeploymentStatus::Applied
        | DeploymentStatus::Failed
        | DeploymentStatus::Unknown
        | DeploymentStatus::Cancelling
        | DeploymentStatus::Cancelled => DeploymentStatus::ALL.contains(&status),
    }
}

#[test]
fn every_status_is_listed_and_spelled_as_it_is_stored() {
    for status in DeploymentStatus::ALL {
        assert!(listed(status));
        assert_eq!(status.as_str(), name_of(status));
    }
}

fn named(want: impl Fn(DeploymentStatus) -> bool) -> Vec<&'static str> {
    DeploymentStatus::ALL
        .into_iter()
        .filter(|status| want(*status))
        .map(DeploymentStatus::as_str)
        .collect()
}

#[test]
fn the_statuses_that_may_still_run_are_claimed_or_ended_keep_their_names() {
    assert_eq!(
        named(DeploymentStatus::in_flight),
        ["queued", "running", "cancelling"]
    );
    assert_eq!(named(DeploymentStatus::claimed), ["running", "cancelling"]);
    assert_eq!(
        named(DeploymentStatus::ended),
        ["applied", "failed", "unknown", "cancelled"]
    );
}

#[test]
fn each_fragment_reads_as_the_literal_it_replaced() {
    assert_eq!(
        IN_FLIGHT.as_str(),
        "status IN ('queued', 'running', 'cancelling')"
    );
    assert_eq!(CLAIMED.as_str(), "status IN ('running', 'cancelling')");
    assert_eq!(
        ENDED.as_str(),
        "status IN ('applied', 'failed', 'unknown', 'cancelled')"
    );
    assert_eq!(NOT_SUPERSEDED.as_str(), "status NOT IN ('superseded')");
    assert_eq!(
        LISTED.as_str(),
        "(number IS NOT NULL OR status <> 'superseded')"
    );
    assert_eq!(NUMBERED, "number IS NOT NULL");
}

/// The SQL with the clock it read cut out: the part after `lease`, up to its
/// closing parentheses, is the time of the call.
fn without_clock(sql: &str, closing: &str) -> (String, i64) {
    let (head, time) = sql.rsplit_once(' ').unwrap();
    let time = time.strip_suffix(closing).unwrap().parse().unwrap();
    (head.to_owned(), time)
}

#[test]
fn the_lease_predicates_read_as_they_did() {
    let (in_flight, at) = without_clock(&in_flight_sql(), "))");
    assert_eq!(
        in_flight,
        "(status IN ('queued', 'running', 'cancelling') AND (status = 'queued' OR lease >"
    );
    assert!((at - now()).abs() <= 1);
    let (lapsed, at) = without_clock(&lapsed_sql(), ")");
    assert_eq!(lapsed, "(status IN ('running', 'cancelling') AND lease <=");
    assert!((at - now()).abs() <= 1);
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
/// the way the index does, and a status added to the set must be added to the index.
#[test]
fn the_in_flight_set_is_the_predicate_the_index_keeps() {
    assert_eq!(index_predicate(), IN_FLIGHT.as_str());
    assert!(in_flight_sql().starts_with(&format!("({} AND ", index_predicate())));
}
