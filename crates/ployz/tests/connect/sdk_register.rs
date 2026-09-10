//! Enrollment observation and publication use the same confirmed session.

use std::time::Duration;

use ployz::sdk;
use ployz_core::{
    AdvertisedEndpoint, MachineId, MachineName, RegisterRequest, RpcError, RpcErrorCode,
    StorageChoice, WireGuardPublicKey,
};
use tokio::time::timeout;

use super::sdk::advertised_description;
use super::support::DiscoveryService;
use super::unix_session::{self, UnixSession};

fn enrollment_service(description: ployz_core::ContractDescription) -> DiscoveryService {
    let mut service = DiscoveryService::new(description);
    service.enrollment = Some(Default::default());
    service
}

#[tokio::test]
async fn session_observes_and_registers_saved_assignment_until_closed() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            enrollment_service(description.clone()),
        )
        .await;
    let identity = joiner_identity();

    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();
    let snapshot = client.observe_enrollment().await.unwrap();
    let assignment = ployz_core::allocate_enrollment(&identity, &snapshot, &[]).unwrap();
    let registered = client.register(&assignment).await.unwrap();

    assert_eq!(registered.assigned_machine.name, identity.name);
    assert_eq!(registered.assigned_machine.public_key, identity.public_key);
    assert_eq!(
        registered.assigned_machine.advertised_endpoints,
        identity.advertised_endpoints
    );

    let again = client.register(&assignment).await.unwrap();
    assert_eq!(again.assigned_machine, registered.assigned_machine);
    client.close().await;
    assert!(client.observe_enrollment().await.is_err());
    assert!(client.register(&assignment).await.is_err());
}

#[tokio::test]
async fn register_isolation_locked_is_rpc_error() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let service = enrollment_service(description.clone());
    service.set_register_error(RpcError {
        code: RpcErrorCode::Unavailable,
        message: "this Machine is isolation-locked".into(),
        details: serde_json::Value::Null,
    });
    let _machine = session.spawn_machine(description.machine_id, service).await;

    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();
    let snapshot = client.observe_enrollment().await.unwrap();
    let assignment = ployz_core::allocate_enrollment(&joiner_identity(), &snapshot, &[]).unwrap();
    let error = client
        .register(&assignment)
        .await
        .expect_err("isolation lock is RpcError");
    client.close().await;

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "this Machine is isolation-locked");
}

#[tokio::test]
async fn node_smoke_covers_session_register() {
    let description = advertised_description();
    let isolated_id = MachineId::parse("ffffffffffffffffffffffffffffffff").unwrap();
    let session = UnixSession::start().await;
    let _entry = session
        .spawn_machine(
            description.machine_id,
            enrollment_service(description.clone()),
        )
        .await;
    let isolated = enrollment_service(ployz_core::ContractDescription {
        machine_id: isolated_id,
        ..description.clone()
    });
    isolated.set_register_error(RpcError {
        code: RpcErrorCode::Unavailable,
        message: "this Machine is isolation-locked".into(),
        details: serde_json::Value::Null,
    });
    let _isolated = session.spawn_machine(isolated_id, isolated).await;

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

// Rung 2: selected sessions cannot walk to a second Entry or replay a lost mutation reply.
#[tokio::test]
async fn ordered_connections_confirm_before_register_and_never_replay() {
    use super::support::serve_discovery;
    use ployz::{connect::SystemConnector, context::Connection};
    use std::sync::{Arc, atomic::Ordering};
    let description = advertised_description();
    let mut first = enrollment_service(description.clone());
    first.lose_register_reply = true;
    let first_calls = first.register_calls.clone();
    let second = enrollment_service(description.clone());
    let second_calls = second.register_calls.clone();
    let (first_addr, first_server) = serve_discovery(first).await;
    let (second_addr, second_server) = serve_discovery(second).await;
    let client = sdk::connect_connections(
        vec![
            Connection::tcp("127.0.0.1:0".parse().unwrap()),
            Connection::tcp(first_addr),
            Connection::tcp(second_addr),
        ],
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();
    assert_eq!(
        client.about().await.unwrap().machine_id,
        description.machine_id
    );
    let snapshot = client.observe_enrollment().await.unwrap();
    let assignment = ployz_core::allocate_enrollment(&joiner_identity(), &snapshot, &[]).unwrap();
    let error = client.register(&assignment).await.unwrap_err();
    assert!(error.message.contains("uncertain"), "{error}");
    assert_eq!(first_calls.load(Ordering::SeqCst), 1);
    assert_eq!(second_calls.load(Ordering::SeqCst), 0);
    client.close().await;
    assert!(client.about().await.is_err());
    first_server.abort();
    second_server.abort();
}

// Rung 2: closing a session releases a blocked mutation with an uncertain outcome.
#[tokio::test]
async fn close_interrupts_register_with_uncertain_outcome() {
    let description = advertised_description();
    let session = UnixSession::start().await;
    let received = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut service = enrollment_service(description.clone());
    service.register_blocked = Some(received.clone());
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();
    let snapshot = client.observe_enrollment().await.unwrap();
    let assignment = ployz_core::allocate_enrollment(&joiner_identity(), &snapshot, &[]).unwrap();
    let pending = {
        let client = client.clone();
        tokio::spawn(async move { client.register(&assignment).await })
    };
    timeout(Duration::from_secs(2), received.notified())
        .await
        .unwrap();
    client.close().await;
    let error = timeout(Duration::from_millis(500), pending)
        .await
        .expect("close must release blocked Register")
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert!(error.message.contains("uncertain"), "{error}");
}
