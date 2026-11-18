use super::*;

#[test]
fn an_unreadable_record_refuses_rather_than_forgetting() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(FILE_NAME), b"{").unwrap();
    let log = ConsumptionLog::new(dir.path());
    let machine = ployz_core::MachineId::parse("1".repeat(32)).unwrap();
    assert!(matches!(
        log.require_unused_join(machine),
        Err(Error::JoinRecord(_))
    ));
}
