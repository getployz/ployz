use std::{num::NonZeroU64, sync::atomic::Ordering};

use ployz::deploy::{DeployIntent, PlanOptions};
use ployz_core::{
    CapabilityName, ContractDescription, MACHINE_STORAGE_OBSERVATION_CAPABILITY, MachineId,
    PROTOCOL_MAJOR, ProjectName, ProvisionedVolume, ProvisionedVolumeMaximumBytes,
    RequestedServiceSpec, ServiceName, ServiceVolumeReference,
};
use serde_json::Value;

use super::{
    run_ployz,
    support::{DiscoveryService, connected_client, machine, serve_discovery},
};

fn storage_capable_service() -> DiscoveryService {
    let mut service = DiscoveryService::new(ContractDescription {
        machine_id: MachineId::random(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "test".into(),
        capabilities: [CapabilityName::parse(MACHINE_STORAGE_OBSERVATION_CAPABILITY).unwrap()]
            .into(),
    });
    service
        .machines
        .first_mut()
        .unwrap()
        .machine
        .runtime
        .daemon_version = env!("CARGO_PKG_VERSION").into();
    service
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
    let mut service = storage_capable_service();
    service.storage = ployz_core::MachineStorageObservation::Pool {
        size_bytes: std::num::NonZeroU64::new(4_294_967_296).unwrap(),
        used_bytes: 3_865_470_566,
        free_bytes: 429_496_730,
    };
    let (address, server) = serve_discovery(service.clone()).await;

    let output = run_ployz(address, &["machine", "ls", "--output", "json"]).await;

    assert!(output.status.success(), "{output:?}");
    let observed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        observed.pointer("/0/storage"),
        Some(&serde_json::json!({
            "state": "pool",
            "size_bytes": 4_294_967_296_u64,
            "used_bytes": 3_865_470_566_u64,
            "free_bytes": 429_496_730_u64,
        }))
    );
    assert_eq!(service.inspect_calls.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn machine_ls_warns_without_failing_when_one_daemon_version_differs() {
    let mut service = storage_capable_service();
    service
        .machines
        .first_mut()
        .unwrap()
        .machine
        .runtime
        .daemon_version = "0.0.0-old".into();
    let (address, server) = serve_discovery(service).await;

    let output = run_ployz(address, &["machine", "ls"]).await;

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "WARNING: 1 Machine runs a daemon version different from CLI {}.\n",
            env!("CARGO_PKG_VERSION")
        )
    );
    server.abort();
}

#[tokio::test]
async fn machine_ls_does_not_warn_when_every_daemon_matches() {
    let service = storage_capable_service();
    let (address, server) = serve_discovery(service).await;

    let output = run_ployz(address, &["machine", "ls"]).await;

    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stderr, b"");
    server.abort();
}

#[tokio::test]
async fn deploy_preview_observes_storage_before_refusing_a_stateless_explicit_target() {
    let mut service = storage_capable_service();
    service.storage = ployz_core::MachineStorageObservation::Stateless;
    let (mut client, server, _) = connected_client(service.clone()).await;
    let requested: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "busybox", "pull_policy": "always" },
        "placement": { "machines": ["one"] },
        "volumes": [{
            "reference": "data",
            "source": { "kind": "named", "name": "data" }
        }],
        "mounts": [{ "volume": "data", "target": "/data" }]
    }))
    .unwrap();
    let mut intent = DeployIntent::apply_one(
        ProjectName::parse("app").unwrap(),
        requested,
        PlanOptions::default(),
    );
    intent.provisioned_volumes = vec![ProvisionedVolume {
        service: ServiceName::parse("api").unwrap(),
        reference: ServiceVolumeReference::parse("data").unwrap(),
        maximum_bytes: ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(1024).unwrap()),
    }];

    let error = client.preview(intent).await.unwrap_err().to_string();

    assert!(error.contains("storage preparation"), "{error}");
    assert!(error.contains("--storage zfs"), "{error}");
    assert_eq!(service.inspect_calls.load(Ordering::SeqCst), 2);
    server.abort();
}
