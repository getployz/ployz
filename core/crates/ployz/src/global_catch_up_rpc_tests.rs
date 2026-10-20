//! Real RPC adapter contracts for Global catch-up.

use super::*;
use ployz_core::{ContainerNow, EnsuredContainer, RpcResponse};

fn global_slot(key: &str) -> SlotKey {
    SlotKey {
        namespace: Namespace::parse("app").unwrap(),
        kind: ContainerKind::ServiceContainer,
        creation_key: CreationKey::try_from(key.to_owned()).unwrap(),
    }
}

fn new_container(key: &str, resolved_spec: ployz_core::ResolvedServiceSpec) -> NewContainer {
    NewContainer {
        r#gen: ployz_core::Gen::FIRST,
        slot: global_slot(key),
        resolved_spec,
        deployment_id: None,
        registry_auth: None,
        image_prepared: false,
        answer: ployz_core::AnswerWhen::Started,
    }
}

fn ensured(now: ContainerNow) -> RpcResponse {
    RpcResponse::from(EnsuredContainer::Ensured { now, health: None })
}

fn present_on(target: &Machine, spec: ployz_core::ResolvedServiceSpec, hex: char) -> RpcResponse {
    ensured(ContainerNow::Present(
        grouped(qualified("app", "api"), spec, running_on(target, hex))
            .containers
            .remove(0)
            .into_observation(),
    ))
}

#[tokio::test(start_paused = true)]
async fn real_catch_up_client_retries_readiness_and_placement_to_their_budget() {
    use ployz_core::{
        ContainerWant, InspectTelemetry, LocalMachinePhase, MachineDetails, OpaquePayload,
        RpcRequestBody, RunTarget,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;
    use tonic::{Request, Response, Status};

    for placement in [false, true] {
        for failures in [1, 4] {
            let target = machine('1', "joiner");
            let observed = target.clone();
            let calls = Arc::new(AtomicUsize::new(0));
            let attempts = calls.clone();
            let request = new_container(
                "global:test",
                requested(ServiceMode::Global)
                    .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
                    .unwrap(),
            );
            let expected = request.clone();
            let (mut client, server) =
                crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
                    let target = observed.clone();
                    let expected = expected.clone();
                    let attempts = attempts.clone();
                    async move {
                        let body = rpc.get_ref().decode_request().unwrap().body;
                        if matches!(body, RpcRequestBody::DescribeContract(_)) {
                            return Ok(Response::new(
                                RpcResponse::from(ployz_core::ContractDescription {
                                    machine_id: target.id,
                                    protocol_major: ployz_core::PROTOCOL_MAJOR,
                                    daemon_version: "fixture".into(),
                                    capabilities: Default::default(),
                                })
                                .encode()
                                .unwrap(),
                            ));
                        }
                        assert_eq!(
                            rpc.metadata().get(ployz_core::ONE_TARGET_HEADER).unwrap(),
                            target.id.as_str()
                        );
                        let inject_failure =
                            !placement || matches!(body, RpcRequestBody::EnsureContainer(_));
                        #[expect(
                            clippy::wildcard_enum_match_arm,
                            reason = "fixture accepts only the catch-up RPCs under test"
                        )]
                        let response = match body {
                            RpcRequestBody::Inspect(inspect) => {
                                if !placement {
                                    assert_eq!(inspect.telemetry, InspectTelemetry::BridgeCapacity);
                                }
                                RpcResponse::from(MachineDetails {
                                    id: target.id,
                                    phase: LocalMachinePhase::Participating,
                                    public_key: target.public_key,
                                    advertised_endpoints: Vec::new(),
                                    machine: Some(target),
                                    store_version: Default::default(),
                                    rtts: Vec::new(),
                                    management_clients: Vec::new(),
                                    telemetry: Some(
                                        ployz_core::TelemetryObservation::BridgeCapacity {
                                            bridge: BridgeEndpointCapacity::new(10, 0),
                                        },
                                    ),
                                    storage: None,
                                    upgrade: None,
                                    token: None,
                                })
                            }
                            RpcRequestBody::ListContainers(_) => {
                                RpcResponse::from(ployz_core::ContainerList {
                                    containers: Vec::new(),
                                })
                            }
                            RpcRequestBody::EnsureContainer(ensure) => {
                                assert!(placement);
                                let ContainerWant::Running {
                                    container: RunTarget::New(new),
                                    ..
                                } = ensure.want
                                else {
                                    panic!("catch-up runs a new Container: {:?}", ensure.want);
                                };
                                assert_eq!(new, expected);
                                present_on(&target, new.resolved_spec, 'a')
                            }
                            other => panic!("unexpected catch-up RPC: {other:?}"),
                        };
                        if inject_failure && attempts.fetch_add(1, Ordering::SeqCst) < failures {
                            Err(Status::unavailable("transient catch-up failure"))
                        } else {
                            Ok(Response::new(response.encode().unwrap()))
                        }
                    }
                })
                .await;
            let started = tokio::time::Instant::now();
            let succeeded = if placement {
                CatchUpClient::run_slot(
                    &mut client,
                    &target.id,
                    request,
                    &tokio_util::sync::CancellationToken::new(),
                )
                .await
                .is_ok()
            } else {
                CatchUpClient::bridge_capacity(&mut client, &target.id)
                    .await
                    .is_ok()
            };
            assert_eq!(succeeded, failures == 1);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                if failures == 1 { 2 } else { 4 }
            );
            assert_eq!(
                started.elapsed(),
                if failures == 1 {
                    Duration::from_millis(500)
                } else {
                    Duration::from_secs(6)
                }
            );
            server.abort();
        }
    }
}

