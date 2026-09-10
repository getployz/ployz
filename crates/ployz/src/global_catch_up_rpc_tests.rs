//! Real RPC adapter contracts for Global catch-up.

use super::*;

#[tokio::test(start_paused = true)]
async fn real_catch_up_client_retries_readiness_and_placement_to_their_budget() {
    use ployz_core::{
        ContainerCreated, InspectTelemetry, LocalMachinePhase, MachineDetails, OpaquePayload,
        RpcRequestBody, RpcResponse,
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
            let request = CreateContainerRequest {
                creation_key: Some("global:test".into()),
                kind: ContainerKind::ServiceContainer,
                project_name: ProjectName::parse("app").unwrap(),
                resolved_spec: requested(ServiceMode::Global)
                    .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
                    .unwrap(),
            };
            let expected = request.clone();
            let (mut client, server) =
                crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
                    let target = observed.clone();
                    let expected = expected.clone();
                    let attempts = attempts.clone();
                    async move {
                        assert_eq!(
                            rpc.metadata().get(ployz_core::ONE_TARGET_HEADER).unwrap(),
                            target.id.as_str()
                        );
                        let body = rpc.into_inner().decode_request().unwrap().body;
                        let inject_failure =
                            !placement || matches!(body, RpcRequestBody::CreateContainer(_));
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
                                    cloud_paired: false,
                                    telemetry: Some(
                                        ployz_core::TelemetryObservation::BridgeCapacity {
                                            bridge: BridgeEndpointCapacity::new(10, 0),
                                        },
                                    ),
                                    storage: None,
                                })
                            }
                            RpcRequestBody::ListContainers(_) => {
                                RpcResponse::from(ployz_core::ContainerList {
                                    containers: Vec::new(),
                                })
                            }
                            RpcRequestBody::CreateContainer(ensure) => {
                                assert!(placement);
                                assert_eq!(ensure, expected);
                                RpcResponse::from(ContainerCreated {
                                    container_id: container_id('a'),
                                    display_name: "api".into(),
                                })
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
                CatchUpClient::create_slot(&mut client, &target.id, request)
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
        ContainerChanged, ContainerList, LocalMachinePhase, MachineDetails, OpaquePayload,
        RpcRequestBody, RpcResponse,
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
        let request = CreateContainerRequest {
            creation_key: Some(crate::cluster::global_creation_key(&spec)),
            kind: ContainerKind::ServiceContainer,
            project_name: ProjectName::parse("app").unwrap(),
            resolved_spec: spec.clone(),
        };
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
                    assert_eq!(
                        rpc.metadata().get(ployz_core::ONE_TARGET_HEADER).unwrap(),
                        target.id.as_str()
                    );
                    #[expect(
                        clippy::wildcard_enum_match_arm,
                        reason = "fixture rejects unrelated RPCs"
                    )]
                    let response = match rpc.into_inner().decode_request().unwrap().body {
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
                                cloud_paired: false,
                                telemetry: Some(ployz_core::TelemetryObservation::BridgeCapacity {
                                    bridge: BridgeEndpointCapacity::new(
                                        1,
                                        u64::from(outcome == "capacity"),
                                    ),
                                }),
                                storage: None,
                            })
                        }
                        RpcRequestBody::CreateContainer(create) => {
                            recorded.lock().unwrap().push("create");
                            assert_eq!(create, expected);
                            if outcome == "conflict" {
                                RpcResponse::from(RpcError {
                                    code: ployz_core::RpcErrorCode::Conflict,
                                    message: "creation key conflict".into(),
                                    details: serde_json::Value::Null,
                                })
                            } else {
                                RpcResponse::from(ployz_core::ContainerCreated {
                                    container_id: container_id(if outcome == "revision" {
                                        'b'
                                    } else {
                                        'a'
                                    }),
                                    display_name: "existing".into(),
                                })
                            }
                        }
                        RpcRequestBody::StartContainer(start) => {
                            recorded.lock().unwrap().push("start");
                            assert_eq!(
                                start.container_id,
                                container_id(if outcome == "revision" { 'b' } else { 'a' })
                            );
                            RpcResponse::from(ContainerChanged {
                                container_id: start.container_id,
                            })
                        }
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
                                    parts.project_name = ProjectName::parse("other").unwrap();
                                    parts.container_id = container_id('c');
                                })
                                .unwrap();
                            RpcResponse::from(ContainerList {
                                containers: vec![slot, unrelated],
                            })
                        }
                        RpcRequestBody::StopContainer(stop) => {
                            recorded.lock().unwrap().push("stop");
                            assert_eq!(stop.container_id, container_id('a'));
                            RpcResponse::from(ContainerChanged {
                                container_id: stop.container_id,
                            })
                        }
                        RpcRequestBody::RemoveContainer(remove) => {
                            recorded.lock().unwrap().push("remove");
                            assert_eq!(remove.container_id, container_id('a'));
                            assert!(!remove.remove_volumes);
                            RpcResponse::from(ContainerChanged {
                                container_id: remove.container_id,
                            })
                        }
                        other => panic!("unexpected RPC: {other:?}"),
                    };
                    Ok(Response::new(response.encode().unwrap()))
                }
            })
            .await;
        let created = CatchUpClient::create_slot(&mut client, &target.id, request).await;
        if let Ok(Some(created)) = &created {
            CatchUpClient::start_slot(&mut client, &target.id, created.container_id)
                .await
                .unwrap();
        }
        assert_eq!(
            created.is_ok(),
            ["matching", "new", "revision", "ineligible"].contains(&outcome)
        );
        let expected: &[&str] = match outcome {
            "new" | "revision" => &["inspect", "list", "capacity", "create", "start"],
            "matching" => &["inspect", "list", "start"],
            "conflict" => &["inspect", "list", "capacity", "create"],
            "ineligible" => &["inspect", "list", "stop", "remove"],
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
        ContainerChanged, ContainerList, LocalMachinePhase, MachineDetails, MachineList,
        MachineObservation, MembershipObservation, OpaquePayload, RpcRequestBody, RpcResponse,
        TelemetryObservation,
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
                            cloud_paired: false,
                            storage: None,
                            telemetry: Some(TelemetryObservation::BridgeCapacity {
                                bridge: BridgeEndpointCapacity::new(1, 1),
                            }),
                        }),
                        RpcRequestBody::StartContainer(start) => {
                            assert!(eligible);
                            let mut current = current.lock().unwrap();
                            let slot = current.first_mut().unwrap();
                            assert_eq!(slot.container_id, start.container_id);
                            slot.try_update(|parts| {
                                parts.runtime = ContainerRuntimeObservation::Running {
                                    health: HealthObservation::Healthy,
                                }
                            })
                            .unwrap();
                            RpcResponse::from(ContainerChanged {
                                container_id: start.container_id,
                            })
                        }
                        RpcRequestBody::StopContainer(stop) => {
                            assert!(!eligible);
                            RpcResponse::from(ContainerChanged {
                                container_id: stop.container_id,
                            })
                        }
                        RpcRequestBody::RemoveContainer(remove) => {
                            assert!(!eligible);
                            assert_eq!(remove.container_id, container_id('b'));
                            current.lock().unwrap().clear();
                            RpcResponse::from(ContainerChanged {
                                container_id: remove.container_id,
                            })
                        }
                        other => {
                            panic!("unexpected RPC (existing slot needs no endpoint): {other:?}")
                        }
                    };
                    Ok(Response::new(response.encode().unwrap()))
                }
            })
            .await;
        catch_up_globals(&mut client, &stale).await.unwrap();
        assert_eq!(retained.lock().unwrap().len(), usize::from(eligible));
        server.abort();
    }
}

