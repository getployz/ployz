//! Today's daemon still reads the certificate rows and local Machine record a
//! 0.2.0 daemon wrote. The fixtures live beside ployz-core's and are frozen.

#[path = "../../ployz-core/tests/frozen/mod.rs"]
mod frozen;

use frozen::assert_retains;
use ployz_core::{ClusterRoute, LocalMachinePhase};
use serde_json::Value;

use crate::corrosion::CertificateRow;
use crate::machine::LocalMachineRecord;

const CERTIFICATE_ROW: &str =
    include_str!("../../ployz-core/tests/fixtures/v0.2.0/store/certificates.json");
const LOCAL_MACHINE_RECORD: &str =
    include_str!("../../ployz-core/tests/fixtures/v0.2.0/local/machine.json");

#[test]
fn certificate_row_from_0_2_0_still_reads() {
    let row = CertificateRow::decode(CERTIFICATE_ROW).unwrap();
    assert!(row.material().is_some(), "stored material no longer admits");
    assert_eq!(row.route(), Some(ClusterRoute::ViaProxy));
    assert!(row.challenge().is_some());
    assert!(row.clock().is_some());
    assert_eq!(row.last_error(), Some("authority refused the order"));
    let frozen: Value = serde_json::from_str(CERTIFICATE_ROW).unwrap();
    let reencoded: Value = serde_json::from_str(&row.encode().unwrap()).unwrap();
    assert_retains(&frozen, &reencoded, "certificates.body");
}

#[test]
fn local_machine_record_from_0_2_0_still_reads() {
    // `LocalMachineStore::open` reads `machine.json` with exactly this call.
    let record: LocalMachineRecord = serde_json::from_str(LOCAL_MACHINE_RECORD).unwrap();
    assert_eq!(record.phase(), LocalMachinePhase::Participating);
    assert_eq!(record.id().as_str(), "fedcba9876543210fedcba9876543210");
    let frozen: Value = serde_json::from_str(LOCAL_MACHINE_RECORD).unwrap();
    assert_retains(
        &frozen,
        &serde_json::to_value(&record).unwrap(),
        "machine.json",
    );
}
