//! Façade tests for Cloud session `runtime.watch`.

use std::{path::PathBuf, process::Command, time::Duration};

use ployz::sdk;
use ployz_core::{
    CapabilityName, ContainerId, ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, DockerVolume,
    DockerVolumeId, DockerVolumeName, MachineId, PROTOCOL_MAJOR, RUNTIME_WATCH_CAPABILITY,
    RpcErrorCode, RuntimeWatchFrame, RuntimeWatchRequest,
};
use serde_json::Value;
use tokio::time::timeout;
use tonic::Status;

use super::relay::{self, FakeMachine, RelaySession};
use super::support::{DiscoveryService, native_addon};

const FROZEN_FRAME: &str =
    include_str!("../../../ployz-core/tests/fixtures/runtime_watch_frame.json");

#[tokio::test]
async fn missing_watch_capability_is_unsupported_and_never_polls_list_rpcs() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let service = DiscoveryService::new(description.clone());
    let _machine = session
        .spawn_machine(description.machine_id, service.clone())
        .await;
    let client = connect(&session.url, description.machine_id.as_str()).await;

    let error = match client.watch().await {
        Ok(_) => panic!("Watch must fail when the capability is absent"),
        Err(error) => error,
    };

    assert_eq!(error.code, RpcErrorCode::Unsupported);
    assert!(error.message.contains(RUNTIME_WATCH_CAPABILITY));
    assert_eq!(
        service
            .watch_opens
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert_no_list_rpc(&service);
    assert!(
        client
            .about()
            .await
            .unwrap()
            .supports(DESCRIBE_CONTRACT_CAPABILITY)
    );
}

#[tokio::test]
async fn first_watch_yield_is_the_complete_generated_frame() {
    let (client, service, _session, _machine) = watching_session().await;
    let first = frozen_frame();
    service.push_watch_frame(first.clone());
    let watch = client.watch().await.unwrap();

    let frame = next_frame(&watch).await;

    assert_eq!(frame, first);
    assert_redacted(&frame);
    assert!(
        !frame.volumes.is_empty(),
        "replicated Docker Volumes belong on the frame"
    );
    let incomplete =
        ContainerId::parse("2222222222222222222222222222222222222222222222222222222222222222")
            .unwrap();
    assert!(frame.incomplete_ids.containers.contains(&incomplete));
    assert!(
        !frame
            .containers
            .iter()
            .any(|container| container.container_id == incomplete),
        "an incomplete ID is not a delete of a present row"
    );
    assert_eq!(
        *service.watch_requests.lock().unwrap(),
        [RuntimeWatchRequest {}]
    );
    assert_no_list_rpc(&service);
}

#[tokio::test]
async fn changed_frame_yields_another_complete_frame() {
    let (client, service, _session, _machine) = watching_session().await;
    let first = frozen_frame();
    let second = frame_with_extra_volume(&first);
    service.push_watch_frame(first.clone());
    let watch = client.watch().await.unwrap();
    assert_eq!(next_frame(&watch).await, first);

    service.push_watch_frame(second.clone());

    let changed = next_frame(&watch).await;
    assert_eq!(changed, second);
    assert_ne!(changed, first);
    assert_no_list_rpc(&service);
}

#[tokio::test]
async fn unchanged_observation_does_not_yield() {
    let (client, service, _session, _machine) = watching_session().await;
    service.push_watch_frame(frozen_frame());
    let watch = client.watch().await.unwrap();
    let _first = next_frame(&watch).await;

    assert!(
        timeout(Duration::from_millis(100), watch.next())
            .await
            .is_err(),
        "an unchanged tick must not yield another frame"
    );
    assert_no_list_rpc(&service);
    watch.cancel();
}

