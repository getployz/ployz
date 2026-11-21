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

/// Three Members that keep floors. Machine 2 doesn't answer the forget.
async fn members() -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    crate::connect::test_support::rpc_client(|rpc: Request<OpaquePayload>| async move {
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
            RpcRequestBody::ForgetVersions(_) if target == id('2') => {
                return Err(Status::unavailable("machine-2 is down"));
            }
            RpcRequestBody::ForgetVersions(_) => RpcResponse::from(VersionsForgotten {
                outcomes: vec![ForgetOutcome::Raised {
                    service: QualifiedService::parse("app/web").unwrap(),
                    floor: Timestamp::now(),
                }],
            }),
            other => panic!("the forget fan-out sent {other:?}"),
        };
        Ok(Response::new(response.encode().unwrap()))
    })
    .await
}

/// A Member that misses the forget keeps admitting older versions, so it is named and the
/// result is partial; the Members that answered still report what they forgot.
#[tokio::test]
async fn a_member_that_does_not_answer_the_forget_makes_the_result_partial() {
    let (mut client, server) = members().await;

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
    let answered: Vec<_> = json["machines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|answer| answer["server"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(answered, ["machine-1", "machine-3"]);
    assert_eq!(json["failures"][0]["machine_id"], id('2').to_string());
    assert_eq!(json["machines"][0]["outcomes"][0]["outcome"], "raised");
    server.abort();
}