#[tokio::test]
async fn catch_up_uses_primitives_and_never_replaces_a_key_conflict_or_unknown_slot() {
    use ployz_core::{
        ContainerList, ContainerLocator, ContainerWant, Drain, LocalMachinePhase, MachineDetails,
        OpaquePayload, RpcRequestBody, RunTarget,
    };
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response};

    for outcome in [
        "new",
        "matching",
        "conflict",
        "revision",
        "ineligible",
        "unknown",
        "capacity",
    ] {
        let mut target = machine('1', "joiner");
        target.accepts_services = outcome != "ineligible";
        let spec = if outcome == "unknown" {
            provisioned_global_spec()
        } else {
            requested(ServiceMode::Global)
        }
        .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
        .unwrap();
        let request = new_container(&crate::cluster::global_creation_key(&spec), spec.clone());
        let expected = request.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let observed = target.clone();
        let (mut client, server) =
            crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
                let target = observed.clone();
                let recorded = recorded.clone();
                let expected = expected.clone();
                async move {
                    let one_target = rpc.metadata().get(ployz_core::ONE_TARGET_HEADER).cloned();
                    let body = rpc.into_inner().decode_request().unwrap().body;
                    assert_eq!(one_target.unwrap(), target.id.as_str());
                    #[expect(
                        clippy::wildcard_enum_match_arm,
                        reason = "fixture rejects unrelated RPCs"
                    )]
                    let response = match body {
                        RpcRequestBody::Inspect(inspect) => {
                            recorded.lock().unwrap().push(if inspect.include_storage {
                                "inspect"
                            } else {
                                "capacity"
                            });
                            RpcResponse::from(MachineDetails {
                                id: target.id,
                                phase: LocalMachinePhase::Participating,
                                public_key: target.public_key,
                                machine: Some(target),
                                advertised_endpoints: Vec::new(),
                                store_version: Default::default(),
                                rtts: Vec::new(),
                                management_clients: Vec::new(),
                                telemetry: Some(ployz_core::TelemetryObservation::BridgeCapacity {
                                    bridge: BridgeEndpointCapacity::new(
                                        1,
                                        u64::from(outcome == "capacity"),
                                    ),
                                }),
                                storage: None,
                                upgrade: None,
                                token: None,
                            })
                        }
                        RpcRequestBody::EnsureContainer(ensure) => match ensure.want {
                            ContainerWant::Running {
                                container: RunTarget::New(new),
                                ..
                            } => {
                                recorded.lock().unwrap().push("run new");
                                assert_eq!(new, expected);
                                if outcome == "conflict" {
                                    RpcResponse::from(RpcError {
                                        code: ployz_core::RpcErrorCode::Conflict,
                                        message: "creation key conflict".into(),
                                        details: serde_json::Value::Null,
                                        cause: Vec::new(),
                                    })
                                } else {
                                    present_on(
                                        &target,
                                        new.resolved_spec,
                                        if outcome == "revision" { 'b' } else { 'a' },
                                    )
                                }
                            }
                            ContainerWant::Running {
                                container: RunTarget::Existing(ContainerLocator::Id(id)),
                                ..
                            } => {
                                recorded.lock().unwrap().push("run existing");
                                assert_eq!(id, container_id('a'));
                                present_on(&target, expected.resolved_spec, 'a')
                            }
                            ContainerWant::Gone {
                                container: ContainerLocator::Id(id),
                                drain,
                                ..
                            } => {
                                recorded.lock().unwrap().push("gone");
                                assert_eq!(id, container_id('a'));
                                assert_eq!(drain, Drain::DEFAULT);
                                ensured(ContainerNow::Absent {
                                    of: ContainerLocator::Id(id),
                                })
                            }
                            other => panic!("unexpected want: {other:?}"),
                        },
                        RpcRequestBody::ListContainers(_) => {
                            recorded.lock().unwrap().push("list");
                            if outcome == "new" {
                                return Ok(Response::new(
                                    RpcResponse::from(ContainerList {
                                        containers: Vec::new(),
                                    })
                                    .encode()
                                    .unwrap(),
                                ));
                            }
                            let mut slot = grouped(
                                qualified("app", "api"),
                                expected.resolved_spec,
                                running_on(&target, 'a'),
                            )
                            .containers
                            .remove(0)
                            .into_observation();
                            if outcome == "conflict" {
                                slot.try_update(|parts| {
                                    parts.resolved_spec.update.monitor_millis = Some(500)
                                })
                                .unwrap();
                            } else if ["revision", "capacity"].contains(&outcome) {
                                slot.try_update(|parts| {
                                    parts.resolved_spec.container.image = "api:old".into()
                                })
                                .unwrap();
                            }
                            let mut unrelated = slot.clone();
                            unrelated
                                .try_update(|parts| {
                                    parts.namespace = Namespace::parse("other").unwrap();
                                    parts.container_id = container_id('c');
                                })
                                .unwrap();
                            RpcResponse::from(ContainerList {
                                containers: vec![slot, unrelated],
                            })
                        }
                        other => panic!("unexpected RPC: {other:?}"),
                    };
                    Ok(Response::new(response.encode().unwrap()))
                }
            })
            .await;
        let ran = CatchUpClient::run_slot(
            &mut client,
            &target.id,
            request,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
        assert_eq!(
            ran.ok().map(|copy| copy.map(|copy| copy.container_id)),
            match outcome {
                "new" | "matching" => Some(Some(container_id('a'))),
                "revision" => Some(Some(container_id('b'))),
                "ineligible" => Some(None),
                _ => None,
            }
        );
        let expected: &[&str] = match outcome {
            "new" | "revision" => &["inspect", "list", "capacity", "run new"],
            "matching" => &["inspect", "list", "run existing"],
            "conflict" => &["inspect", "list", "capacity", "run new"],
            "ineligible" => &["inspect", "list", "gone"],
            "capacity" => &["inspect", "list", "capacity"],
            _ => &["inspect"],
        };
        assert_eq!(calls.lock().unwrap().as_slice(), expected);
        server.abort();
    }
}

