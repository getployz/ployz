use super::{
    NamespaceRemoveOperationSubmission, OperationRepository, select_all_statuses_newest_first,
    select_owned_operation_statuses, select_status, to_durable_operation_json, upsert_status,
};
use crate::control::store::{CoreStore, from_json};
use crate::tasks::TaskRegistry;
use ployz_core::ids::CertId;
use ployz_core::operation::{
    CertInterruptionStage, CertOperationFailure, CertOperationState,
    CertificateInterruptionNextAction, CertificateProvisionFailure, EventSequence,
    NamespaceRemoveOperationState, NamespaceRemoveRunningStage, NamespaceRemoveTransition,
    OperationEventReplayCursor, OperationEventReplayRequest, OperationInterruptionCause,
    OperationOutcome, OperationStatus,
};
use ployz_test_support::ids::{event_replay_limit, event_sequence, namespace_id, operation_id};
use rusqlite::Connection;
use std::time::Duration;

fn assert_durable_interruption_evidence(json: &str) {
    fn visit(value: &serde_json::Value, found: &mut bool) {
        match value {
            serde_json::Value::Object(fields) => {
                if fields.contains_key("cause") && fields.contains_key("last_durable_stage") {
                    *found = true;
                    assert!(!fields.contains_key("kind"));
                    assert!(!fields.contains_key("uncertain_work"));
                    assert!(!fields.contains_key("next_action"));
                }
                for field in fields.values() {
                    visit(field, found);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value, found);
                }
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
        }
    }

    let value: serde_json::Value = serde_json::from_str(json).expect("durable row is JSON");
    let mut found = false;
    visit(&value, &mut found);
    assert!(found, "durable row contains interruption evidence");
}

#[test]
fn durable_interruption_projection_does_not_strip_certificate_failure_evidence() {
    let cert_id = CertId::try_new("cert-a").expect("cert id");
    let status = OperationStatus::Cert {
        id: operation_id("op-cert-interrupted"),
        cert_id: cert_id.clone(),
        state: CertOperationState::Failed {
            failure: CertOperationFailure::try_new(
                cert_id,
                CertificateProvisionFailure::CoreInterrupted {
                    cause: OperationInterruptionCause::PriorCoreProcessLoss,
                    last_durable_stage: CertInterruptionStage::Accepted,
                    next_action: CertificateInterruptionNextAction::RetryFromCurrentIntent,
                },
                None,
            )
            .expect("certificate failure"),
        },
        last_event_sequence: EventSequence::first(),
    };

    let json = to_durable_operation_json(&status).expect("serialize durable certificate status");
    assert!(json.contains("\"next_action\":\"retry_from_current_intent\""));
    assert_eq!(
        from_json::<OperationStatus>(&json).expect("read durable certificate status"),
        status
    );
}

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

#[tokio::test]
async fn caught_up_terminal_replay_carries_the_stored_status_outcome() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let store = CoreStore::open_in_memory()
        .await
        .expect("open operation store");
    let repository = OperationRepository::open(store, nats.controller);
    let operation_id = operation_id("op_terminal_replay_outcome");

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
        .expect("record running transition");
    repository
        .record_namespace_remove_transition(&operation_id, NamespaceRemoveTransition::Completed)
        .await
        .expect("record completed transition");

    let page = repository
        .replay_operation_events(OperationEventReplayRequest {
            operation_id,
            start_sequence: event_sequence(4),
            limit: event_replay_limit(10),
        })
        .await
        .expect("replay events after terminal event");

    assert!(page.events.is_empty());
    assert_eq!(
        page.cursor,
        OperationEventReplayCursor::Terminal {
            outcome: OperationOutcome::Succeeded,
        }
    );
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

fn seed_submission_event(
    conn: &Connection,
    operation_id: &str,
    submission_event: &str,
) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO operation_events
            (operation_id, sequence, recorded_at_unix_ms, event_json)
         VALUES (?1, 1, 1, ?2)",
        rusqlite::params![operation_id, format!(r#"{{"event":"{submission_event}"}}"#)],
    )?;
    Ok(())
}

