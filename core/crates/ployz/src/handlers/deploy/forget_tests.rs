use std::time::Duration;

use ployz_core::{
    CapabilityName, ContractDescription, MachineList, MachineObservation, MembershipObservation,
    Namespace, OpaquePayload, PROTOCOL_MAJOR, RpcRequestBody, RpcResponse, SealingPublicKey,
    ServiceMode, ServiceName, VersionsForgotten,
    authority::{PayloadDigest, Signer, SignerKey, Timestamp},
};
use tonic::{Request, Response, Status};

use super::*;
use crate::global_fixtures::{machine, requested};

fn id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

/// A version of `app/<service>` this Device signed marked to forget.
fn version(service: &str) -> Signed<ServiceVersion> {
    let signer = Signer::from_seed(&[1; 32]);
    let namespace = Namespace::parse("app").unwrap();
    Signed::sign(
        &ServiceVersion {
            forget_older: true,
            parent: None,
            cluster: SealingPublicKey::from_bytes([9; 32]),
            digest: PayloadDigest::of(&requested(ServiceMode::Global), &namespace).unwrap(),
            namespace,
            service: ServiceName::parse(service).unwrap(),
            signed_at: Timestamp::from_millis(1_800_000_000_000).unwrap(),
            signer: SignerKey::Device(signer.public().into()),
            proposal: None,
        },
        &signer,
    )
    .unwrap()
}

fn versions(count: usize) -> Vec<Signed<ServiceVersion>> {
    (0..count).map(|n| version(&format!("s{n}"))).collect()
}

/// How a Member answers one forget request of so many versions.
enum Reply {
    /// Raise every version's floor, as far as one request's budget reaches.
    Forget,
    Refuse,
    Down,
    /// Hold the call open and never answer.
    Silent,
}

/// Three Members that keep floors, each answering a forget request as `answer` says. A
/// Member that forgets refuses each version past the verifications one request may cost,
/// as a real one does.
async fn members(
    answer: fn(MachineId, usize) -> Reply,
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
            RpcRequestBody::ForgetVersions(request) => {
                let outcomes = match answer(target, request.versions.len()) {
                    Reply::Forget => request
                        .versions
                        .iter()
                        .enumerate()
                        .map(|(spent, version)| {
                            let body = version.open().unwrap();
                            if spent < MAX_CHECKED {
                                ForgetOutcome::Raised {
                                    service: QualifiedService::new(body.namespace, body.service),
                                    floor: body.signed_at,
                                }
                            } else {
                                ForgetOutcome::Refused {
                                    reason: "This request needs too many signature checks.".into(),
                                }
                            }
                        })
                        .collect(),
                    Reply::Refuse => vec![ForgetOutcome::Refused {
                        reason: "This forget is stamped ahead of this server's clock.".into(),
                    }],
                    Reply::Down => return Err(Status::unavailable("the server is down")),
                    Reply::Silent => std::future::pending().await,
                };
                RpcResponse::from(VersionsForgotten { outcomes })
            }
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
    let (mut client, server) = members(|target, _| {
        if target == id('2') {
            Reply::Down
        } else {
            Reply::Forget
        }
    })
    .await;

    let forgot = ask(&mut client, &versions(1)).await.unwrap();

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
    let (mut client, server) = members(|target, _| {
        if target == id('3') {
            Reply::Refuse
        } else {
            Reply::Forget
        }
    })
    .await;

    let forgot = ask(&mut client, &versions(1)).await.unwrap();

    assert!(!forgot.is_complete());
    server.abort();
}

/// A deploy of more Services than one request may verify is still forgotten whole on every
/// healthy Member.
#[tokio::test(start_paused = true)]
async fn every_service_of_a_deploy_past_one_requests_budget_is_forgotten() {
    let (mut client, server) = members(|_, _| Reply::Forget).await;

    let forgot = ask(&mut client, &versions(MAX_CHECKED + 1)).await.unwrap();

    assert!(forgot.is_complete());
    let json = serde_json::to_value(&forgot).unwrap();
    for answer in json.pointer("/machines").unwrap().as_array().unwrap() {
        let raised = answer
            .pointer("/outcomes")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter(|outcome| outcome.pointer("/outcome") == Some(&serde_json::json!("raised")))
            .count();
        assert_eq!(raised, MAX_CHECKED + 1, "{answer}");
    }
    server.abort();
}