#[tokio::test]
async fn top_level_catch_up_uses_fresh_target_eligibility_and_reuses_hidden_full_capacity_slot() {
    use ployz_core::{
        ContainerList, ContainerLocator, ContainerWant, LocalMachinePhase, MachineDetails,
        MachineList, MachineObservation, MembershipObservation, OpaquePayload, RpcRequestBody,
        RunRotation, RunTarget, TelemetryObservation,
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tonic::{Request, Response};
    for eligible in [false, true] {
        let mut stale = machine('1', "joiner");
        stale.accepts_services = false;
        let mut fresh = stale.clone();
        fresh.accepts_services = eligible;
        let founder = machine('f', "founder");
        let spec = requested(ServiceMode::Global)
            .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
            .unwrap();
        let peer = grouped(
            qualified("app", "api"),
            spec.clone(),
            running_on(&founder, 'a'),
        )
        .containers
        .remove(0)
        .into_observation();
        let local = grouped(qualified("app", "api"), spec, created_on(&fresh, 'b'))
            .containers
            .remove(0)
            .into_observation();
        let retained = Arc::new(Mutex::new(vec![local]));
        let current = retained.clone();
        let local_reads = Arc::new(AtomicUsize::new(0));
        let observed = fresh.clone();
        let (mut client, server) =
            crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
                let target = observed.clone();
                let founder = founder.clone();
                let peer = peer.clone();
                let current = current.clone();
                let reads = local_reads.clone();
                let selected = rpc
                    .metadata()
                    .get(ployz_core::ONE_TARGET_HEADER)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                async move {
                    #[expect(
                        clippy::wildcard_enum_match_arm,
                        reason = "fixture rejects unrelated operations and any duplicate creation"
                    )]
                    let response = match rpc.into_inner().decode_request().unwrap().body {
                        RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                            enrollment: None,
                            machines: vec![
                                MachineObservation::new(target.clone(), MembershipObservation::Up),
                                MachineObservation::new(founder.clone(), MembershipObservation::Up),
                            ],
                        }),
                        RpcRequestBody::ListContainers(_) => {
                            let containers = if selected.as_deref() == Some(founder.id.as_str()) {
                                vec![peer]
                            } else if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                                Vec::new()
                            } else {
                                current.lock().unwrap().clone()
                            };
                            RpcResponse::from(ContainerList { containers })
                        }
                        RpcRequestBody::Inspect(_) => RpcResponse::from(MachineDetails {
                            id: target.id,
                            public_key: target.public_key,
                            machine: Some(target),
                            phase: LocalMachinePhase::Participating,
                            advertised_endpoints: Vec::new(),
                            store_version: Default::default(),
                            rtts: Vec::new(),
                            management_clients: Vec::new(),
                            storage: None,
                            upgrade: None,
                            telemetry: Some(TelemetryObservation::BridgeCapacity {
                                bridge: BridgeEndpointCapacity::new(1, 1),
                            }),
                            token: None,
                        }),
                        RpcRequestBody::EnsureContainer(ensure) => match ensure.want {
                            ContainerWant::Running {
                                container: RunTarget::Existing(ContainerLocator::Id(id)),
                                rotation,
                            } => {
                                assert!(eligible);
                                let mut current = current.lock().unwrap();
                                let slot = current.first_mut().unwrap();
                                assert_eq!(slot.container_id, id);
                                slot.try_update(|parts| {
                                    parts.runtime = ContainerRuntimeObservation::Running {
                                        health: HealthObservation::Healthy,
                                    };
                                    if let RunRotation::In { expect, .. } = rotation {
                                        assert_eq!(expect, Some(CopyRotation::Pending));
                                        parts.rotation = Some(CopyRotation::In);
                                    }
                                })
                                .unwrap();
                                ensured(ContainerNow::Present(slot.clone()))
                            }
                            ContainerWant::Gone {
                                container: ContainerLocator::Id(id),
                                ..
                            } => {
                                assert!(!eligible);
                                assert_eq!(id, container_id('b'));
                                current.lock().unwrap().clear();
                                ensured(ContainerNow::Absent {
                                    of: ContainerLocator::Id(id),
                                })
                            }
                            other => panic!("unexpected want: {other:?}"),
                        },
                        other => {
                            panic!("unexpected RPC (existing slot needs no endpoint): {other:?}")
                        }
                    };
                    Ok(Response::new(response.encode().unwrap()))
                }
            })
            .await;
        catch_up_globals(
            &mut client,
            &stale,
            &tokio_util::sync::CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(retained.lock().unwrap().len(), usize::from(eligible));
        server.abort();
    }
}

