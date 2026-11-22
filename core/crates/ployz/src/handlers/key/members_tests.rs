use ployz_core::{MachineName, SealingPublicKey};
use ployz_testkit::fake_fleet::{FakeFleet, Scenario};

use super::Roster;
use crate::context::ConnectionSource;

fn key(byte: u8) -> SealingPublicKey {
    SealingPublicKey::from_bytes([byte; 32])
}

/// Two Servers that hold the Cluster key, reporting it as a real daemon does.
fn fleet(sockets: &std::path::Path) -> FakeFleet {
    let file = serde_json::from_value(serde_json::json!({"machines": [
        {"name": "edge", "id": "1".repeat(32), "version": "0.4.0", "online": true,
         "sealing": key(1).to_string()},
        {"name": "web", "id": "2".repeat(32), "version": "0.4.0", "online": true,
         "sealing": key(1).to_string()},
    ]}))
    .unwrap();
    FakeFleet::start(file, Scenario::default(), sockets, None).unwrap()
}

#[tokio::test]
async fn every_member_names_the_cluster_key_it_holds() {
    let sockets = tempfile::tempdir().unwrap();
    let fleet = fleet(sockets.path());
    let socket = fleet.socket(&MachineName::parse("edge").unwrap()).unwrap();
    let mut client = crate::connect::connect_selected_with(
        crate::context::SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![crate::context::Connection::unix(socket).unwrap()],
        },
        std::sync::Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap();

    let roster = Roster::read(&mut client).await.unwrap();

    assert_eq!(roster.members.len(), 2);
    assert!(roster.gaps.is_complete());
    assert_eq!(roster.cluster().unwrap(), key(1));
}
