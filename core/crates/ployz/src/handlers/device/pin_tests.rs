use std::sync::Arc;

use ployz_core::{MachineName, SealingPublicKey};
use ployz_testkit::fake_fleet::{FakeFleet, FleetFile, Scenario};
use serde_json::json;

use super::*;
use crate::cloud_login::Secret;
use crate::context::{Connection, ConnectionSource, SelectedConnections};

const ORGANIZATION: &str = "org-1";

fn key(byte: u8) -> SealingPublicKey {
    SealingPublicKey::from_bytes([byte; 32])
}

fn token(cloud: String) -> Credential {
    Credential::Token {
        cloud,
        token: Secret::new("ployz_secret".to_owned()),
        from: crate::cloud_account::TokenFrom::Env,
    }
}

/// A Cloud whose Organization backs up `backed_up` as its Cluster's key.
fn cloud(backed_up: SealingPublicKey) -> Credential {
    token(crate::cloud_login::tests::fake_cloud(
        move |route, _| match route {
            "GET /api/cli/organizations" => (
                200,
                json!({"organizations": [
                    {"id": ORGANIZATION, "slug": "acme", "name": "Acme", "current": true}
                ]}),
            ),
            "POST /api/cli/sealing/recovery" => (
                200,
                json!({"publicKey": backed_up, "backup": null, "digest": null}),
            ),
            other => panic!("unexpected {other}"),
        },
    ))
}

/// One Server, edge, holding key 1.
fn fleet(sockets: &Path) -> FakeFleet {
    let file: FleetFile = serde_json::from_value(json!({"machines": [
        {"name": "edge", "id": "1".repeat(32), "version": "0.4.0", "online": true,
         "sealing": key(1).to_string()},
    ]}))
    .unwrap();
    FakeFleet::start(file, Scenario::default(), sockets, None).unwrap()
}

async fn client(fleet: &FakeFleet, source: ConnectionSource) -> Client {
    let socket = fleet.socket(&MachineName::parse("edge").unwrap()).unwrap();
    crate::connect::connect_selected_with(
        SelectedConnections {
            source,
            connections: vec![Connection::unix(socket).unwrap()],
        },
        Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_pin_keeps_the_key_the_server_reports_over_its_own_socket() {
    let sockets = tempfile::tempdir().unwrap();
    let fleet = fleet(sockets.path());
    let home = tempfile::tempdir().unwrap();
    let pins = Pins::beside(&home.path().join("config.toml"));
    let mut chosen = client(&fleet, ConnectionSource::LocalSocket).await;

    let (organization, pin, replaced) = pin_through(&mut chosen, &cloud(key(1)), &pins)
        .await
        .unwrap();

    assert_eq!(organization.id, ORGANIZATION);
    assert_eq!(pin.public_key, key(1));
    assert_eq!(pin.server.to_string(), "edge");
    assert_eq!(replaced, None);
    assert_eq!(pins.get(ORGANIZATION).unwrap(), Some(pin));
}

#[tokio::test]
async fn a_pin_refuses_a_route_cloud_chose_or_a_key_cloud_doesnt_back_up() {
    let sockets = tempfile::tempdir().unwrap();
    let fleet = fleet(sockets.path());
    let home = tempfile::tempdir().unwrap();
    let pins = Pins::beside(&home.path().join("config.toml"));

    let mut from_cloud = client(&fleet, ConnectionSource::Cloud).await;
    let refusal = pin_through(&mut from_cloud, &cloud(key(1)), &pins)
        .await
        .unwrap_err();
    assert!(
        refusal
            .report()
            .message
            .contains("SSH or the local socket only"),
        "{}",
        refusal.report().message
    );

    let mut chosen = client(&fleet, ConnectionSource::LocalSocket).await;
    let refusal = pin_through(&mut chosen, &cloud(key(9)), &pins)
        .await
        .unwrap_err();
    assert_eq!(refusal.report().code, ployz_core::RpcErrorCode::Conflict);
    assert!(refusal.report().message.contains("nothing was pinned"));

    assert_eq!(pins.get(ORGANIZATION).unwrap(), None, "a refusal pinned");
}
