//! Façade tests for one-shot Machine RPC Register over Relay Dial.

use std::time::Duration;

use ployz::sdk;
use ployz_core::{
    AdvertisedEndpoint, MachineId, MachineName, RegisterRequest, RpcError, RpcErrorCode,
    StorageChoice, WireGuardPublicKey,
};
use tokio::time::timeout;

use super::relay::{self, RelaySession};
use super::sdk::advertised_description;
use super::support::DiscoveryService;

#[tokio::test]
async fn list_held_then_register_returns_registered_and_closes_the_dial() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let held = wait_held(&session.url, description.machine_id).await;
    let identity = joiner_identity();

    let registered = timeout(
        Duration::from_secs(5),
        sdk::register(
            &session.url,
            relay::DIAL,
            relay::PAIRING,
            held.as_str(),
            identity.clone(),
        ),
    )
    .await
    .expect("register must not hang")
    .unwrap();

    assert_eq!(registered.assigned_machine.name, identity.name);
    assert_eq!(registered.assigned_machine.public_key, identity.public_key);
    assert_eq!(
        registered.assigned_machine.advertised_endpoints,
        identity.advertised_endpoints
    );

    let again = timeout(
        Duration::from_secs(5),
        sdk::register(
            &session.url,
            relay::DIAL,
            relay::PAIRING,
            held.as_str(),
            identity,
        ),
    )
    .await
    .expect("a second register must not hang")
    .unwrap();
    assert_eq!(again.assigned_machine.name.as_str(), "joiner");
}

#[tokio::test]
async fn register_rejects_bad_dial_pairing_and_unknown_machine_like_connect() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let machine_id = description.machine_id.as_str();
    let identity = joiner_identity();

    let empty = timeout(
        Duration::from_secs(2),
        sdk::register(
            &session.url,
            "",
            relay::PAIRING,
            machine_id,
            identity.clone(),
        ),
    )
    .await
    .expect("empty Dial Credential must not hang");
    assert_eq!(unwrap_rpc(empty).code, RpcErrorCode::Unauthenticated);

    let empty_pairing = timeout(
        Duration::from_secs(2),
        sdk::register(&session.url, relay::DIAL, "", machine_id, identity.clone()),
    )
    .await
    .expect("empty pairing must not hang");
    assert_eq!(
        unwrap_rpc(empty_pairing).code,
        RpcErrorCode::InvalidArgument
    );

    let bad = timeout(
        Duration::from_secs(2),
        sdk::register(
            &session.url,
            "wrong-secret",
            relay::PAIRING,
            machine_id,
            identity.clone(),
        ),
    )
    .await
    .expect("bad Dial Credential must not hang");
    assert_eq!(unwrap_rpc(bad).code, RpcErrorCode::Unauthenticated);

    let unknown = timeout(
        Duration::from_secs(2),
        sdk::register(
            &session.url,
            relay::DIAL,
            relay::PAIRING,
            MachineId::random().as_str(),
            identity.clone(),
        ),
    )
    .await
    .expect("unknown Machine ID must not hang");
    assert_eq!(unwrap_rpc(unknown).code, RpcErrorCode::NotFound);

    let invalid = timeout(
        Duration::from_secs(2),
        sdk::register(
            &session.url,
            relay::DIAL,
            relay::PAIRING,
            "not-a-machine-id",
            identity,
        ),
    )
    .await
    .expect("invalid Machine ID must not hang");
    assert_eq!(unwrap_rpc(invalid).code, RpcErrorCode::InvalidArgument);
}

#[tokio::test]
async fn register_isolation_locked_is_rpc_error() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let service = DiscoveryService::new(description.clone());
    service.set_register_error(RpcError {
        code: RpcErrorCode::Unavailable,
        message: "this Machine is isolation-locked".into(),
        details: serde_json::Value::Null,
    });
    let _machine = session.spawn_machine(description.machine_id, service).await;
    wait_held(&session.url, description.machine_id).await;

    let error = timeout(
        Duration::from_secs(5),
        sdk::register(
            &session.url,
            relay::DIAL,
            relay::PAIRING,
            description.machine_id.as_str(),
            joiner_identity(),
        ),
    )
    .await
    .expect("isolation lock must not hang")
    .expect_err("isolation lock is RpcError");

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "this Machine is isolation-locked");
}

#[tokio::test]
async fn node_smoke_covers_list_held_then_register() {
    let description = advertised_description();
    let isolated_id = MachineId::parse("ffffffffffffffffffffffffffffffff").unwrap();
    let session = RelaySession::start().await;
    let _entry = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let isolated = DiscoveryService::new(description.clone());
    isolated.set_register_error(RpcError {
        code: RpcErrorCode::Unavailable,
        message: "this Machine is isolation-locked".into(),
        details: serde_json::Value::Null,
    });
    let _isolated = session.spawn_machine(isolated_id, isolated).await;
    wait_held(&session.url, description.machine_id).await;
    wait_held(&session.url, isolated_id).await;

    session
        .assert_sdk_script(
            "node_register.js",
            description.machine_id,
            &[
                ("PLOYZ_ISOLATED_MACHINE_ID", isolated_id.as_str()),
                ("PLOYZ_UNKNOWN_MACHINE_ID", MachineId::random().as_str()),
            ],
        )
        .await;
}

async fn wait_held(url: &str, machine_id: MachineId) -> MachineId {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let listed = sdk::list_held(url, relay::DIAL, relay::PAIRING)
            .await
            .unwrap();
        if let Some(row) = listed
            .iter()
            .find(|row| row.machine_id().ok() == Some(machine_id))
        {
            return row.machine_id().unwrap();
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("List did not return the echoed Machine");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn joiner_identity() -> RegisterRequest {
    RegisterRequest {
        machine_id: MachineId::random(),
        assigned_subnet: Some("10.210.1.0/24".parse().unwrap()),
        initial_policy: Default::default(),
        name: MachineName::parse("joiner").unwrap(),
        storage: StorageChoice::Zfs,
        public_key: WireGuardPublicKey([1; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.9:51820".parse().unwrap())],
        runtime: Default::default(),
    }
}

fn unwrap_rpc<T>(result: Result<T, RpcError>) -> RpcError {
    match result {
        Ok(_) => panic!("expected RpcError"),
        Err(error) => error,
    }
}
