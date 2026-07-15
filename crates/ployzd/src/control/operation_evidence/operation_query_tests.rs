use super::{select_all_statuses_newest_first, select_status, upsert_status};
use ployz_core::operation::{EventSequence, OperationStatus};
use ployz_test_support::ids::{namespace_id, operation_id};
use rusqlite::Connection;

#[test]
fn newest_status_order_is_durable_and_old_status_remains_addressable() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("operation-order.db");
    let connection = Connection::open(&path).expect("open operation database");
    connection
        .execute_batch(
            "CREATE TABLE operations (
                created_order INTEGER PRIMARY KEY AUTOINCREMENT,
                operation_id  TEXT NOT NULL UNIQUE,
                status_json   TEXT NOT NULL
            );",
        )
        .expect("create operations table");

    let oldest_id = operation_id("op_z_oldest");
    upsert_status(
        &connection,
        &oldest_id,
        &namespace_remove_status("op_z_oldest"),
    )
    .expect("insert oldest operation");
    for index in 0..100 {
        let id = format!("op_{index:03}");
        upsert_status(
            &connection,
            &operation_id(&id),
            &namespace_remove_status(&id),
        )
        .expect("insert newer operation");
    }
    drop(connection);

    let mut reopened = Connection::open(&path).expect("reopen operation database");
    let newest = select_all_statuses_newest_first(&mut reopened, false)
        .expect("read operations newest first");
    let [newest_status, middle @ .., oldest_status] = newest.as_slice() else {
        panic!("expected 101 operations");
    };
    let Some(second_oldest_status) = middle.last() else {
        panic!("expected operations between newest and oldest");
    };

    assert_eq!(newest_status.id().as_str(), "op_099");
    assert_eq!(second_oldest_status.id().as_str(), "op_000");
    assert_eq!(oldest_status.id(), &oldest_id);
    assert_eq!(
        select_status(&reopened, &oldest_id)
            .expect("read old operation status")
            .expect("old operation remains stored")
            .id(),
        &oldest_id
    );
}

fn namespace_remove_status(id: &str) -> OperationStatus {
    OperationStatus::namespace_remove_accepted(
        operation_id(id),
        namespace_id("team-a"),
        EventSequence::first(),
    )
}