#[tokio::test]
async fn provisioned_globals_use_target_storage_and_report_unknown() {
    use ployz_core::{
        ContainerList, LocalMachinePhase, MachineDetails, OpaquePayload, RpcRequestBody,
        RpcResponse,
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
        let request = CreateContainerRequest {
            creation_key: Some("global:storage".into()),
            kind: ContainerKind::ServiceContainer,
            project_name: ProjectName::parse("app").unwrap(),
            resolved_spec: provisioned_global_spec()
                .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
                .unwrap(),
        };
        let (mut client, server) =
            crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
                let target = observed.clone();
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
                            cloud_paired: false,
                            storage,
                            telemetry: Some(ployz_core::TelemetryObservation::BridgeCapacity {
                                bridge: BridgeEndpointCapacity::new(10, 0),
                            }),
                        }),
                        RpcRequestBody::ListContainers(_) => RpcResponse::from(ContainerList {
                            containers: Vec::new(),
                        }),
                        RpcRequestBody::CreateContainer(_) => {
                            assert!(matches!(
                                storage,
                                Some(
                                    MachineStorageObservation::Ready
                                        | MachineStorageObservation::Pool { .. }
                                )
                            ));
                            RpcResponse::from(created())
                        }
                        other => panic!("unexpected storage eligibility RPC: {other:?}"),
                    };
                    Ok(Response::new(response.encode().unwrap()))
                }
            })
            .await;
        let result = CatchUpClient::create_slot(&mut client, &target.id, request).await;
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
                    .contains("eligibility is Unknown")
            ),
        }
        server.abort();
    }
}
