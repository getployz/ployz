use std::time::Duration;

use ployz_core::corrosion::{
    ControllerDocument, CorrosionDocument, CorrosionDocumentVersion, CorrosionTable,
    CorrosionTimestamp, RowSkipReason, StoredRow, controller_heartbeat_is_stale,
    is_preferred_controller, read_rows,
};
use ployz_core::ids::{ClusterName, MachineName};
use serde_json::json;

const CLUSTER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const MACHINE_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const OTHER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
const HEARTBEAT_AT: &str = "2026-08-09T12:00:00Z";

fn controller_document() -> ControllerDocument {
    ControllerDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: ClusterName::try_new(CLUSTER_ID).expect("cluster id"),
        preferred_machine_name: MachineName::try_new(MACHINE_ID).expect("machine name"),
        heartbeat_at: CorrosionTimestamp::try_new(HEARTBEAT_AT).expect("heartbeat timestamp"),
    }
}

#[test]
fn controller_document_is_the_single_cluster_keyed_controller_row() {
    let document = controller_document();
    let encoded = serde_json::to_value(&document).expect("controller document JSON");

    assert_eq!(ControllerDocument::TABLE, CorrosionTable::Controller);
    assert_eq!(CorrosionTable::Controller.as_str(), "controller");
    assert!(CorrosionTable::ALL.contains(&CorrosionTable::Controller));
    assert_eq!(
        encoded,
        json!({
            "v": 1,
            "cluster_id": CLUSTER_ID,
            "preferred_machine_name": MACHINE_ID,
            "heartbeat_at": "2026-08-09T12:00:00.000000000Z"
        })
    );
    assert_eq!(
        serde_json::from_value::<ControllerDocument>(encoded.clone())
            .expect("controller document round-trip"),
        document
    );

    let report = read_rows::<ControllerDocument>(
        &ClusterName::try_new(CLUSTER_ID).expect("cluster id"),
        [
            StoredRow::new(CLUSTER_ID, encoded.to_string()),
            StoredRow::new(MACHINE_ID, encoded.to_string()),
        ],
    );
    assert_eq!(report.accepted.len(), 1);
    assert!(matches!(
        report.skipped.as_slice(),
        [skipped]
            if matches!(
                &skipped.reason,
                RowSkipReason::InvalidRowKey { expected } if expected == CLUSTER_ID
            )
    ));
}

#[test]
fn controller_admission_uses_the_preferred_machine_name() {
    let document = controller_document();
    let preferred = MachineName::try_new(MACHINE_ID).expect("preferred machine id");
    let other = MachineName::try_new(OTHER_ID).expect("other machine id");

    assert!(is_preferred_controller(&document, &preferred));
    assert!(!is_preferred_controller(&document, &other));
}

#[test]
fn heartbeat_expires_only_after_the_timeout() {
    let heartbeat =
        CorrosionTimestamp::try_new("2026-08-09T12:00:00Z").expect("heartbeat timestamp");
    let timeout = Duration::from_secs(30);

    for fresh_now in [
        "2026-08-09T11:59:59Z",
        "2026-08-09T12:00:00Z",
        "2026-08-09T12:00:30Z",
    ] {
        assert!(!controller_heartbeat_is_stale(
            CorrosionTimestamp::try_new(fresh_now).expect("current timestamp"),
            heartbeat,
            timeout,
        ));
    }
    assert!(controller_heartbeat_is_stale(
        CorrosionTimestamp::try_new("2026-08-09T12:00:31Z").expect("current timestamp"),
        heartbeat,
        timeout,
    ));
}