#[tokio::test]
async fn abort_ends_only_that_watch_and_leaves_the_client_usable() {
    let (client, service, _session, _machine) = watching_session().await;
    service.push_watch_frame(frozen_frame());
    let watch = client.watch().await.unwrap();
    let _first = next_frame(&watch).await;
    let waiting = watch.next();
    tokio::pin!(waiting);
    assert!(
        timeout(Duration::from_millis(50), &mut waiting)
            .await
            .is_err()
    );

    watch.cancel();

    assert_eq!(
        timeout(Duration::from_secs(1), waiting)
            .await
            .expect("cancel must unblock next()")
            .unwrap(),
        None
    );
    wait_watch_rpc_dropped(&service).await;
    assert!(
        client
            .about()
            .await
            .unwrap()
            .supports(DESCRIBE_CONTRACT_CAPABILITY)
    );

    service.push_watch_frame(frozen_frame());
    let again = client.watch().await.unwrap();
    let _ = next_frame(&again).await;
    again.cancel();
    wait_watch_rpc_dropped(&service).await;
    assert_eq!(
        service
            .watch_opens
            .load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    assert_no_list_rpc(&service);
}

#[tokio::test]
async fn store_or_rpc_failure_ends_the_iterable_with_a_typed_error() {
    let (client, service, _session, _machine) = watching_session().await;
    service.push_watch_frame(frozen_frame());
    let watch = client.watch().await.unwrap();
    let _first = next_frame(&watch).await;

    service.fail_watch(Status::unavailable("store closed"));

    let error = timeout(Duration::from_secs(1), watch.next())
        .await
        .expect("failure must end the iterable")
        .expect_err("daemon failure is a typed error");
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert!(error.message.contains("store closed"));
}

#[tokio::test]
async fn reconnect_starts_with_a_fresh_complete_frame_and_no_cursor() {
    let (client, service, _session, _machine) = watching_session().await;
    let first = frozen_frame();
    let second = frame_with_extra_volume(&first);
    service.push_watch_frame(first.clone());
    let watch = client.watch().await.unwrap();
    assert_eq!(next_frame(&watch).await, first);
    watch.cancel();
    assert_eq!(
        timeout(Duration::from_secs(1), watch.next())
            .await
            .expect("cancel must end the first Watch")
            .unwrap(),
        None
    );
    drop(watch);

    let watch = client.watch().await.unwrap();
    service.push_watch_frame(second.clone());
    let reconnect = next_frame(&watch).await;
    assert_eq!(reconnect, second);
    assert_ne!(reconnect, first);
    assert_eq!(
        *service.watch_requests.lock().unwrap(),
        [RuntimeWatchRequest {}, RuntimeWatchRequest {}]
    );
    watch.cancel();
}

#[tokio::test]
async fn node_watch_covers_abort_signal_and_async_iteration() {
    let description = watch_description();
    let session = RelaySession::start().await;
    let service = DiscoveryService::new(description.clone());
    service.emit_watch_frame_on_open(frozen_frame());
    let _machine = session
        .spawn_machine(description.machine_id, service.clone())
        .await;
    let addon = native_addon();
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("ployz-sdk");
    let script = package.join("tests/node_watch.js");
    let url = session.url.clone();
    let machine_id = description.machine_id.as_str().to_owned();

    let output = timeout(
        Duration::from_secs(20),
        tokio::task::spawn_blocking(move || {
            Command::new("node")
                .arg(&script)
                .env("PLOYZ_SDK_ADDON", addon)
                .env("PLOYZ_SDK_PACKAGE", package)
                .env("PLOYZ_RELAY_URL", url)
                .env("PLOYZ_BEARER", relay::DIAL)
                .env("PLOYZ_PAIRING", relay::PAIRING)
                .env("PLOYZ_MACHINE_ID", machine_id)
                .output()
        }),
    )
    .await
    .expect("Node Watch smoke must not hang")
    .expect("Node Watch smoke task joins")
    .expect("Node Watch smoke spawns");

    assert!(
        output.status.success(),
        "Node Watch smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        service
            .watch_opens
            .load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    assert_no_list_rpc(&service);
}

async fn watching_session() -> (sdk::Session, DiscoveryService, RelaySession, FakeMachine) {
    let description = watch_description();
    let session = RelaySession::start().await;
    let service = DiscoveryService::new(description.clone());
    let machine = session
        .spawn_machine(description.machine_id, service.clone())
        .await;
    let client = connect(&session.url, description.machine_id.as_str()).await;
    (client, service, session, machine)
}

async fn connect(url: &str, machine_id: &str) -> sdk::Session {
    timeout(
        Duration::from_secs(5),
        sdk::connect(url, relay::DIAL, relay::PAIRING, machine_id),
    )
    .await
    .expect("connect must not hang")
    .unwrap()
}

async fn next_frame(watch: &sdk::Watch) -> RuntimeWatchFrame {
    timeout(Duration::from_secs(2), watch.next())
        .await
        .expect("Watch frame")
        .expect("Watch result")
        .expect("Watch item")
}

async fn wait_watch_rpc_dropped(service: &DiscoveryService) {
    timeout(Duration::from_secs(2), async {
        loop {
            if service.live_watch_senders() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("cancel must drop the Watch RPC without another next()");
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

fn watch_description() -> ContractDescription {
    ContractDescription {
        machine_id: MachineId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "do-not-branch-on-me".into(),
        capabilities: [
            CapabilityName::parse(DESCRIBE_CONTRACT_CAPABILITY)
                .expect("catalogued capability names are valid"),
            CapabilityName::parse(RUNTIME_WATCH_CAPABILITY)
                .expect("catalogued capability names are valid"),
        ]
        .into(),
    }
}

fn frozen_frame() -> RuntimeWatchFrame {
    serde_json::from_str(FROZEN_FRAME).unwrap()
}

fn frame_with_extra_volume(frame: &RuntimeWatchFrame) -> RuntimeWatchFrame {
    let mut changed = frame.clone();
    changed.volumes.push(DockerVolume {
        id: DockerVolumeId {
            machine_id: MachineId::parse("0123456789abcdef0123456789abcdef").unwrap(),
            name: DockerVolumeName::parse("logs").unwrap(),
        },
        driver: "local".into(),
        options: Default::default(),
        labels: Default::default(),
    });
    changed
}

fn assert_redacted(frame: &RuntimeWatchFrame) {
    let payload = serde_json::to_value(frame).unwrap();
    let text = payload.to_string();
    for forbidden in [
        "BEGIN CERTIFICATE",
        "BEGIN PRIVATE KEY",
        "private_key",
        "challenge_token",
        "challenge_response",
        "renewal_token",
        "dns_endpoint",
    ] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} must not appear on the Watch frame"
        );
    }
    assert_eq!(
        payload.get("hosted_dns_hostname"),
        Some(&Value::String("cluster.example.ts.net".into()))
    );
}

fn assert_no_list_rpc(service: &DiscoveryService) {
    assert_eq!(
        service
            .list_rpc_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}
