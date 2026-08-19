//! Façade tests for Cloud session volume removal.

use std::{path::PathBuf, process::Command, time::Duration};

use ployz::sdk;
use ployz_core::{
    DESCRIBE_CONTRACT_CAPABILITY, DockerVolumeId, DockerVolumeName, MachineId,
    MembershipObservation, RemoveVolumesRequest, RpcErrorCode,
};
use tokio::time::timeout;

use super::relay::{self, RelaySession};
use super::support::{DiscoveryService, machine, machine_id, native_addon};

#[tokio::test]
async fn remove_volumes_destroys_named_volumes_on_a_live_machine() {
    let (client, _session, _machine) = volume_session().await;

    let result = client
        .remove_volumes(remove([volume('a', "data")], false))
        .await
        .unwrap();

    assert_eq!(result.successes.len(), 1);
    assert_eq!(
        result.successes.first().map(|success| success.machine_id),
        Some(machine_id('a'))
    );
    assert_eq!(
        result
            .successes
            .first()
            .map(|success| success.value.as_str()),
        Some("data")
    );
    assert!(result.failures.is_empty());
    assert!(result.omissions.is_empty());
    assert!(
        client
            .about()
            .await
            .unwrap()
            .supports(DESCRIBE_CONTRACT_CAPABILITY)
    );
}

#[tokio::test]
async fn remove_volumes_keeps_successes_when_another_machine_fails() {
    let (client, _session, _machine) = volume_session().await;

    let result = client
        .remove_volumes(remove([volume('a', "data"), volume('b', "data")], false))
        .await
        .unwrap();

    assert_eq!(
        result
            .successes
            .iter()
            .map(|success| success.machine_id)
            .collect::<Vec<_>>(),
        vec![machine_id('a')]
    );
    let [failure] = result.failures.as_slice() else {
        panic!("expected one failure: {result:?}")
    };
    assert_eq!(failure.machine_id, machine_id('b'));
    assert_eq!(failure.error.code, RpcErrorCode::Unavailable);
    assert!(result.omissions.is_empty());
}

#[tokio::test]
async fn remove_volumes_treats_not_found_as_a_per_volume_failure() {
    let (client, _session, _machine) = volume_session().await;

    let result = client
        .remove_volumes(remove([volume('a', "data"), volume('a', "missing")], false))
        .await
        .unwrap();

    assert_eq!(result.successes.len(), 1);
    assert_eq!(
        result
            .successes
            .first()
            .map(|success| success.value.as_str()),
        Some("data")
    );
    let [failure] = result.failures.as_slice() else {
        panic!("expected one not-found failure: {result:?}")
    };
    assert_eq!(failure.machine_id, machine_id('a'));
    assert_eq!(failure.error.code, RpcErrorCode::NotFound);
}

#[tokio::test]
async fn remove_volumes_force_is_off_by_default() {
    let (client, _session, _machine) = volume_session().await;

    let blocked = client
        .remove_volumes(remove([volume('a', "busy")], false))
        .await
        .unwrap();
    let [failure] = blocked.failures.as_slice() else {
        panic!("expected in-use failure: {blocked:?}")
    };
    assert_eq!(failure.error.code, RpcErrorCode::Conflict);

    let forced = client
        .remove_volumes(remove([volume('a', "busy")], true))
        .await
        .unwrap();
    assert!(forced.all_targets_succeeded());
    assert_eq!(
        forced
            .successes
            .first()
            .map(|success| success.value.as_str()),
        Some("busy")
    );
}

#[tokio::test]
async fn remove_volumes_omits_machines_that_do_not_invite_rpc() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let mut service = DiscoveryService::new(description.clone());
    let mut down = machine('c', "down");
    down.membership = MembershipObservation::Down;
    service.machines = vec![machine('a', "one"), machine('b', "two"), down];
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let client = sdk::connect(
        &session.url,
        relay::DIAL,
        relay::PAIRING,
        description.machine_id.as_str(),
    )
    .await
    .unwrap();

    let result = client
        .remove_volumes(remove(
            [
                volume('a', "data"),
                volume('c', "data"),
                volume('c', "logs"),
            ],
            false,
        ))
        .await
        .unwrap();

    assert_eq!(result.successes.len(), 1);
    assert_eq!(result.omissions, vec![machine_id('c')]);
    assert!(result.failures.is_empty());
}

#[tokio::test]
async fn remove_volumes_after_close_is_unavailable() {
    let (client, _session, _machine) = volume_session().await;
    client.close().await;
    let error = client
        .remove_volumes(remove([volume('a', "data")], false))
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unavailable);
}

#[tokio::test]
async fn node_smoke_covers_successful_and_partial_volume_removal() {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![machine('a', "one"), machine('b', "two")];
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let addon = native_addon();
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("ployz-sdk");
    let script = package.join("tests/node_volumes.js");
    let url = session.url.clone();
    let entry_id = description.machine_id.as_str().to_owned();
    let machine_a = machine_id('a').as_str().to_owned();
    let machine_b = machine_id('b').as_str().to_owned();

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
                .env("PLOYZ_MACHINE_ID", entry_id)
                .env("PLOYZ_MACHINE_A", machine_a)
                .env("PLOYZ_MACHINE_B", machine_b)
                .output()
        }),
    )
    .await
    .expect("Node volume smoke must not hang")
    .expect("Node volume smoke task joins")
    .expect("Node volume smoke spawns");

    assert!(
        output.status.success(),
        "Node volume smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn volume_session() -> (sdk::Session, RelaySession, super::relay::FakeMachine) {
    let description = advertised_description();
    let session = RelaySession::start().await;
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![machine('a', "one"), machine('b', "two")];
    let machine = session.spawn_machine(description.machine_id, service).await;
    let client = sdk::connect(
        &session.url,
        relay::DIAL,
        relay::PAIRING,
        description.machine_id.as_str(),
    )
    .await
    .unwrap();
    (client, session, machine)
}

fn advertised_description() -> ployz_core::ContractDescription {
    ployz_core::ContractDescription {
        machine_id: MachineId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        protocol_major: ployz_core::PROTOCOL_MAJOR,
        daemon_version: "do-not-branch-on-me".into(),
        capabilities: [
            ployz_core::CapabilityName::parse(DESCRIBE_CONTRACT_CAPABILITY)
                .expect("catalogued capability names are valid"),
        ]
        .into(),
    }
}

fn remove<const N: usize>(volumes: [DockerVolumeId; N], force: bool) -> RemoveVolumesRequest {
    RemoveVolumesRequest {
        volumes: volumes.into(),
        force,
    }
}

fn volume(machine: char, name: &str) -> DockerVolumeId {
    DockerVolumeId {
        machine_id: machine_id(machine),
        name: DockerVolumeName::parse(name).unwrap(),
    }
}
