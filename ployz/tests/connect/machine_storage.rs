use std::sync::atomic::Ordering;

use ployz_core::{
    CapabilityName, ContractDescription, MACHINE_STORAGE_OBSERVATION_CAPABILITY, MachineId,
    PROTOCOL_MAJOR,
};
use serde_json::Value;

use super::{
    run_ployz,
    support::{DiscoveryService, connected_client, machine, serve_discovery},
};

fn storage_capable_service() -> DiscoveryService {
    DiscoveryService::new(ContractDescription {
        machine_id: MachineId::random(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "test".into(),
        capabilities: [CapabilityName::parse(MACHINE_STORAGE_OBSERVATION_CAPABILITY).unwrap()]
            .into(),
    })
}

#[tokio::test]
async fn machines_returns_raw_list_machines_observations_without_storage_fanout() {
    let service = storage_capable_service();
    let (mut client, server, _) = connected_client(service.clone()).await;

    let observed = client.machines().await.unwrap();

    assert_eq!(observed, vec![machine('a', "one")]);
    assert_eq!(service.inspect_calls.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn machine_ls_observes_storage_only_when_the_target_advertises_it() {
    let service = storage_capable_service();
    let (address, server) = serve_discovery(service.clone()).await;

    let output = run_ployz(address, &["machine", "ls", "--output", "json"]).await;

    assert!(output.status.success(), "{output:?}");
    let observed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        observed.pointer("/0/storage").and_then(Value::as_str),
        Some("ready")
    );
    assert_eq!(service.inspect_calls.load(Ordering::SeqCst), 1);
    server.abort();
}
