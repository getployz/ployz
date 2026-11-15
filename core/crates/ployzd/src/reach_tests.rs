use std::io;

use ployz_core::MachineId;

use super::Reach;
use crate::corrosion::Error as CorrosionError;

/// A failed sample is unknown evidence, as `ReadRoutes` reads it: a Machine seen Down
/// before must not stay filtered after it recovered.
#[test]
fn a_failed_membership_sample_marks_no_machine_down() {
    let (local, remote) = (MachineId::random(), MachineId::random());
    let mut reach = Reach::new(local);
    assert!(reach.update_down(Ok([remote].into())));
    assert!(reach.is_down(&remote));

    let failed = CorrosionError::Io(io::Error::new(io::ErrorKind::TimedOut, "sample"));
    assert!(
        reach.update_down(Err(failed)),
        "routes change when the Down set clears"
    );
    assert!(!reach.is_down(&remote));
}

/// The local Machine is never Down, whatever the sample reports.
#[test]
fn the_local_machine_is_never_down() {
    let (local, remote) = (MachineId::random(), MachineId::random());
    let mut reach = Reach::new(local);
    assert!(reach.update_down(Ok([local, remote].into())));
    assert!(!reach.is_down(&local));
    assert!(reach.is_down(&remote));
    assert!(!reach.update_down(Ok([local, remote].into())));
    assert_eq!(reach.local_id(), local);
}