#[tokio::test]
async fn an_ineligible_target_retires_its_own_in_copy() {
    use ployz_core::{
        ContainerList, ContainerLocator, ContainerWant, LocalMachinePhase, MachineDetails,
        MachineList, MachineObservation, MembershipObservation, OpaquePayload, RoutesRead,
        RpcRequestBody,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response};
    for eligible in [false, true] {
        let mut stale = machine('1', "joiner");
        stale.accepts_services = true;
        let mut fresh = stale.clone();
        fresh.accepts_services = eligible;
        let spec = requested(ServiceMode::Global)
            .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
            .unwrap();
        let mut old = grouped(qualified("app", "api"), spec, running_on(&fresh, 'a'))
            .containers
            .remove(0)
            .into_observation();
        old.try_update(|parts| parts.rotation = Some(CopyRotation::In))
            .unwrap();
        let local = Arc::new(Mutex::new(vec![old]));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (current, recorded, observed) = (local.clone(), calls.clone(), fresh.clone());
        let (mut client, server) =
            crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
                let (target, current, recorded) =
                    (observed.clone(), current.clone(), recorded.clone());
                async move {
                    #[expect(
                        clippy::wildcard_enum_match_arm,
                        reason = "fixture rejects every RPC that would create or run"
                    )]
                    let response = match rpc.into_inner().decode_request().unwrap().body {
                        RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                            enrollment: None,
                            machines: vec![MachineObservation::new(
                                target.clone(),
                                MembershipObservation::Up,
                            )],
                        }),
                        RpcRequestBody::ListContainers(_) => RpcResponse::from(ContainerList {
                            containers: current.lock().unwrap().clone(),
                        }),
                        RpcRequestBody::ReadRoutes(_) => {
                            recorded.lock().unwrap().push("routes");
                            RpcResponse::from(RoutesRead {
                                routes: BTreeMap::from([(target.id, Some(BTreeSet::new()))]),
                                complete: true,
                                ..Default::default()
                            })
                        }
                        RpcRequestBody::Inspect(_) => {
                            recorded.lock().unwrap().push("inspect");
                            RpcResponse::from(MachineDetails {
                                id: target.id,
                                public_key: target.public_key,
                                machine: Some(target),
                                phase: LocalMachinePhase::Participating,
                                advertised_endpoints: Vec::new(),
                                store_version: Default::default(),
                                rtts: Vec::new(),
                                management_clients: Vec::new(),
                                storage: None,
                                upgrade: None,
                                telemetry: None,
                                token: None,
                            })
                        }
                        RpcRequestBody::EnsureContainer(ensure) => match ensure.want {
                            ContainerWant::Out {
                                container: ContainerLocator::Id(id),
                                allow_empty,
                                expect,
                            } => {
                                recorded.lock().unwrap().push("out");
                                assert_eq!(id, container_id('a'));
                                assert!(!allow_empty, "Global retirement never allows empty");
                                assert_eq!(expect, Some(CopyRotation::In));
                                let mut current = current.lock().unwrap();
                                let copy = current.first_mut().unwrap();
                                copy.try_update(|parts| parts.rotation = Some(CopyRotation::Out))
                                    .unwrap();
                                ensured(ContainerNow::Present(copy.clone()))
                            }
                            ContainerWant::Gone {
                                container: ContainerLocator::Id(id),
                                ..
                            } => {
                                recorded.lock().unwrap().push("gone");
                                assert_eq!(id, container_id('a'));
                                current.lock().unwrap().clear();
                                ensured(ContainerNow::Absent {
                                    of: ContainerLocator::Id(id),
                                })
                            }
                            other => panic!("an ineligible target never runs a copy: {other:?}"),
                        },
                        other => panic!("unexpected RPC: {other:?}"),
                    };
                    Ok(Response::new(response.encode().unwrap()))
                }
            })
            .await;
        let mut excluded = Vec::new();
        let result = catch_up_globals(
            &mut client,
            &stale,
            &tokio_util::sync::CancellationToken::new(),
            |fact| {
                if let CatchUpFact::Excluded(service) = fact {
                    excluded.push(service);
                }
            },
        )
        .await;
        if eligible {
            let error = result.unwrap_err();
            assert_eq!(error.unresolved, [qualified("app", "api")]);
            assert!(excluded.is_empty());
            assert!(!calls.lock().unwrap().contains(&"gone"));
            assert_eq!(local.lock().unwrap().len(), 1, "an eligible copy stays put");
        } else {
            result.unwrap();
            assert_eq!(excluded, [qualified("app", "api")]);
            assert_eq!(
                calls.lock().unwrap().as_slice(),
                ["inspect", "out", "routes", "gone"],
                "an In copy goes Out, waits for the proxies, then goes"
            );
            assert!(local.lock().unwrap().is_empty());
        }
        server.abort();
    }
}