#[test]
fn owner_status_selection_skips_other_owner_corruption_and_reports_owned_corruption() {
    let connection = Connection::open_in_memory().expect("open operation database");
    connection
        .execute_batch(
            "CREATE TABLE operations (
                operation_id TEXT PRIMARY KEY,
                status_json TEXT NOT NULL
            );
            CREATE TABLE operation_events (
                operation_id TEXT NOT NULL,
                sequence INTEGER NOT NULL,
                recorded_at_unix_ms INTEGER NOT NULL,
                event_json TEXT NOT NULL,
                subject TEXT
            );",
        )
        .expect("create operation tables");
    connection
        .execute(
            "INSERT INTO operations (operation_id, status_json) VALUES (?1, 'not json')",
            ["op_machine_corrupt"],
        )
        .expect("insert corrupt machine-add status");
    seed_submission_event(&connection, "op_machine_corrupt", "machine_add_submitted")
        .expect("insert machine-add discriminator");
    upsert_status(
        &connection,
        &operation_id("op_cert"),
        &namespace_remove_status("op_cert"),
    )
    .expect("insert readable owned status");
    seed_submission_event(&connection, "op_cert", "cert_provision_submitted")
        .expect("insert certificate discriminator");

    assert_eq!(
        select_owned_operation_statuses(&connection, &["cert_provision_submitted"])
            .expect("unrelated corrupt status is skipped")
            .len(),
        1
    );
    assert!(
        select_owned_operation_statuses(&connection, &["managed_dns_reconcile_submitted"])
            .expect("unrelated corrupt status is skipped")
            .is_empty()
    );
    assert!(
        select_owned_operation_statuses(&connection, &["namespace_remove_submitted"])
            .expect("unrelated corrupt status is skipped")
            .is_empty()
    );

    connection
        .execute(
            "UPDATE operations SET status_json = 'not json' WHERE operation_id = ?1",
            ["op_cert"],
        )
        .expect("corrupt owned status");
    assert!(
        select_owned_operation_statuses(&connection, &["cert_provision_submitted"]).is_err(),
        "selected-owner corruption remains visible"
    );

    upsert_status(
        &connection,
        &operation_id("op_cert"),
        &namespace_remove_status("op_cert"),
    )
    .expect("restore readable owned status");
    connection
        .execute(
            "UPDATE operation_events SET event_json = 'not json' WHERE operation_id = ?1",
            ["op_cert"],
        )
        .expect("corrupt owned submission event");
    assert!(
        select_owned_operation_statuses(&connection, &["cert_provision_submitted"]).is_err(),
        "malformed submission evidence remains visible"
    );
}