/// A Member that holds its call open and never answers is a gap once its deadline passes,
/// and the other Members, the entry included, are still asked.
#[tokio::test(start_paused = true)]
async fn a_member_that_never_answers_is_a_gap_and_the_rest_still_forget() {
    let (mut client, server) = members(|target, _| {
        if target == id('2') {
            Reply::Silent
        } else {
            Reply::Forget
        }
    })
    .await;

    let forgot = tokio::time::timeout(Duration::from_secs(120), ask(&mut client, &versions(1)))
        .await
        .expect("the forget waited on a Member that never answers")
        .unwrap();

    assert!(!forgot.is_complete());
    let json = serde_json::to_value(&forgot).unwrap();
    assert_eq!(
        json.pointer("/failures/0/server"),
        Some(&serde_json::json!("machine-2"))
    );
    let answered: Vec<_> = json
        .pointer("/machines")
        .and_then(serde_json::Value::as_array)
        .unwrap()
        .iter()
        .filter_map(|answer| answer.pointer("/server")?.as_str())
        .collect();
    assert_eq!(answered, ["machine-1", "machine-3"]);
    server.abort();
}

/// A Member that forgets the first batch and misses a later one keeps what it forgot,
/// names the Services it missed, and makes the result partial.
#[tokio::test(start_paused = true)]
async fn a_member_that_misses_a_later_batch_names_its_services() {
    let (mut client, server) = members(|target, sent| {
        if target == id('3') && sent < MAX_CHECKED {
            Reply::Down
        } else {
            Reply::Forget
        }
    })
    .await;

    let forgot = ask(&mut client, &versions(MAX_CHECKED + 1)).await.unwrap();

    assert!(!forgot.is_complete());
    let json = serde_json::to_value(&forgot).unwrap();
    let third = json.pointer("/machines/2").unwrap();
    assert_eq!(
        third.pointer("/server"),
        Some(&serde_json::json!("machine-3"))
    );
    assert_eq!(
        third
            .pointer("/outcomes")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        MAX_CHECKED
    );
    assert_eq!(
        third.pointer("/missed/services"),
        Some(&serde_json::json!([format!("app/s{MAX_CHECKED}")]))
    );
    server.abort();
}

/// Machine 4 joins through machine 2 once the forget reached machine 2, sealed the floors
/// machine 2 held before it. The fan-out lists the Members again, so machine 4 forgets too,
/// though machine 2 doesn't see it up yet.
#[tokio::test(start_paused = true)]
async fn a_member_that_joins_while_the_forget_runs_forgets_too() {
    let forgot_on = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = forgot_on.clone();
    let (mut client, server) =
        crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
            let seen = seen.clone();
            async move {
                let target = rpc.metadata().get("machine").map_or(id('1'), |value| {
                    MachineId::parse(value.to_str().unwrap()).unwrap()
                });
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "the fixture answers only what the forget fan-out sends"
                )]
                let response = match rpc.into_inner().decode_request().unwrap().body {
                    RpcRequestBody::ListMachines(_) => {
                        let joined = target == id('2') && seen.lock().unwrap().contains(&target);
                        RpcResponse::from(MachineList {
                            enrollment: None,
                            machines: ['1', '2', '3', '4']
                                .into_iter()
                                .filter(|hex| *hex != '4' || joined)
                                .map(|hex| {
                                    MachineObservation::new(
                                        machine(hex, &format!("machine-{hex}")),
                                        if hex == '4' {
                                            MembershipObservation::Unknown
                                        } else {
                                            MembershipObservation::Up
                                        },
                                    )
                                })
                                .collect(),
                        })
                    }
                    RpcRequestBody::DescribeContract(_) => RpcResponse::from(ContractDescription {
                        attest: None,
                        machine_id: target,
                        protocol_major: PROTOCOL_MAJOR,
                        daemon_version: "fixture".into(),
                        capabilities: [CapabilityName::parse(FORGET_VERSIONS_CAPABILITY).unwrap()]
                            .into(),
                        sealing: None,
                    }),
                    RpcRequestBody::ForgetVersions(request) => {
                        seen.lock().unwrap().push(target);
                        RpcResponse::from(VersionsForgotten {
                            outcomes: request
                                .versions
                                .iter()
                                .map(|version| {
                                    let body = version.open().unwrap();
                                    ForgetOutcome::Raised {
                                        service: QualifiedService::new(
                                            body.namespace,
                                            body.service,
                                        ),
                                        floor: body.signed_at,
                                    }
                                })
                                .collect(),
                        })
                    }
                    other => panic!("the forget fan-out sent {other:?}"),
                };
                Ok(Response::new(response.encode().unwrap()))
            }
        })
        .await;

    let forgot = ask(&mut client, &versions(1)).await.unwrap();

    let mut reached = forgot_on.lock().unwrap().clone();
    reached.sort();
    assert_eq!(reached, [id('1'), id('2'), id('3'), id('4')]);
    assert!(forgot.is_complete());
    server.abort();
}
