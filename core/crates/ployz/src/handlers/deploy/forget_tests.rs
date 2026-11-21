use ployz_core::{
    CapabilityName, ContractDescription, MachineList, MachineObservation, MembershipObservation,
    OpaquePayload, PROTOCOL_MAJOR, QualifiedService, RpcRequestBody, RpcResponse,
    VersionsForgotten, authority::Timestamp,
};
use tonic::{Request, Response, Status};

use super::*;
use crate::global_fixtures::machine;

fn id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

fn forgot(service: &str) -> VersionsForgotten {
    VersionsForgotten {
        outcomes: vec![ForgetOutcome::Raised {
            service: QualifiedService::parse(service).unwrap(),
            floor: Timestamp::from_millis(1_800_000_000_000).unwrap(),
        }],
    }
}

/// Three Members that keep floors, each answering the forget with `answer`; one it gives
/// `None` is down.
async fn members(
    answer: fn(MachineId) -> Option<VersionsForgotten>,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| async move {
        let target = rpc.metadata().get("machine").map_or(id('1'), |value| {
            MachineId::parse(value.to_str().unwrap()).unwrap()
        });
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "the fixture answers only what the forget fan-out sends"
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
                attest: None,
                machine_id: target,
                protocol_major: PROTOCOL_MAJOR,
                daemon_version: "fixture".into(),
                capabilities: [CapabilityName::parse(FORGET_VERSIONS_CAPABILITY).unwrap()].into(),
                sealing: None,
            }),
            RpcRequestBody::ForgetVersions(_) => RpcResponse::from(
                answer(target).ok_or_else(|| Status::unavailable("the server is down"))?,
            ),
            other => panic!("the forget fan-out sent {other:?}"),
        };
        Ok(Response::new(response.encode().unwrap()))
    })
    .await
}

/// A Member that misses the forget keeps admitting older versions, so it is named and the
/// result is partial; the Members that answered still report what they forgot.
#[tokio::test(start_paused = true)]
async fn a_member_that_does_not_answer_the_forget_makes_the_result_partial() {
    let (mut client, server) =
        members(|target| (target != id('2')).then(|| forgot("app/web"))).await;

    let forgot = ask(
        &mut client,
        ForgetVersionsRequest {
            versions: Vec::new(),
        },
    )
    .await
    .unwrap();

    assert!(!forgot.is_complete());
    let json = serde_json::to_value(&forgot).unwrap();
    let answered: Vec<_> = json
        .pointer("/machines")
        .and_then(serde_json::Value::as_array)
        .unwrap()
        .iter()
        .filter_map(|answer| answer.pointer("/server")?.as_str())
        .collect();
    assert_eq!(answered, ["machine-1", "machine-3"]);
    assert_eq!(
        json.pointer("/failures/0/machine_id"),
        Some(&serde_json::json!(id('2').to_string()))
    );
    assert_eq!(
        json.pointer("/failures/0/server"),
        Some(&serde_json::json!("machine-2"))
    );
    assert_eq!(
        json.pointer("/machines/0/outcomes/0/outcome"),
        Some(&serde_json::json!("raised"))
    );
    server.abort();
}

/// A Member that answers but refuses the forget, as one whose clock runs behind does,
/// still admits older versions, so the result is partial.
#[tokio::test(start_paused = true)]
async fn a_member_that_refuses_the_forget_makes_the_result_partial() {
    let (mut client, server) = members(|target| {
        Some(if target == id('3') {
            VersionsForgotten {
                outcomes: vec![ForgetOutcome::Refused {
                    reason: "This forget is stamped ahead of this server's clock.".into(),
                }],
            }
        } else {
            forgot("app/web")
        })
    })
    .await;

    let forgot = ask(
        &mut client,
        ForgetVersionsRequest {
            versions: Vec::new(),
        },
    )
    .await
    .unwrap();

    assert!(!forgot.is_complete());
    server.abort();
}