/// Decides a rotation want on `copy` the way its owner does: the `expect`
/// check, then the shared edge table, then the LastIn guard on In to Out.
fn owner_decides(
    copy: &mut ContainerObservation,
    target: &ployz_core::RotationTarget,
    expect: Option<&CopyRotation>,
    last_in: bool,
) -> Option<ContainerRefusal> {
    use ployz_core::{RotationEdge, RotationRefusal, RotationTarget};
    let rotation = copy.rotation().clone();
    let refused = |reason, message: String| Some(ContainerRefusal { reason, message });
    if matches!(
        (&rotation, target),
        (CopyRotation::In, RotationTarget::In { .. }) | (CopyRotation::Out, RotationTarget::Out)
    ) {
        return None;
    }
    if let Some(expect) = expect
        && *expect != rotation
    {
        return refused(
            ContainerRefusalReason::RotationMismatch,
            format!("the copy is {rotation:?}, not {expect:?} as expected"),
        );
    }
    match rotation.edge(target) {
        RotationEdge::Write(CopyRotation::Out) if rotation == CopyRotation::In && last_in => {
            refused(
                ContainerRefusalReason::LastIn,
                "no other copy is In and routed; send allow_empty to empty the Service".into(),
            )
        }
        RotationEdge::Write(to) => {
            copy.try_update(|parts| parts.rotation = Some(to)).unwrap();
            None
        }
        RotationEdge::Keep | RotationEdge::Remove => None,
        RotationEdge::Refused(refusal) => refused(
            match refusal {
                RotationRefusal::HeldByOtherRun => ContainerRefusalReason::HeldByOtherRun,
                RotationRefusal::InRotation => ContainerRefusalReason::InRotation,
                RotationRefusal::OutIsFinal | RotationRefusal::NoEdge => {
                    ContainerRefusalReason::RotationMismatch
                }
            },
            format!("{refusal:?}"),
        ),
    }
}

