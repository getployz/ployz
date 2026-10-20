use ployz_core::{ContainerId, DeploymentLogId};

use super::*;

pub(crate) fn deploy(deploy: char) -> DeploymentLogId {
    DeploymentLogId::parse(format!("dep-{deploy}")).unwrap()
}

const UNNAMED_SOURCE: char = '0';

pub(crate) fn pointer(deploy: char, containers: &[char], retiring: &[char]) -> ServingPointer {
    ServingPointer {
        deploy: self::deploy(deploy),
        containers: containers.iter().map(|id| container(*id)).collect(),
        previous: None,
        retiring: retiring.iter().map(|id| container(*id)).collect(),
        deciding: None,
    }
}

/// `pointer` as a rotation leaves it: `previous` names the Deploy its retiring
/// Containers came from.
pub(crate) fn retired_from(mut pointer: ServingPointer, deploy: char) -> ServingPointer {
    pointer.previous = Some(DeploymentLogId::parse(format!("dep-{deploy}")).unwrap());
    pointer
}

pub(crate) fn container(id: char) -> ContainerId {
    ContainerId::parse(id.to_string().repeat(64)).unwrap()
}

fn service() -> QualifiedService {
    QualifiedService::parse("app/api").unwrap()
}

pub(crate) fn replacing(new: char, old: char) -> Rotation {
    Rotation {
        joining: vec![Join {
            copy: container(new),
            provenance: Provenance {
                source: container(old),
                source_deploy: None,
            },
        }],
        leaving: vec![container(old)],
        pruning: Vec::new(),
    }
}

pub(crate) fn labelled(new: char, deploy: char) -> Rotation {
    Rotation {
        joining: vec![Join {
            copy: container(new),
            provenance: Provenance {
                source: container(UNNAMED_SOURCE),
                source_deploy: Some(self::deploy(deploy)),
            },
        }],
        leaving: Vec::new(),
        pruning: Vec::new(),
    }
}

#[test]
fn a_first_pointer_that_gains_retiring_ids_names_its_own_deploy_as_previous() {
    let first = pointer('a', &['1', '2'], &[]);
    assert_eq!(
        replacing('3', '1').apply(&first),
        Applied::Next(retired_from(pointer('a', &['2', '3'], &['1']), 'a'))
    );
}

#[test]
fn a_rotation_that_retires_nothing_leaves_previous_alone() {
    let first = pointer('a', &['1'], &[]);
    assert_eq!(
        labelled('2', 'a').apply(&first),
        Applied::Next(pointer('a', &['1', '2'], &[]))
    );
}

#[test]
fn a_rotation_prunes_only_retiring_ids_the_listing_lacked_before_the_read() {
    let read = retired_from(pointer('a', &['1'], &['8', '9']), 'a');
    let rotation = Rotation {
        joining: vec![Join {
            copy: container('2'),
            provenance: Provenance {
                source: container('1'),
                source_deploy: None,
            },
        }],
        leaving: vec![container('1')],
        pruning: vec![container('8')],
    };
    assert_eq!(
        rotation.apply(&read),
        Applied::Next(retired_from(pointer('a', &['2'], &['9', '1']), 'a')),
        "8 is gone from the listing; 9 still runs and keeps retiring"
    );
    let later = retired_from(pointer('a', &['1'], &['8', '9', '7']), 'a');
    assert_eq!(
        rotation.apply(&later),
        Applied::Next(retired_from(pointer('a', &['2'], &['9', '7', '1']), 'a')),
        "7 retired after the listing; the listing proves nothing about it"
    );
    let prune_only = Rotation {
        pruning: vec![container('8'), container('9')],
        ..Rotation::default()
    };
    assert_eq!(
        prune_only.apply(&read),
        Applied::Next(retired_from(pointer('a', &['1'], &[]), 'a')),
        "pruning alone is a change worth publishing"
    );
}

#[test]
fn a_rotation_joins_with_evidence_retires_named_ids_and_reports_no_change() {
    let current = retired_from(pointer('a', &['1', '2'], &['9']), 'a');
    assert_eq!(
        replacing('3', '1').apply(&current),
        Applied::Next(retired_from(pointer('a', &['2', '3'], &['9', '1']), 'a'))
    );
    assert_eq!(
        labelled('3', 'a').apply(&current),
        Applied::Next(retired_from(pointer('a', &['1', '2', '3'], &['9']), 'a')),
        "a source labelled with the row's deploy is evidence without being named"
    );
    assert_eq!(
        Rotation::leaving([container('9')]).apply(&current),
        Applied::Unchanged
    );
    assert_eq!(
        Rotation::leaving([container('7')]).apply(&current),
        Applied::Unchanged,
        "an id the row does not name never enters retiring"
    );
    assert_eq!(
        replacing('2', '1').apply(&current),
        Applied::Next(retired_from(pointer('a', &['2'], &['9', '1']), 'a')),
        "a copy already pointed joins without evidence"
    );
}

#[test]
fn a_join_without_evidence_is_refused() {
    let current = pointer('b', &['1'], &[]);
    assert_eq!(
        replacing('3', '7').apply(&current),
        Applied::Unproven,
        "7 is neither named nor labelled with dep-b"
    );
    assert_eq!(labelled('3', 'a').apply(&current), Applied::Unproven);
}

