//! Façade tests for Cloud session connect / about / deploy / close.

use std::{path::PathBuf, process::Command, time::Duration};

use ployz::deploy::{DeployIntent, PlanOptions};
use ployz::sdk;
use ployz_core::{
    CapabilityName, ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, DeployOperation,
    DeployOutcome, ExecutionError, FailedOperation, MachineId, PROTOCOL_MAJOR,
    RequestedServiceSpec, RpcErrorCode,
};
use tokio::time::timeout;

use super::relay::{self, RelaySession};
use super::support::{DiscoveryService, native_addon};

#[tokio::test]
async fn connect_about_returns_contract_and_branches_on_capability_names() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;

    let client = timeout(
        Duration::from_secs(5),
        sdk::connect(&session.url, relay::DIAL, description.machine_id.as_str()),
    )
    .await
    .expect("connect must not hang")
    .unwrap();

    let about = client.about().await.unwrap();
    assert_eq!(about.machine_id, description.machine_id);
    assert!(
        about.supports(DESCRIBE_CONTRACT_CAPABILITY),
        "callers branch on capability names, not daemon_version"
    );
    assert_eq!(about.daemon_version, description.daemon_version);
}

#[tokio::test]
async fn bad_credentials_and_unknown_machines_reject_with_typed_errors() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let machine_id = description.machine_id.as_str();

    let empty = timeout(
        Duration::from_secs(2),
        sdk::connect(&session.url, "", machine_id),
    )
    .await
    .expect("empty Dial Credential must not hang");
    let empty = match empty {
        Ok(_) => panic!("expected empty Dial Credential to fail"),
        Err(error) => error,
    };
    assert_eq!(empty.code, RpcErrorCode::Unauthenticated);

    let bad = timeout(
        Duration::from_secs(2),
        sdk::connect(&session.url, "wrong-secret", machine_id),
    )
    .await
    .expect("bad Dial Credential must not hang");
    let bad = match bad {
        Ok(_) => panic!("expected invalid Dial Credential to fail"),
        Err(error) => error,
    };
    assert_eq!(bad.code, RpcErrorCode::Unauthenticated);

    let unknown = timeout(
        Duration::from_secs(2),
        sdk::connect(&session.url, relay::DIAL, MachineId::random().as_str()),
    )
    .await
    .expect("unknown Machine ID must not hang");
    let unknown = match unknown {
        Ok(_) => panic!("expected unknown Machine ID to fail"),
        Err(error) => error,
    };
    assert_eq!(unknown.code, RpcErrorCode::NotFound);

    let invalid = timeout(
        Duration::from_secs(2),
        sdk::connect(&session.url, relay::DIAL, "not-a-machine-id"),
    )
    .await
    .expect("invalid Machine ID must not hang");
    let invalid = match invalid {
        Ok(_) => panic!("expected invalid Machine ID to fail"),
        Err(error) => error,
    };
    assert_eq!(invalid.code, RpcErrorCode::InvalidArgument);
}

#[tokio::test]
async fn close_drops_the_session_and_repeated_lifecycle_works() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let machine_id = description.machine_id.as_str();

    for _ in 0..3 {
        let client = sdk::connect(&session.url, relay::DIAL, machine_id)
            .await
            .unwrap();
        assert!(
            client
                .about()
                .await
                .unwrap()
                .supports(DESCRIBE_CONTRACT_CAPABILITY)
        );
        client.close().await;
        let closed = match client.about().await {
            Ok(_) => panic!("about() after close must fail"),
            Err(error) => error,
        };
        assert_eq!(closed.code, RpcErrorCode::Unavailable);
        client.close().await;
    }
}

#[tokio::test]
async fn deploy_returns_success_for_a_completed_run() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let client = sdk::connect(&session.url, relay::DIAL, description.machine_id.as_str())
        .await
        .unwrap();

    let outcome = client
        .deploy(DeployIntent::apply_one(spec("web"), skip_health()))
        .await
        .unwrap();

    let DeployOutcome::Success { completed } = outcome else {
        panic!("expected success: {outcome:?}");
    };
    assert_eq!(completed.len(), 1);
    assert!(matches!(
        completed.first(),
        Some(DeployOperation::RunContainer { spec, skip_health_monitor: true, .. })
            if spec.name.as_str() == "web"
    ));
    assert!(
        client
            .about()
            .await
            .unwrap()
            .supports(DESCRIBE_CONTRACT_CAPABILITY)
    );
}