fn refused_with(now: ContainerObservation, refusal: ContainerRefusal) -> RpcResponse {
    RpcResponse::from(EnsuredContainer::Refused {
        now: ContainerNow::Present(now),
        refusal,
    })
}

struct OverMatching {
    result: Result<(), CatchUpError>,
    calls: Vec<&'static str>,
    local: Vec<ContainerObservation>,
    old: ContainerObservation,
    joiner: Machine,
}

/// Catches up a joiner that already holds a copy matching the founder's In
/// copy, rotated `rotation`, against an owner that decides like the real one.
async fn catch_up_over_matching_copy(rotation: CopyRotation) -> OverMatching {
    use ployz_core::{
        ContainerList, ContainerLocator, ContainerWant, LocalMachinePhase, MachineDetails,
        MachineList, MachineObservation, MembershipObservation, OpaquePayload, RotationTarget,
        RoutesRead, RpcRequestBody, RunRotation, RunTarget, TelemetryObservation,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response};
    let joiner = machine('1', "joiner");
    let founder = machine('f', "founder");
    let spec = requested(ServiceMode::Global)
        .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
        .unwrap();
    let mut peer = grouped(
        qualified("app", "api"),
        spec.clone(),
        running_on(&founder, 'a'),
    )
    .containers
    .remove(0)
    .into_observation();
    peer.try_update(|parts| parts.rotation = Some(CopyRotation::In))
        .unwrap();
    let mut old = grouped(
        qualified("app", "api"),
        spec.clone(),
        if rotation == CopyRotation::Out {
            running_on(&joiner, 'b')
        } else {
            created_on(&joiner, 'b')
        },
    )
    .containers
    .remove(0)
    .into_observation();
    old.try_update(|parts| parts.rotation = Some(rotation.clone()))
        .unwrap();
    let fresh = grouped(qualified("app", "api"), spec, running_on(&joiner, 'c'))
        .containers
        .remove(0)
        .into_observation();
    let local = Arc::new(Mutex::new(vec![old.clone()]));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (current, recorded, target) = (local.clone(), calls.clone(), joiner.clone());
    let (mut client, server) =
        crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
            let (target, founder, peer, fresh) =
                (target.clone(), founder.clone(), peer.clone(), fresh.clone());
            let (current, recorded) = (current.clone(), recorded.clone());
            let selected = rpc
                .metadata()
                .get(ployz_core::ONE_TARGET_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            async move {
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "fixture rejects unrelated operations"
                )]
                let response = match rpc.into_inner().decode_request().unwrap().body {
                    RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                        enrollment: None,
                        machines: vec![
                            MachineObservation::new(target.clone(), MembershipObservation::Up),
                            MachineObservation::new(founder.clone(), MembershipObservation::Up),
                        ],
                    }),
                    RpcRequestBody::ListContainers(_) => {
                        let containers = if selected.as_deref() == Some(founder.id.as_str()) {
                            vec![peer]
                        } else {
                            current.lock().unwrap().clone()
                        };
                        RpcResponse::from(ContainerList { containers })
                    }
                    RpcRequestBody::ReadRoutes(_) => {
                        recorded.lock().unwrap().push("routes");
                        RpcResponse::from(RoutesRead {
                            routes: BTreeMap::from([
                                (target.id, Some(BTreeSet::new())),
                                (founder.id, Some(BTreeSet::new())),
                            ]),
                            complete: true,
                            ..Default::default()
                        })
                    }
                    RpcRequestBody::Inspect(_) => RpcResponse::from(MachineDetails {
                        id: target.id,
                        public_key: target.public_key,
                        machine: Some(target),
                        phase: LocalMachinePhase::Participating,
                        advertised_endpoints: Vec::new(),
                        store_version: Default::default(),
                        rtts: Vec::new(),
                        management_clients: Vec::new(),
                        storage: None,
                        upgrade: None,
                        telemetry: Some(TelemetryObservation::BridgeCapacity {
                            bridge: BridgeEndpointCapacity::new(4, 0),
                        }),
                        token: None,
                    }),
                    RpcRequestBody::EnsureContainer(ensure) => {
                        let mut current = current.lock().unwrap();
                        match ensure.want {
                            ContainerWant::Running {
                                container: RunTarget::New(_),
                                ..
                            } => {
                                recorded.lock().unwrap().push("create");
                                // A keyed create answers with the Container
                                // already holding the key's name.
                                if current.is_empty() {
                                    current.push(fresh);
                                }
                                ensured(ContainerNow::Present(current.first().unwrap().clone()))
                            }
                            ContainerWant::Running {
                                container: RunTarget::Existing(ContainerLocator::Id(id)),
                                rotation,
                            } => {
                                let copy = current
                                    .iter_mut()
                                    .find(|copy| copy.container_id == id)
                                    .unwrap();
                                let refusal = match rotation {
                                    RunRotation::Pending => {
                                        recorded.lock().unwrap().push("start");
                                        None
                                    }
                                    RunRotation::In { run, expect } => {
                                        recorded.lock().unwrap().push("in");
                                        owner_decides(
                                            copy,
                                            &RotationTarget::In { run },
                                            expect.as_ref(),
                                            false,
                                        )
                                    }
                                };
                                match refusal {
                                    Some(refusal) => refused_with(copy.clone(), refusal),
                                    None => {
                                        copy.try_update(|parts| {
                                            parts.runtime = ContainerRuntimeObservation::Running {
                                                health: HealthObservation::Healthy,
                                            };
                                        })
                                        .unwrap();
                                        ensured(ContainerNow::Present(copy.clone()))
                                    }
                                }
                            }
                            ContainerWant::Gone {
                                container: ContainerLocator::Id(id),
                                ..
                            } => {
                                recorded.lock().unwrap().push("gone");
                                let at = current
                                    .iter()
                                    .position(|copy| copy.container_id == id)
                                    .unwrap();
                                let copy = current.get_mut(at).unwrap();
                                match owner_decides(copy, &RotationTarget::Gone, None, false) {
                                    Some(refusal) => refused_with(copy.clone(), refusal),
                                    None => {
                                        current.remove(at);
                                        ensured(ContainerNow::Absent {
                                            of: ContainerLocator::Id(id),
                                        })
                                    }
                                }
                            }
                            other => panic!("unexpected want: {other:?}"),
                        }
                    }
                    other => panic!("unexpected RPC: {other:?}"),
                };
                Ok(Response::new(response.encode().unwrap()))
            }
        })
        .await;
    let result = catch_up_globals(
        &mut client,
        &joiner,
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .await;
    let calls = calls.lock().unwrap().clone();
    let local = local.lock().unwrap().clone();
    server.abort();
    OverMatching {
        result,
        calls,
        local,
        old,
        joiner,
    }
}

