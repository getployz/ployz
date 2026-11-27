use std::{io, sync::Arc, time::Duration};

use ployz_core::MachineId;
use tokio::net::UnixListener;

use super::load_down;
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

    let started = tokio::time::Instant::now();
    let error = load_down(&machines, &AdminClient::new(path), &MachineId::random())
        .await
        .unwrap_err();

    assert!(matches!(error, CorrosionError::Io(error) if error.kind() == io::ErrorKind::TimedOut));
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(1),
        "a route decision waits the production bound on a sample, then fails open"
    );
    admin_server.abort();
    std::fs::remove_dir_all(root).unwrap();
}
