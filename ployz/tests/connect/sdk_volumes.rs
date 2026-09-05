//! Façade tests for Cloud session volume removal.

use std::{path::PathBuf, process::Command, time::Duration};

use ployz::sdk;
use ployz_core::{
    DESCRIBE_CONTRACT_CAPABILITY, DockerVolumeId, DockerVolumeName, MachineId,
    MembershipObservation, RemoveVolumesRequest, RpcErrorCode, VolumeRemoval, VolumeRemovalOutcome,
};
use tokio::time::timeout;

use super::relay::{self, RelaySession};
use super::support::{DiscoveryService, machine, machine_id, native_addon};

#[tokio::test]
async fn removal_outcomes_retain_each_volume_identity() {
    let (client, _session, _machine) = volume_session().await;
    let result = client
        .remove_volumes(remove(
            [
                volume('b', "logs"),
                volume('a', "data"),
                volume('b', "data"),
                volume('a', "busy"),
                volume('c', "logs"),
                volume('c', "data"),
            ],
            false,
        ))
        .await
        .unwrap();
    let wire = serde_json::to_value(result).unwrap();
    let outcomes = wire
        .as_array()
        .expect("one outcome per requested Docker Volume");
    assert_eq!(outcomes.len(), 6);
    for (machine, name, status) in [
        ('b', "logs", "failed"),
        ('a', "data", "removed"),
        ('b', "data", "failed"),
        ('a', "busy", "failed"),
        ('c', "logs", "omitted"),
        ('c', "data", "omitted"),
    ] {
        let outcome = outcomes
            .iter()
            .find(|entry| entry["id"] == serde_json::to_value(volume(machine, name)).unwrap())
            .unwrap();
        assert_eq!(
            outcome
                .pointer("/outcome/status")
                .and_then(serde_json::Value::as_str),
            Some(status)
        );
        if machine == 'b' {
            assert_eq!(
                outcome
                    .pointer("/outcome/error/message")
                    .and_then(serde_json::Value::as_str),
                Some("target unavailable")
            );
        }
    }
}

#[tokio::test]
async fn remove_volumes_destroys_named_volumes_on_a_live_machine() {
    let (client, _session, _machine) = volume_session().await;

    let result = client
        .remove_volumes(remove([volume('a', "data")], false))
        .await
        .unwrap();

    assert_eq!(
        result,
        vec![VolumeRemoval {
            id: volume('a', "data"),
            outcome: VolumeRemovalOutcome::Removed
        }]
    );
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

    let [success, failure] = result.as_slice() else {
        panic!("expected two volume outcomes: {result:?}")
    };
    assert_eq!(
        success,
        &VolumeRemoval {
            id: volume('a', "data"),
            outcome: VolumeRemovalOutcome::Removed
        }
    );
    assert_eq!(failure.id, volume('b', "data"));
    assert!(
        matches!(&failure.outcome, VolumeRemovalOutcome::Failed { error } if error.code == RpcErrorCode::Unavailable)
    );
}

#[tokio::test]
async fn remove_volumes_treats_not_found_as_success() {
    let (client, _session, _machine) = volume_session().await;

    let result = client
        .remove_volumes(remove([volume('a', "data"), volume('a', "missing")], false))
        .await
        .unwrap();

    assert_eq!(
        result,
        ["data", "missing"].map(|name| VolumeRemoval {
            id: volume('a', name),
            outcome: VolumeRemovalOutcome::Removed
        })
    );
}

#[tokio::test]
async fn remove_volumes_force_is_off_by_default() {
    let (client, _session, _machine) = volume_session().await;

    let blocked = client
        .remove_volumes(remove([volume('a', "busy")], false))
        .await
        .unwrap();
    let [failure] = blocked.as_slice() else {
        panic!("expected one volume outcome: {blocked:?}")
    };
    assert_eq!(failure.id, volume('a', "busy"));
    assert!(
        matches!(&failure.outcome, VolumeRemovalOutcome::Failed { error } if error.code == RpcErrorCode::Conflict)
    );

    let forced = client
        .remove_volumes(remove([volume('a', "busy")], true))
        .await
        .unwrap();
    assert_eq!(
        forced,
        vec![VolumeRemoval {
            id: volume('a', "busy"),
            outcome: VolumeRemovalOutcome::Removed
        }]
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

    assert_eq!(
        result,
        vec![
            VolumeRemoval {
                id: volume('a', "data"),
                outcome: VolumeRemovalOutcome::Removed
            },
            VolumeRemoval {
                id: volume('c', "data"),
                outcome: VolumeRemovalOutcome::Omitted
            },
            VolumeRemoval {
                id: volume('c', "logs"),
                outcome: VolumeRemovalOutcome::Omitted
            },
        ]
    );
}

#[tokio::test]
async fn timed_out_removal_retains_identity_and_unknown_completion() {
    let (client, _session, _machine) = volume_session().await;
    let outcomes = timeout(
        Duration::from_secs(15),
        client.remove_volumes(remove([volume('a', "slow"), volume('a', "data")], false)),
    )
    .await
    .expect("removal attempt is bounded")
    .unwrap();
    let [slow, data] = outcomes.as_slice() else {
        panic!("expected both volume outcomes: {outcomes:?}")
    };
    assert_eq!(slow.id, volume('a', "slow"));
    let VolumeRemovalOutcome::Failed { error } = &slow.outcome else {
        panic!("timeout is not removal or omission: {slow:?}")
    };
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "target Machine RPC timed out");
    assert_eq!(
        data,
        &VolumeRemoval {
            id: volume('a', "data"),
            outcome: VolumeRemovalOutcome::Removed
        }
    );
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
