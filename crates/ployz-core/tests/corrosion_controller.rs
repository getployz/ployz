use ployz_core::corrosion::{
    ControllerAppointmentId, ControllerDocument, CorrosionDocument, CorrosionDocumentVersion,
    CorrosionTable, RowSkipReason, StoredRow, controller_visibility_allows_work,
    owns_current_controller_appointment, read_rows,
};
use ployz_core::ids::{ClusterId, MachineRowId};
use serde_json::json;

const CLUSTER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const MACHINE_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const APPOINTMENT_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const OTHER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";

fn controller_document() -> ControllerDocument {
    ControllerDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: ClusterId::try_new(CLUSTER_ID).expect("cluster id"),
        preferred_machine_id: MachineRowId::try_new(MACHINE_ID).expect("machine id"),
        appointment_id: ControllerAppointmentId::try_new(APPOINTMENT_ID).expect("appointment id"),
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
            "preferred_machine_id": MACHINE_ID,
            "appointment_id": APPOINTMENT_ID
        })
    );
    assert_eq!(
        serde_json::from_value::<ControllerDocument>(encoded.clone())
            .expect("controller document round-trip"),
        document
    );

    let report = read_rows::<ControllerDocument>(
        &ClusterId::try_new(CLUSTER_ID).expect("cluster id"),
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
fn controller_appointment_ids_are_opaque_canonical_ulids() {
    let appointment =
        ControllerAppointmentId::try_new(APPOINTMENT_ID).expect("canonical appointment id");

    assert_eq!(appointment.as_str(), APPOINTMENT_ID);
    assert!(ControllerAppointmentId::try_new("first-appointment").is_err());
    assert!(ControllerAppointmentId::try_new(APPOINTMENT_ID.to_ascii_lowercase()).is_err());
}

#[test]
fn ownership_requires_both_the_preferred_machine_and_exact_appointment() {
    let document = controller_document();
    let preferred = MachineRowId::try_new(MACHINE_ID).expect("preferred machine id");
    let appointment = ControllerAppointmentId::try_new(APPOINTMENT_ID).expect("appointment id");
    let other = MachineRowId::try_new(OTHER_ID).expect("other machine id");
    let stale = ControllerAppointmentId::try_new(OTHER_ID).expect("stale appointment id");

    assert!(owns_current_controller_appointment(
        &document,
        &preferred,
        &appointment
    ));
    assert!(!owns_current_controller_appointment(
        &document,
        &other,
        &appointment
    ));
    assert!(!owns_current_controller_appointment(
        &document, &preferred, &stale
    ));
}

#[test]
fn visibility_brake_applies_only_to_isolated_members_of_three_plus_rosters() {
    assert!(!controller_visibility_allows_work(0, 0));
    assert!(controller_visibility_allows_work(1, 0));
    assert!(controller_visibility_allows_work(2, 0));
    assert!(!controller_visibility_allows_work(3, 1));
    assert!(controller_visibility_allows_work(3, 2));
    assert!(controller_visibility_allows_work(200, 2));
}