#[tokio::test]
async fn a_matching_out_copy_is_retired_before_a_new_copy_comes_in() {
    let run = catch_up_over_matching_copy(CopyRotation::Out).await;
    run.result.unwrap();
    assert_eq!(
        run.calls,
        ["routes", "gone", "create", "in"],
        "the Out copy leaves the routes and goes before a new copy is made"
    );
    let local = run
        .local
        .iter()
        .map(|copy| (copy.container_id, copy.rotation().clone()))
        .collect::<Vec<_>>();
    assert_eq!(local, [(container_id('c'), CopyRotation::In)]);
}

#[tokio::test]
async fn a_matching_held_copy_is_left_as_the_user_stopped_it() {
    let held = CopyRotation::Held(ployz_core::DeploymentLogId::parse("dep-stop").unwrap());
    let run = catch_up_over_matching_copy(held).await;
    let error = run.result.unwrap_err();
    assert_eq!(error.unresolved, [qualified("app", "api")]);
    let message = message(error, &run.joiner);
    assert!(
        message.contains(&format!(
            "copy {} is stopped on this Machine by a user",
            container_id('b').as_str()
        )),
        "{message}"
    );
    assert!(
        run.calls.is_empty(),
        "a Held copy is not touched: {:?}",
        run.calls
    );
    assert_eq!(
        run.local,
        [run.old],
        "the user's stopped copy stays as it is"
    );
}

