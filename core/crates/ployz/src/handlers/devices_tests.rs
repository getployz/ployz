use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};

use ployz_core::{
    CapabilityName, ContractDescription, DeviceKey, DeviceSet, MachineList, MachineObservation,
    MembershipObservation, OpaquePayload, PROTOCOL_MAJOR, RpcRequestBody, RpcResponse,
    SET_DEVICE_CAPABILITY,
};
use tonic::{Request, Response, Status};

use super::*;
use crate::cloud_login::Secret;
use crate::global_fixtures::machine;

fn key(first: u8) -> DeviceKey {
    DeviceKey::from_bytes(std::array::from_fn(|index| first + index as u8))
}

fn id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

/// What each Machine of a three-Machine Cluster does when asked to set a Device.
#[derive(Clone, Copy, PartialEq)]
enum Daemon {
    Approves,
    AlreadyApproved,
    Unreachable,
    PredatesDevices,
}

async fn cluster(
    daemons: [Daemon; 3],
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| async move {
        let target = rpc
            .metadata()
            .get("machine")
            .map(|value| MachineId::parse(value.to_str().unwrap()).unwrap());
        let daemon = ['1', '2', '3']
            .into_iter()
            .zip(daemons)
            .find_map(|(hex, daemon)| (target == Some(id(hex))).then_some(daemon));
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "the fixture answers only what a Device fan-out sends"
        )]
        let response = match rpc.into_inner().decode_request().unwrap().body {
            RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                enrollment: None,
                machines: ['1', '2', '3']
                    .map(|hex| {
                        MachineObservation::new(
                            machine(hex, &format!("machine-{hex}")),
                            MembershipObservation::Up,
                        )
                    })
                    .into(),
            }),
            RpcRequestBody::DescribeContract(_) => RpcResponse::from(ContractDescription {
                machine_id: target.unwrap(),
                protocol_major: PROTOCOL_MAJOR,
                daemon_version: "fixture".into(),
                capabilities: (daemon != Some(Daemon::PredatesDevices))
                    .then(|| CapabilityName::parse(SET_DEVICE_CAPABILITY).unwrap())
                    .into_iter()
                    .collect(),
            }),
            RpcRequestBody::SetDevice(_) if daemon == Some(Daemon::Unreachable) => {
                return Err(Status::unavailable("machine-2 is down"));
            }
            RpcRequestBody::SetDevice(_) => {
                let code = key(0).code();
                RpcResponse::from(if daemon == Some(Daemon::AlreadyApproved) {
                    DeviceSet::AlreadyApproved { code }
                } else {
                    DeviceSet::Approved { code }
                })
            }
            other => panic!("unexpected request {other:?}"),
        };
        Ok(Response::new(response.encode().unwrap()))
    })
    .await
}

fn approve() -> SetDeviceRequest {
    SetDeviceRequest::Approve { key: key(0) }
}

#[tokio::test]
async fn a_key_that_does_not_hash_to_the_code_is_refused_before_any_machine() {
    let cloud = crate::cloud_login::tests::fake_cloud(|route, _| {
        assert_eq!(route, "GET /api/cli/device-requests/mmg42klgyq");
        (
            200,
            json!({ "code": "mmg42klgyq", "key": key(1), "expiresAt": "x" }),
        )
    });
    let credential = Credential::Token {
        cloud,
        token: Secret::new("ployz_secret".to_owned()),
    };
    let calls = AtomicUsize::new(0);
    let code = DeviceCode::parse("mmg42klgyq").unwrap();
    assert_eq!(key(0).code(), code);

    let error = approve_with(&credential, code, async |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .await
    .unwrap_err();

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(error.report().message.contains("doesn't match mmg42klgyq"));
}

#[tokio::test]
async fn one_failing_machine_is_a_partial_result_that_names_it() {
    let (mut client, server) =
        cluster([Daemon::Approves, Daemon::Unreachable, Daemon::Approves]).await;

    let settled = set_everywhere(&mut client, approve()).await.unwrap();

    assert_eq!(settled.answers.len(), 2);
    let failed: Vec<_> = settled
        .gaps
        .failures
        .iter()
        .map(|failure| failure.machine_id)
        .collect();
    assert_eq!(failed, [id('2')]);
    assert!(settled.gaps.omitted.is_empty());
    assert_eq!(settled.gaps.outcome().unwrap_err().printed_exit(), Some(3));
    server.abort();
}

#[tokio::test]
async fn rerunning_where_every_machine_already_approved_succeeds() {
    let (mut client, server) = cluster([Daemon::AlreadyApproved; 3]).await;

    let settled = set_everywhere(&mut client, approve()).await.unwrap();

    assert!(
        settled
            .answers
            .iter()
            .all(|answer| matches!(answer.set, DeviceSet::AlreadyApproved { .. }))
    );
    assert_eq!(settled.answers.len(), 3);
    settled.gaps.outcome().unwrap();
    server.abort();
}

#[tokio::test]
async fn a_machine_without_a_device_roster_is_an_omission() {
    let (mut client, server) =
        cluster([Daemon::Approves, Daemon::PredatesDevices, Daemon::Approves]).await;

    let settled = set_everywhere(&mut client, approve()).await.unwrap();

    assert_eq!(settled.gaps.omitted, [id('2')]);
    assert!(settled.gaps.failures.is_empty());
    assert_eq!(settled.answers.len(), 2);
    server.abort();
}

#[test]
fn a_device_key_other_users_can_read_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = key::path(&dir.path().join("config.yaml"));
    let secret = key::load_or_create(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(key::load_or_create(&path).unwrap(), secret);

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let error = key::load(&path).unwrap_err().to_string();
    assert!(error.contains("chmod 600"), "{error}");
    assert!(error.contains(&path.display().to_string()), "{error}");
}

#[test]
fn shared_scope_refusals_point_at_a_device_request() {
    let refused = unless_full(Error::command(std::io::Error::other(
        "exec needs Full access",
    )));
    assert_eq!(refused.hints(), [request_hint()]);
    let other = unless_full(Error::usage("Service selector is required"));
    assert!(other.hints().is_empty());
}