mod rpc {
    use std::sync::{Arc, Mutex};

    use ployz_core::{
        MachineList, MachineObservation, MembershipObservation, NotPointerPublisher, OpaquePayload,
        RoutesRead, RpcRequestBody, RpcResponse,
    };
    use tonic::{Request, Response};

    use super::*;
    use crate::global_fixtures::machine;

    struct Cluster {
        row: Arc<Mutex<Option<ServingPointer>>>,
        calls: Arc<Mutex<Vec<(&'static str, String)>>>,
    }

    async fn cluster(
        row: Option<ServingPointer>,
        down: Option<char>,
        refusing: Option<char>,
    ) -> (
        Client,
        Cluster,
        tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    ) {
        let row = Arc::new(Mutex::new(row));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fixture = Cluster {
            row: row.clone(),
            calls: calls.clone(),
        };
        let (client, server) =
            crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
                let row = row.clone();
                let calls = calls.clone();
                let target = rpc
                    .metadata()
                    .get(ployz_core::ONE_TARGET_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
                    .unwrap_or_default();
                async move {
                    let body = rpc.into_inner().decode_request().unwrap().body;
                    let refusal = refusing
                        .filter(|refusing| target == machine(*refusing, "m").id.as_str())
                        .map(|refusing| NotPointerPublisher {
                            publisher: machine(if refusing == '1' { '2' } else { '1' }, "m").id,
                        });
                    #[expect(
                        clippy::wildcard_enum_match_arm,
                        reason = "fixture answers only the pointer RPCs under test"
                    )]
                    let response = match body {
                        RpcRequestBody::DescribeContract(_) => {
                            RpcResponse::from(ployz_core::ContractDescription {
                                machine_id: machine('1', "m").id,
                                protocol_major: ployz_core::PROTOCOL_MAJOR,
                                daemon_version: "fixture".into(),
                                capabilities: Default::default(),
                            })
                        }
                        RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                            enrollment: None,
                            machines: ['1', '2']
                                .into_iter()
                                .filter(|hex| Some(*hex) != down)
                                .map(|hex| {
                                    MachineObservation::new(
                                        machine(hex, "m"),
                                        MembershipObservation::Up,
                                    )
                                })
                                .collect(),
                        }),
                        RpcRequestBody::ReadRoutes(request) => {
                            let read = if request.routes_only {
                                "read routes"
                            } else {
                                "read"
                            };
                            calls.lock().unwrap().push((read, target));
                            match refusal {
                                Some(refusal) => RpcResponse::from(refusal.rpc_error()),
                                None => RpcResponse::from(RoutesRead {
                                    pointer: row.lock().unwrap().clone(),
                                    ..Default::default()
                                }),
                            }
                        }
                        RpcRequestBody::PublishServingPointer(request) => {
                            calls.lock().unwrap().push(("publish", target));
                            match refusal {
                                Some(refusal) => RpcResponse::from(refusal.rpc_error()),
                                None => {
                                    let mut row = row.lock().unwrap();
                                    RpcResponse::from(if *row == request.read {
                                        *row = Some(request.pointer);
                                        ServingPointerPublication::Published
                                    } else {
                                        ServingPointerPublication::Superseded {
                                            current: row.clone(),
                                        }
                                    })
                                }
                            }
                        }
                        other => panic!("unexpected pointer RPC: {other:?}"),
                    };
                    Ok(Response::new(response.encode().unwrap()))
                }
            })
            .await;
        (client, fixture, server)
    }

    fn at(hex: char) -> String {
        machine(hex, "m").id.to_string()
    }

    #[tokio::test]
    async fn a_superseded_publish_is_answered_once_and_never_resent() {
        let (mut client, fixture, server) =
            cluster(Some(pointer('b', &['2'], &[])), None, None).await;
        let publication = client
            .publish_serving_pointer(PublishServingPointerRequest {
                service: service(),
                read: Some(pointer('a', &['1'], &[])),
                pointer: pointer('a', &['3'], &[]),
            })
            .await
            .unwrap();
        assert_eq!(
            publication,
            ServingPointerPublication::Superseded {
                current: Some(pointer('b', &['2'], &[]))
            }
        );
        assert_eq!(*fixture.calls.lock().unwrap(), vec![("publish", at('1'))]);
        assert_eq!(
            *fixture.row.lock().unwrap(),
            Some(pointer('b', &['2'], &[]))
        );
        server.abort();
    }

    /// Only the publisher answers a pointer read, so the read asks for the pointer and
    /// not just the routes any Machine holds.
    #[tokio::test]
    async fn a_pointer_read_asks_for_the_pointer_where_it_follows() {
        let (mut client, fixture, server) =
            cluster(Some(pointer('a', &['1'], &[])), None, Some('1')).await;
        let read = client.serving_pointer_read(&service()).await.unwrap();
        assert_eq!(read.pointer, Some(pointer('a', &['1'], &[])));
        assert_eq!(
            *fixture.calls.lock().unwrap(),
            vec![("read", at('1')), ("read", at('2'))]
        );
        server.abort();
    }
}