#[tokio::test]
async fn deploy_preserves_completed_prefix_failed_op_and_unexecuted_suffix() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let client = sdk::connect(&session.url, relay::DIAL, description.machine_id.as_str())
        .await
        .unwrap();
    let outcome = client
        .deploy(DeployIntent::apply_one(
            spec_with_volume("web", "scratch"),
            skip_health(),
        ))
        .await
        .unwrap();

    let DeployOutcome::Failed {
        completed,
        failed,
        unexecuted,
    } = outcome
    else {
        panic!("expected partial failure: {outcome:?}");
    };
    assert!(completed.is_empty());
    assert!(matches!(
        failed,
        FailedOperation::Operation {
            operation: DeployOperation::CreateVolume { volume, .. },
            error: ExecutionError::Machine { .. },
        } if volume.reference.as_str() == "scratch"
    ));
    assert_eq!(unexecuted.len(), 1);
    assert!(matches!(
        unexecuted.first(),
        Some(DeployOperation::RunContainer { spec, .. }) if spec.name.as_str() == "web"
    ));
    assert!(
        client
            .about()
            .await
            .unwrap()
            .supports(DESCRIBE_CONTRACT_CAPABILITY)
    );
}

#[tokio::test]
async fn deploy_planning_error_is_a_typed_rpc_error() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(description.machine_id, {
            let mut service = DiscoveryService::new(description.clone());
            service.machines.clear();
            service
        })
        .await;
    let client = sdk::connect(&session.url, relay::DIAL, description.machine_id.as_str())
        .await
        .unwrap();

    let error = client
        .deploy(DeployIntent::apply_one(spec("web"), skip_health()))
        .await
        .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(
        client
            .about()
            .await
            .unwrap()
            .supports(DESCRIBE_CONTRACT_CAPABILITY)
    );
}

#[tokio::test]
async fn node_smoke_covers_connect_about_deploy_and_close() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(
            description.machine_id,
            DiscoveryService::new(description.clone()),
        )
        .await;
    let addon = native_addon();
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("ployz-sdk");
    let script = package.join("tests/node_smoke.js");
    let url = session.url.clone();
    let machine_id = description.machine_id.as_str().to_owned();
    let unknown = MachineId::random().as_str().to_owned();

    let output = timeout(
        Duration::from_secs(20),
        tokio::task::spawn_blocking(move || {
            Command::new("node")
                .arg(&script)
                .env("PLOYZ_SDK_ADDON", addon)
                .env("PLOYZ_SDK_PACKAGE", package)
                .env("PLOYZ_RELAY_URL", url)
                .env("PLOYZ_BEARER", relay::DIAL)
                .env("PLOYZ_MACHINE_ID", machine_id)
                .env("PLOYZ_UNKNOWN_MACHINE_ID", unknown)
                .output()
        }),
    )
    .await
    .expect("Node smoke must not hang")
    .expect("Node smoke task joins")
    .expect("Node smoke spawns");

    assert!(
        output.status.success(),
        "Node smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn advertised_description() -> ContractDescription {
    ContractDescription {
        machine_id: MachineId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "do-not-branch-on-me".into(),
        capabilities: [CapabilityName::parse(DESCRIBE_CONTRACT_CAPABILITY)
            .expect("catalogued capability names are valid")]
        .into(),
    }
}
fn spec(name: &str) -> RequestedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "always" }
    }))
    .unwrap()
}

fn spec_with_volume(name: &str, volume: &str) -> RequestedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "always" },
        "volumes": [{
            "reference": volume,
            "source": { "kind": "named", "name": volume }
        }],
        "mounts": [{ "volume": volume, "target": format!("/{volume}") }]
    }))
    .unwrap()
}

fn skip_health() -> PlanOptions {
    PlanOptions {
        skip_health_monitor: true,
        ..PlanOptions::default()
    }
}
