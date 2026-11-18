use ployz_core::MachineName;
use ployz_testkit::fake_fleet::{FakeFleet, Scenario};

use super::*;

fn key(byte: u8) -> SealingPublicKey {
    SealingPublicKey::from_bytes([byte; 32])
}

fn pinned(byte: u8) -> Pin {
    Pin {
        public_key: key(byte),
        server: MachineName::parse("web-1").unwrap(),
    }
}

#[test]
fn a_relayed_key_is_sealed_to_only_when_it_is_the_pinned_one() {
    assert_eq!(check(Some(&pinned(1)), key(1)).unwrap(), key(1));

    for (pin, why) in [
        (Some(pinned(1)), "a key Cloud relays in place of the pin"),
        (None, "a key with nothing pinned to check it"),
    ] {
        let refusal = check(pin.as_ref(), key(2)).unwrap_err();
        let report = refusal.report();
        assert_eq!(report.code, RpcErrorCode::Conflict, "{why}");
        assert!(report.message.contains("nothing was sealed"), "{why}");
        assert_eq!(
            refusal.hints(),
            vec![Hint::Next(PIN_COMMAND.to_owned())],
            "{why}"
        );
    }
}

#[test]
fn a_pin_is_kept_per_organization_and_a_new_one_replaces_the_old() {
    let dir = tempfile::tempdir().unwrap();
    let pins = Pins::beside(&dir.path().join("config.toml"));
    assert_eq!(pins.get("org-a").unwrap(), None);

    assert_eq!(pins.set("org-a", pinned(1)).unwrap(), None);
    assert_eq!(pins.set("org-b", pinned(2)).unwrap(), None);
    assert_eq!(pins.set("org-a", pinned(3)).unwrap(), Some(pinned(1)));

    let reopened = Pins::beside(&dir.path().join("config.toml"));
    assert_eq!(reopened.get("org-a").unwrap(), Some(pinned(3)));
    assert_eq!(reopened.get("org-b").unwrap(), Some(pinned(2)));
}

#[test]
fn an_unreadable_pin_file_refuses_rather_than_reading_as_no_pin() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        crate::config_dir::ConfigDir::new(&config).sealing_keys(),
        "{",
    )
    .unwrap();
    let pins = Pins::beside(&config);

    assert!(matches!(pins.get("org-a"), Err(PinError::Malformed { .. })));
    assert!(matches!(
        pins.set("org-a", pinned(1)),
        Err(PinError::Malformed { .. })
    ));
}

/// web lists first and its row claims another key, as a Machine Cloud planted could.
fn planted_row() -> ployz_testkit::fake_fleet::FleetFile {
    serde_json::from_value(serde_json::json!({"machines": [
        {"name": "web", "id": "2".repeat(32), "version": "0.4.0", "online": true,
         "sealing": key(9).to_string()},
        {"name": "edge", "id": "1".repeat(32), "version": "0.4.0", "online": true,
         "sealing": key(1).to_string()},
    ]}))
    .unwrap()
}

async fn direct_client(fleet: &FakeFleet) -> Client {
    client_from(fleet, ConnectionSource::Direct).await
}

/// A client to edge's socket, as if `source` had chosen it.
async fn client_from(fleet: &FakeFleet, source: ConnectionSource) -> Client {
    let socket = fleet.socket(&MachineName::parse("edge").unwrap()).unwrap();
    crate::connect::connect_selected_with(
        crate::context::SelectedConnections {
            source,
            connections: vec![crate::context::Connection::unix(socket).unwrap()],
        },
        std::sync::Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_direct_entry_seals_to_its_own_key_not_a_row_another_machine_wrote() {
    let sockets = tempfile::tempdir().unwrap();
    let fleet = FakeFleet::start(planted_row(), Scenario::default(), sockets.path(), None).unwrap();
    let mut client = direct_client(&fleet).await;
    let relayed = crate::private_access::observe_access(&mut client)
        .await
        .unwrap()
        .sealing;
    assert_eq!(relayed, Some(key(9)), "the rows claim web's key first");

    assert!(SealTo::direct(&client));
    assert_eq!(
        SealTo::Entry.key(&mut client, relayed).await.unwrap(),
        key(1)
    );
    assert_eq!(
        entry_pin(&mut client).await.unwrap(),
        Pin {
            public_key: key(1),
            server: MachineName::parse("edge").unwrap(),
        }
    );
}

#[tokio::test]
async fn a_relayed_key_other_than_the_pin_is_refused_before_anything_is_sealed() {
    let sockets = tempfile::tempdir().unwrap();
    let fleet = FakeFleet::start(planted_row(), Scenario::default(), sockets.path(), None).unwrap();
    let mut client = direct_client(&fleet).await;

    let refusal = SealTo::Pinned(Some(pinned(1)))
        .key(&mut client, Some(key(9)))
        .await
        .unwrap_err();
    assert_eq!(refusal.report().code, RpcErrorCode::Conflict);
    assert_eq!(
        SealTo::Pinned(Some(pinned(9)))
            .key(&mut client, Some(key(9)))
            .await
            .unwrap(),
        key(9)
    );
    assert_eq!(
        SealTo::Pinned(Some(pinned(1)))
            .key(&mut client, None)
            .await
            .unwrap_err()
            .report()
            .code,
        RpcErrorCode::Unavailable
    );
}

#[test]
fn only_ssh_or_the_local_socket_the_person_chose_reports_its_own_key() {
    use crate::context::{Connection, SshDestination};

    let ssh = Connection::ssh(SshDestination::parse("root@host").unwrap());
    let unix = Connection::unix("/run/ployz/ployzd.sock").unwrap();
    let tcp = Connection::tcp("127.0.0.1:9000".parse().unwrap());
    let management = Connection::management(
        ployz_core::ManagementCapability::new(
            ployz_core::ManagementIdentity::from_bytes([1; 32]),
            [2; 32],
        )
        .to_secret_string(),
    )
    .unwrap();
    let context = || ConnectionSource::Context("prod".to_owned());

    for (source, connection, trusted, why) in [
        (ConnectionSource::Direct, &ssh, true, "--connect ssh"),
        (
            ConnectionSource::LocalSocket,
            &unix,
            true,
            "the local socket",
        ),
        (context(), &ssh, true, "a context's SSH"),
        (
            ConnectionSource::Direct,
            &tcp,
            false,
            "plain TCP authenticates no one",
        ),
        (
            context(),
            &management,
            false,
            "Cloud carries a Management connection",
        ),
        (
            ConnectionSource::Cloud,
            &ssh,
            false,
            "Server Access is Cloud's choice",
        ),
        (
            ConnectionSource::Cloud,
            &unix,
            false,
            "Server Access is Cloud's choice",
        ),
    ] {
        assert_eq!(trusts(&source, connection.transport()), trusted, "{why}");
    }
}

#[tokio::test]
async fn a_connection_from_cloud_checks_the_pin_however_it_reaches_the_server() {
    let sockets = tempfile::tempdir().unwrap();
    let fleet = FakeFleet::start(planted_row(), Scenario::default(), sockets.path(), None).unwrap();

    let from_cloud = client_from(&fleet, ConnectionSource::Cloud).await;
    let seal_to = SealTo::of(&from_cloud, async || Ok(Some(pinned(1))))
        .await
        .unwrap();
    assert_eq!(seal_to, SealTo::Pinned(Some(pinned(1))));

    let chosen = direct_client(&fleet).await;
    let seal_to = SealTo::of(&chosen, async || panic!("a chosen socket reads no pin"))
        .await
        .unwrap();
    assert_eq!(seal_to, SealTo::Entry);
}
