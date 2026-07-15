use super::{
    NamespaceRemoveOperationSubmission, OperationRepository, select_all_statuses_newest_first,
    select_status, upsert_status,
};
use crate::control::store::CoreStore;
use ployz_core::operation::{
    EventSequence, NamespaceRemoveRunningStage, NamespaceRemoveTransition,
    OperationEventReplayRequest, OperationStatus,
};
use ployz_test_support::ids::{event_replay_limit, event_sequence, namespace_id, operation_id};
use rusqlite::Connection;

#[tokio::test]
async fn replay_preserves_recorded_timestamps_for_ordered_events() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let store = CoreStore::open_in_memory()
        .await
        .expect("open operation store");
    let repository = OperationRepository::open(store.clone(), nats.controller);
    let operation_id = operation_id("op_recorded_timestamps");

    repository
        .submit_namespace_remove(NamespaceRemoveOperationSubmission {
            operation_id: operation_id.clone(),
            namespace_id: namespace_id("team-a"),
        })
        .await
        .expect("submit operation");
    repository
        .record_namespace_remove_transition(
            &operation_id,
            NamespaceRemoveTransition::Running {
                stage: NamespaceRemoveRunningStage::RemovingRouteBindings,
            },
        )
        .await
        .expect("append second event");
    let persisted_operation_id = operation_id.clone();
    let persisted = store
        .call(move |conn| {
            let mut statement = conn.prepare(
                "SELECT sequence, recorded_at_unix_ms FROM operation_events
                 WHERE operation_id = ?1 ORDER BY sequence",
            )?;
            let rows = statement.query_map([persisted_operation_id.as_str()], |row| {
                Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .expect("read persisted event timestamps");
    let request = OperationEventReplayRequest {
        operation_id,
        start_sequence: event_sequence(1),
        limit: event_replay_limit(10),
    };

    let first_replay = repository
        .replay_operation_events(request.clone())
        .await
        .expect("replay events");
    let second_replay = repository
        .replay_operation_events(request)
        .await
        .expect("replay events again");

    assert_eq!(
        first_replay
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        [event_sequence(1), event_sequence(2)]
    );
    assert_eq!(
        first_replay
            .events
            .iter()
            .map(|event| (
                event.sequence.get(),
                event.recorded_at_unix_ms.unix_millis()
            ))
            .collect::<Vec<_>>(),
        persisted
    );
    assert_eq!(first_replay, second_replay);
}

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
