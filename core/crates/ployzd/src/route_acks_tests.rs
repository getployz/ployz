use std::{io, sync::Arc};

use ployz_core::MachineId;
use tokio::net::UnixListener;

use super::{DownMachines, load_down};
use crate::corrosion::{AdminClient, Error as CorrosionError, MachineView, ReplicatedObservations};

#[tokio::test(start_paused = true)]
async fn membership_sample_times_out() {
    let root = std::env::temp_dir().join(format!("ployzd-membership-{}", MachineId::random()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("admin.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let admin_server = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let machines = MachineView::fixed(Some(Arc::new(ReplicatedObservations {
        observations: Vec::new(),
        incomplete_ids: Vec::new(),
    })));

    let error = load_down(&machines, &AdminClient::new(path), &MachineId::random())
        .await
        .unwrap_err();

    assert!(matches!(error, CorrosionError::Io(error) if error.kind() == io::ErrorKind::TimedOut));
    admin_server.abort();
    std::fs::remove_dir_all(root).unwrap();
}

/// A failed sample is unknown evidence, as `ReadRoutes` reads it: a Machine seen Down
/// before must not stay filtered after it recovered.
#[test]
fn a_failed_membership_sample_marks_no_machine_down() {
    let (local, remote) = (MachineId::random(), MachineId::random());
    let mut down = DownMachines::new(local);
    assert!(down.update(Ok([remote].into())));
    assert!(down.contains(&remote));

    let failed = CorrosionError::Io(io::Error::new(io::ErrorKind::TimedOut, "sample"));
    assert!(
        down.update(Err(failed)),
        "routes change when the Down set clears"
    );
    assert!(!down.contains(&remote));
}