#[tokio::test]
async fn interruption_recovery_is_uncapped_idempotent_and_excludes_other_owners() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let store = CoreStore::open_in_memory()
        .await
        .expect("open operation store");
    let repository = OperationRepository::open(store.clone(), nats.controller);
    store
        .call(|conn| {
            for index in 0..102 {
                let id = format!("op_interrupted_{index:03}");
                upsert_status(conn, &operation_id(&id), &namespace_remove_status(&id))?;
                seed_submission_event(conn, &id, "namespace_remove_submitted")?;
            }
            let terminal_id = operation_id("op_terminal");
            upsert_status(
                conn,
                &terminal_id,
                &OperationStatus::NamespaceRemove {
                    id: terminal_id.clone(),
                    namespace_id: namespace_id("team-a"),
                    state: NamespaceRemoveOperationState::Completed,
                    last_event_sequence: EventSequence::first(),
                },
            )?;
            seed_submission_event(conn, "op_terminal", "namespace_remove_submitted")?;
            let storage_id = operation_id("op_storage_prepare");
            upsert_status(
                conn,
                &storage_id,
                &OperationStatus::machine_storage_prepare_accepted(
                    storage_id.clone(),
                    ployz_test_support::ids::machine_id("machine-storage"),
                    None,
                    EventSequence::first(),
                ),
            )?;
            seed_submission_event(
                conn,
                "op_storage_prepare",
                "machine_storage_prepare_submitted",
            )?;
            let prune_id = operation_id("op_build_cache_prune");
            upsert_status(
                conn,
                &prune_id,
                &OperationStatus::machine_build_cache_prune_accepted(
                    prune_id.clone(),
                    ployz_test_support::ids::machine_id("machine-cache"),
                    EventSequence::first(),
                ),
            )?;
            seed_submission_event(
                conn,
                "op_build_cache_prune",
                "machine_build_cache_prune_submitted",
            )?;
            let cert_id = operation_id("op_cert_owned_elsewhere");
            upsert_status(
                conn,
                &cert_id,
                &OperationStatus::cert_accepted(
                    cert_id.clone(),
                    ployz_core::ids::CertId::try_new("cert-a").expect("cert id"),
                    EventSequence::first(),
                ),
            )?;
            seed_submission_event(conn, "op_cert_owned_elsewhere", "cert_provision_submitted")?;
            Ok(())
        })
        .await
        .expect("seed statuses");

    let first = repository
        .record_interrupted_operations(OperationInterruptionCause::PriorCoreProcessLoss)
        .await
        .expect("recover interrupted operations");
    assert_eq!(first.recorded, 104);

    let recovered = repository
        .get(&operation_id("op_interrupted_000"))
        .await
        .expect("read recovered status")
        .expect("recovered status exists");
    assert!(matches!(
        recovered,
        OperationStatus::NamespaceRemove {
            state: NamespaceRemoveOperationState::Interrupted { .. },
            ..
        }
    ));
    assert!(
        repository
            .get(&operation_id("op_storage_prepare"))
            .await
            .expect("read recovered storage preparation")
            .expect("recovered storage preparation exists")
            .is_terminal()
    );
    assert!(
        repository
            .get(&operation_id("op_build_cache_prune"))
            .await
            .expect("read recovered cache prune")
            .expect("recovered cache prune exists")
            .is_terminal()
    );
    let (status_json, event_json) = store
        .call(|conn| {
            let status_json = conn.query_row(
                "SELECT status_json FROM operations WHERE operation_id = ?1",
                ["op_interrupted_000"],
                |row| row.get::<_, String>(0),
            )?;
            let event_json = conn.query_row(
                "SELECT event_json FROM operation_events
                 WHERE operation_id = ?1 ORDER BY sequence DESC LIMIT 1",
                ["op_interrupted_000"],
                |row| row.get::<_, String>(0),
            )?;
            Ok((status_json, event_json))
        })
        .await
        .expect("read durable interruption rows");
    for durable_json in [&status_json, &event_json] {
        assert_durable_interruption_evidence(durable_json);
    }
    let public_json = serde_json::to_string(&recovered).expect("serialize public status");
    assert!(public_json.contains("\"kind\":\"namespace_remove\""));
    assert!(public_json.contains("\"uncertain_work\":\"intent_and_runtime\""));
    assert!(public_json.contains("\"next_action\":\"inspect_then_resubmit\""));
    assert!(
        !repository
            .get(&operation_id("op_cert_owned_elsewhere"))
            .await
            .expect("read cert")
            .expect("cert exists")
            .is_terminal()
    );

    let second = repository
        .record_interrupted_operations(OperationInterruptionCause::PriorCoreProcessLoss)
        .await
        .expect("repeat recovery");
    assert_eq!(second.recorded, 0);
}

#[tokio::test]
async fn quiesced_running_worker_cannot_race_interruption_sealing() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let store = CoreStore::open_in_memory()
        .await
        .expect("open operation store");
    let repository = OperationRepository::open(store.clone(), nats.controller);
    let operation_id = operation_id("op_running_worker");
    store
        .call({
            let operation_id = operation_id.clone();
            move |conn| {
                upsert_status(
                    conn,
                    &operation_id,
                    &OperationStatus::NamespaceRemove {
                        id: operation_id.clone(),
                        namespace_id: namespace_id("team-a"),
                        state: NamespaceRemoveOperationState::Running {
                            stage: NamespaceRemoveRunningStage::RemovingContainers,
                        },
                        last_event_sequence: EventSequence::first(),
                    },
                )?;
                seed_submission_event(conn, operation_id.as_str(), "namespace_remove_submitted")
            }
        })
        .await
        .expect("seed running operation");

    let tasks = TaskRegistry::default();
    let worker_repository = repository.clone();
    let worker_operation_id = operation_id.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    tasks
        .spawn(|| async move {
            let _ = started_tx.send(());
            if release_rx.await.is_ok() {
                worker_repository
                    .record_namespace_remove_transition(
                        &worker_operation_id,
                        NamespaceRemoveTransition::Completed,
                    )
                    .await
                    .expect("worker completion records");
            }
        })
        .expect("worker admits");
    started_rx.await.expect("worker starts");

    tasks
        .abort_and_join(Duration::from_secs(1))
        .await
        .expect("worker quiesces");
    assert!(release_tx.send(()).is_err(), "worker receiver must be gone");
    repository
        .record_interrupted_operations(OperationInterruptionCause::CoreShutdown)
        .await
        .expect("interruption seals after quiescence");

    let status = repository
        .get(&operation_id)
        .await
        .expect("status reads")
        .expect("status exists");
    assert!(matches!(
        status,
        OperationStatus::NamespaceRemove {
            state: NamespaceRemoveOperationState::Interrupted { .. },
            ..
        }
    ));
}