#[tokio::test]
async fn provisioned_globals_use_target_storage_and_report_unknown() {
    use ployz_core::{
        ContainerList, LocalMachinePhase, MachineDetails, OpaquePayload, RpcRequestBody,
    };
    use tonic::{Request, Response};

    for (storage, inspection_error) in [
        (Some(MachineStorageObservation::Ready), false),
        (
            Some(MachineStorageObservation::Pool {
                size_bytes: NonZeroU64::new(100).unwrap(),
                used_bytes: 0,
                free_bytes: 100,
            }),
            false,
        ),
        (Some(MachineStorageObservation::Stateless), false),
        (None, false),
        (None, true),
    ] {
        let target = machine('1', "joiner");
        let observed = target.clone();
        let request = new_container(
            "global:storage",
            provisioned_global_spec()
                .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
                .unwrap(),
        );
        let spec = request.resolved_spec.clone();
        let (mut client, server) =
            crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
                let target = observed.clone();
                let spec = spec.clone();
                async move {
                    #[expect(
                        clippy::wildcard_enum_match_arm,
                        reason = "fixture rejects unrelated RPCs"
                    )]
                    let response = match rpc.into_inner().decode_request().unwrap().body {
                        RpcRequestBody::Inspect(_) if inspection_error => {
                            RpcResponse::from(RpcError {
                                code: ployz_core::RpcErrorCode::Conflict,
                                message: "storage inspection failed".into(),
                                details: serde_json::Value::Null,
                                cause: Vec::new(),
                            })
                        }
                        RpcRequestBody::Inspect(_) => RpcResponse::from(MachineDetails {
                            id: target.id,
                            phase: LocalMachinePhase::Participating,
                            public_key: target.public_key,
                            machine: Some(target),
                            advertised_endpoints: Vec::new(),
                            store_version: Default::default(),
                            rtts: Vec::new(),
                            management_clients: Vec::new(),
                            storage,
                            upgrade: None,
                            telemetry: Some(ployz_core::TelemetryObservation::BridgeCapacity {
                                bridge: BridgeEndpointCapacity::new(10, 0),
                            }),
                            token: None,
                        }),
                        RpcRequestBody::ListContainers(_) => RpcResponse::from(ContainerList {
                            containers: Vec::new(),
                        }),
                        RpcRequestBody::EnsureContainer(_) => {
                            assert!(matches!(
                                storage,
                                Some(
                                    MachineStorageObservation::Ready
                                        | MachineStorageObservation::Pool { .. }
                                )
                            ));
                            present_on(&target, spec, 'a')
                        }
                        other => panic!("unexpected storage eligibility RPC: {other:?}"),
                    };
                    Ok(Response::new(response.encode().unwrap()))
                }
            })
            .await;
        let result = CatchUpClient::run_slot(
            &mut client,
            &target.id,
            request,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
        if inspection_error {
            assert!(
                result
                    .unwrap_err()
                    .message
                    .contains("storage inspection failed")
            );
            server.abort();
            continue;
        }
        match storage {
            Some(MachineStorageObservation::Ready | MachineStorageObservation::Pool { .. }) => {
                assert!(result.unwrap().is_some())
            }
            Some(MachineStorageObservation::Stateless) => assert!(result.unwrap().is_none()),
            None => assert!(
                result
                    .unwrap_err()
                    .message
                    .contains("eligibility on the Server is unknown")
            ),
        }
        server.abort();
    }
}
