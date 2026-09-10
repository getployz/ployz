use super::*;

#[tokio::test]
async fn dispatches_the_complete_algebra() {
    let first = machine('1');
    let second = machine('2');
    let old = container('a');
    let removed = container('b');
    let hook = container('c');
    let new_run = container('d');
    let replacement = container('e');
    let new_hook = container('f');
    let bind_conflict = container('8');
    let nested = container('9');
    let service = spec(None, None, None);
    let hook_spec = spec(None, None, Some(5_000));
    let operations = vec![
        DeployOperation::RunContainer {
            machine_id: first,
            spec: service.clone(),
            skip_health_monitor: true,
        },
        DeployOperation::StopContainer {
            machine_id: first,
            container_id: old,
            purpose: ployz_core::StopContainerPurpose::Lifecycle,
        },
        DeployOperation::StopContainer {
            machine_id: first,
            container_id: bind_conflict,
            purpose: ployz_core::StopContainerPurpose::FreeHostPorts,
        },
        DeployOperation::RemoveContainer {
            machine_id: first,
            container_id: removed,
        },
        DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id: first,
            old_container_id: old,
            spec: service.clone(),
            skip_health_monitor: true,
        }),
        DeployOperation::StopHook {
            machine_id: second,
            container_id: hook,
        },
        DeployOperation::RunHook {
            machine_id: first,
            spec: hook_spec,
            old_hook_containers: vec![(second, hook)],
        },
        DeployOperation::RunContainer {
            machine_id: second,
            spec: service,
            skip_health_monitor: true,
        },
        DeployOperation::RemoveVolume {
            id: DockerVolumeId {
                machine_id: first,
                name: DockerVolumeName::parse("data").unwrap(),
            },
        },
    ];
    let plan = operations.clone();
    let client = Scripted::new(vec![
        created(
            Call::Create(first, ContainerKind::ServiceContainer),
            &new_run,
        ),
        ok(Call::Start(first, new_run)),
        ok(Call::Wait(
            vec![new_run],
            ContainerObservationCondition::Serving,
        )),
        ok(Call::Stop(first, old)),
        ok(Call::Wait(
            vec![old],
            ContainerObservationCondition::Dropped,
        )),
        ok(Call::Stop(first, bind_conflict)),
        ok(Call::Stop(first, removed)),
        ok(Call::Remove(first, removed)),
        ok(Call::Wait(
            vec![removed],
            ContainerObservationCondition::Dropped,
        )),
        created(
            Call::Create(first, ContainerKind::ServiceContainer),
            &replacement,
        ),
        ok(Call::Start(first, replacement)),
        ok(Call::Wait(
            vec![replacement],
            ContainerObservationCondition::Serving,
        )),
        ok(Call::Stop(first, old)),
        ok(Call::Remove(first, old)),
        dropped(old),
        ok(Call::Stop(second, hook)),
        ok(Call::Remove(second, hook)),
        created(Call::Create(first, ContainerKind::PreDeployHook), &new_hook),
        ok(Call::Start(first, new_hook)),
        observed(Call::Inspect(first, new_hook), exited(0)),
        created(
            Call::Create(second, ContainerKind::ServiceContainer),
            &nested,
        ),
        ok(Call::Start(second, nested)),
        ok(Call::Wait(
            vec![nested],
            ContainerObservationCondition::Serving,
        )),
        ok(Call::RemoveVolume(DockerVolumeId {
            machine_id: first,
            name: DockerVolumeName::parse("data").unwrap(),
        })),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        DeployOutcome::Success {
            completed: operations,
        }
    );
    client.assert_done();
}

#[tokio::test]
async fn volume_ensure_failure_is_the_container_operation_failure() {
    let machine_id = machine('1');
    let operation = run(&machine_id, spec(None, None, None), true);
    let client = Scripted::new(vec![failed(
        Call::Create(machine_id, ContainerKind::ServiceContainer),
        "Volume Ensure failed after creating data",
    )]);

    let outcome = execute_with(
        std::slice::from_ref(&operation),
        &client,
        &CancellationToken::new(),
    )
    .await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            completed,
            failed: FailedOperation::Operation {
                operation: failed,
                error: ExecutionError::Machine {
                    action: MachineAction::CreateContainer,
                    error,
                },
            },
            unexecuted,
        } if completed.is_empty()
            && failed == operation
            && error.message == "Volume Ensure failed after creating data"
            && unexecuted.is_empty()
    ));
    client.assert_done();
}

#[tokio::test]
async fn a_failure_at_each_position_keeps_the_exact_prefix_and_suffix() {
    let machine = machine('1');
    let operations = ['a', 'b', 'c']
        .map(|id| DeployOperation::StopContainer {
            machine_id: machine,
            container_id: container(id),
            purpose: ployz_core::StopContainerPurpose::Lifecycle,
        })
        .to_vec();
    let plan = operations.clone();

    for failed_index in 0..operations.len() {
        let steps = operations
            .iter()
            .take(failed_index + 1)
            .enumerate()
            .flat_map(|(index, operation)| {
                let DeployOperation::StopContainer { container_id, .. } = operation else {
                    unreachable!()
                };
                if index == failed_index {
                    vec![failed(Call::Stop(machine, *container_id), "boom")]
                } else {
                    vec![
                        ok(Call::Stop(machine, *container_id)),
                        dropped(*container_id),
                    ]
                }
            })
            .collect();
        let client = Scripted::new(steps);
        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        let DeployOutcome::Failed {
            completed,
            failed,
            unexecuted,
        } = &outcome
        else {
            panic!("expected failure at {failed_index}");
        };
        assert_eq!(completed, operations.get(..failed_index).unwrap());
        assert!(matches!(
            failed,
            FailedOperation::Operation {
                operation: DeployOperation::StopContainer { .. },
                error: ExecutionError::Machine { error, .. },
            } if error.message == "boom"
        ));
        assert_eq!(unexecuted, operations.get(failed_index + 1..).unwrap());
        client.assert_done();
    }
}

#[tokio::test]
async fn create_then_start_failure_removes_the_candidate_and_keeps_the_start_error() {
    for cleanup in [
        ok(Call::Remove(machine('1'), container('a'))),
        failed(Call::Remove(machine('1'), container('a')), "cleanup failed"),
    ] {
        let machine = machine('1');
        let created_id = container('a');
        let plan = vec![run(&machine, spec(None, None, None), false)];
        let client = Scripted::new(vec![
            created(
                Call::Create(machine, ContainerKind::ServiceContainer),
                &created_id,
            ),
            failed(Call::Start(machine, created_id), "start failed"),
            cleanup,
        ]);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::StartContainer,
                        error,
                    },
                    ..
                },
                ..
            } if error.message == "start failed"
        ));
        client.assert_done();
    }
}

#[tokio::test]
async fn standalone_stop_and_remove_tolerate_missing_targets() {
    let machine = machine('1');
    let stopped = container('9');
    let removed = container('a');
    let suffix = container('b');
    let volume = DockerVolumeId {
        machine_id: machine,
        name: DockerVolumeName::parse("data").unwrap(),
    };
    let mut missing = error("not found");
    missing.code = RpcErrorCode::NotFound;
    let plan = vec![
        DeployOperation::StopContainer {
            machine_id: machine,
            container_id: stopped,
            purpose: ployz_core::StopContainerPurpose::Lifecycle,
        },
        DeployOperation::RemoveContainer {
            machine_id: machine,
            container_id: removed,
        },
        DeployOperation::RemoveVolume { id: volume.clone() },
        stop(&machine, &suffix),
    ];
    let client = Scripted::new(vec![
        Step(Call::Stop(machine, stopped), Reply::Error(missing.clone())),
        dropped(stopped),
        Step(Call::Stop(machine, removed), Reply::Error(missing.clone())),
        Step(
            Call::Remove(machine, removed),
            Reply::Error(missing.clone()),
        ),
        dropped(removed),
        Step(Call::RemoveVolume(volume), Reply::Error(missing)),
        ok(Call::Stop(machine, suffix)),
        dropped(suffix),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

#[tokio::test]
async fn global_start_failure_retains_the_keyed_container_for_retry() {
    let machine = machine('1');
    let id = container('a');
    let mut service = spec(None, None, None);
    service.mode = ployz_core::ServiceMode::Global;
    let client = Scripted::new(vec![
        created(Call::Create(machine, ContainerKind::ServiceContainer), &id),
        failed(Call::Start(machine, id), "start failed"),
    ]);
    let outcome = execute_with(
        &[run(&machine, service, false)],
        &client,
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, DeployOutcome::Failed { .. }));
    client.assert_done();
}

#[tokio::test]
async fn global_deploy_uses_stable_revision_keys_without_keying_hooks_or_replicas() {
    use ployz_core::{
        CreateContainerRequest, OpaquePayload, RpcRequestBody, RpcResponse, ServiceMode,
    };
    use std::sync::Arc;
    use tonic::{Request, Response};

    let captured = Arc::new(Mutex::new(Vec::<CreateContainerRequest>::new()));
    let requests = captured.clone();
    let (client, server) =
        crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
            let requests = requests.clone();
            async move {
                let body = rpc.into_inner().decode_request().unwrap().body;
                if matches!(body, RpcRequestBody::ListContainers(_)) {
                    return Ok(Response::new(
                        RpcResponse::from(ployz_core::ContainerList {
                            containers: Vec::new(),
                        })
                        .encode()
                        .unwrap(),
                    ));
                }
                let RpcRequestBody::CreateContainer(request) = body else {
                    panic!("only create is expected");
                };
                requests.lock().unwrap().push(request);
                Ok(Response::new(
                    RpcResponse::from(ContainerCreated {
                        container_id: container('a'),
                        display_name: "api".into(),
                    })
                    .encode()
                    .unwrap(),
                ))
            }
        })
        .await;
    let mut service = spec(None, None, None);
    service.container.pull_policy = ployz_core::PullPolicy::Always;
    service.mode = ServiceMode::Global;
    let mut revision = service.clone();
    revision.container.image = "alpine:new".into();
    let mut other_inputs = service.clone();
    other_inputs.update.monitor_millis = Some(500);
    for specification in [&service, &service, &revision, &other_inputs] {
        MachineOperations::create_container(
            &client,
            &machine('1'),
            ContainerKind::ServiceContainer,
            &test_project(),
            specification,
            None,
        )
        .await
        .unwrap();
    }
    MachineOperations::create_container(
        &client,
        &machine('1'),
        ContainerKind::PreDeployHook,
        &test_project(),
        &service,
        None,
    )
    .await
    .unwrap();
    MachineOperations::create_container(
        &client,
        &machine('1'),
        ContainerKind::ServiceContainer,
        &test_project(),
        &service,
        Some(container('f')),
    )
    .await
    .unwrap();
    MachineOperations::create_container(
        &client,
        &machine('1'),
        ContainerKind::ServiceContainer,
        &test_project(),
        &service,
        Some(container('f')),
    )
    .await
    .unwrap();
    service.mode = ServiceMode::Replicated {
        replicas: std::num::NonZeroU32::new(1).unwrap(),
    };
    MachineOperations::create_container(
        &client,
        &machine('1'),
        ContainerKind::ServiceContainer,
        &test_project(),
        &service,
        None,
    )
    .await
    .unwrap();
    let requests = captured.lock().unwrap();
    let keys: Vec<_> = requests
        .iter()
        .map(|request| request.creation_key.as_ref())
        .collect();
    let [
        first,
        retry,
        revision,
        changed,
        hook,
        replacement,
        replacement_retry,
        replica,
    ] = keys.as_slice()
    else {
        panic!("expected eight create requests");
    };
    assert!(first.is_some());
    assert_eq!(first, retry);
    assert_ne!(first, revision, "explicit revisions can coexist");
    assert_eq!(
        first, changed,
        "changed non-revision inputs reach keyed conflict validation"
    );
    assert_eq!(*hook, None);
    assert_ne!(
        first, replacement,
        "forced replacement can overlap an identical spec"
    );
    assert_eq!(
        replacement, replacement_retry,
        "retry the same explicit replacement"
    );
    assert_eq!(*replica, None);
    server.abort();
}

#[tokio::test]
async fn global_replacement_scopes_creation_to_the_old_container_and_retires_it_explicitly() {
    let machine = machine('1');
    let old = container('a');
    let new = container('b');
    let mut service = spec(None, None, None);
    service.mode = ployz_core::ServiceMode::Global;
    let client = Scripted::new(vec![
        created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Start(machine, new)),
        ok(Call::Wait(
            vec![new],
            ContainerObservationCondition::Serving,
        )),
        ok(Call::Stop(machine, old)),
        ok(Call::Remove(machine, old)),
        ok(Call::Wait(
            vec![old],
            ContainerObservationCondition::Dropped,
        )),
    ]);
    let operation = DeployOperation::ReplaceContainer(ReplacementOperation {
        machine_id: machine,
        old_container_id: old,
        spec: service,
        skip_health_monitor: true,
    });
    assert!(matches!(
        execute_with(&[operation], &client, &CancellationToken::new()).await,
        DeployOutcome::Success { .. }
    ));
    assert_eq!(*client.replacing.lock().unwrap(), [Some(old)]);
    client.assert_done();
}

#[tokio::test]
async fn global_stop_first_retry_replays_retained_candidate_with_no_free_endpoints() {
    use crate::deploy::{DeployIntent, DeploySnapshot, IngressContext, PlanOptions, plan_deploy};
    use ployz_core::{
        BridgeEndpointCapacity, ContainerChanged, ContainerDetails, ContainerList, Machine,
        MachineName, MachineObservation, MembershipObservation, OpaquePayload,
        RequestedServiceSpec, RpcRequestBody, RpcResponse, ServiceMode, WireGuardPublicKey,
    };
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    use tonic::{Request, Response};

    let target = Machine {
        id: machine('1'),
        name: MachineName::parse("one").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        public_key: WireGuardPublicKey([1; 32]),
        public_ip: None,
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
    };
    let mut desired = spec(None, None, None);
    desired.mode = ServiceMode::Global;
    desired.update.order = UpdateOrder::StopFirst;
    desired.container.pull_policy = ployz_core::PullPolicy::Always;
    let requested: RequestedServiceSpec =
        serde_json::from_value(serde_json::to_value(&desired).unwrap()).unwrap();
    let old_id = container('a');
    let new_id = container('b');
    let mut old = observation(
        &target.id,
        &old_id,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
    );
    old.try_update(|parts| {
        parts.resolved_spec = desired.clone();
        parts.resolved_spec.container.image = "alpine:old".into();
    })
    .unwrap();
    let state = Arc::new(Mutex::new(vec![old]));
    let remote = state.clone();
    let keys = Arc::new(Mutex::new(Vec::new()));
    let captured = keys.clone();
    let first_start = Arc::new(AtomicBool::new(true));
    let (client, server) =
        crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
            let remote = remote.clone();
            let captured = captured.clone();
            let first_start = first_start.clone();
            async move {
                let mut remote = remote.lock().unwrap();
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "fixture exercises only replacement failure and retry primitives"
                )]
                let response = match rpc.into_inner().decode_request().unwrap().body {
                    RpcRequestBody::InspectContainer(inspect) => {
                        RpcResponse::from(ContainerDetails {
                            container: remote
                                .iter()
                                .find(|c| c.container_id == inspect.container_id)
                                .unwrap()
                                .clone(),
                            environment: None,
                        })
                    }
                    RpcRequestBody::ListContainers(_) => RpcResponse::from(ContainerList {
                        containers: remote.clone(),
                    }),
                    RpcRequestBody::StopContainer(stop) => {
                        remote
                            .iter_mut()
                            .find(|c| c.container_id == stop.container_id)
                            .unwrap()
                            .try_update(|parts| {
                                parts.runtime = ContainerRuntimeObservation::Exited { code: 0 }
                            })
                            .unwrap();
                        RpcResponse::from(ContainerChanged {
                            container_id: stop.container_id,
                        })
                    }
                    RpcRequestBody::CreateContainer(create) => {
                        let key = create.creation_key.unwrap();
                        captured.lock().unwrap().push(key.clone());
                        if let Some(existing) = remote
                            .iter()
                            .find(|c| c.labels.get("ployz.creation.key") == Some(&key))
                        {
                            assert_eq!(existing.resolved_spec, create.resolved_spec);
                            RpcResponse::from(ContainerCreated {
                                container_id: existing.container_id,
                                display_name: "retained".into(),
                            })
                        } else {
                            let mut candidate = observation(
                                &machine('1'),
                                &new_id,
                                ContainerRuntimeObservation::Created,
                            );
                            candidate
                                .try_update(|parts| {
                                    parts.resolved_spec = create.resolved_spec;
                                    parts.created_at_unix_nanos = 1;
                                    parts.labels.insert("ployz.creation.key".into(), key);
                                })
                                .unwrap();
                            remote.push(candidate);
                            RpcResponse::from(ContainerCreated {
                                container_id: new_id,
                                display_name: "candidate".into(),
                            })
                        }
                    }
                    RpcRequestBody::StartContainer(start) => {
                        if first_start.swap(false, Ordering::SeqCst) {
                            RpcResponse::from(error("start failed"))
                        } else {
                            remote
                                .iter_mut()
                                .find(|c| c.container_id == start.container_id)
                                .unwrap()
                                .try_update(|parts| {
                                    parts.runtime = ContainerRuntimeObservation::Running {
                                        health: HealthObservation::Healthy,
                                    }
                                })
                                .unwrap();
                            RpcResponse::from(ContainerChanged {
                                container_id: start.container_id,
                            })
                        }
                    }
                    other => panic!("unexpected mutation: {other:?}"),
                };
                Ok(Response::new(response.encode().unwrap()))
            }
        })
        .await;
    let snapshot = |used| DeploySnapshot {
        machines: vec![MachineObservation::new(
            target.clone(),
            MembershipObservation::Up,
        )],
        containers: state.lock().unwrap().clone(),
        capacity: Some(BTreeMap::from([(
            target.id,
            BridgeEndpointCapacity::new(2, used),
        )])),
        ..Default::default()
    };
    let intent = DeployIntent::apply_one(
        test_project(),
        requested,
        PlanOptions {
            skip_health_monitor: true,
            ..Default::default()
        },
    );
    let first = plan_deploy(&intent, &snapshot(1), IngressContext::default()).unwrap();
    assert!(matches!(
        execute_operation_sequence(&first, &client, &CancellationToken::new(), None).await,
        DeployOutcome::Failed { .. }
    ));
    assert_eq!(
        state.lock().unwrap().len(),
        2,
        "failed start retains v2 beside stopped v1"
    );
    let retry = plan_deploy(&intent, &snapshot(2), IngressContext::default())
        .expect("retained candidate needs no new endpoint");
    let DeployOperation::RunContainer {
        machine_id, spec, ..
    } = retry.operations().first().unwrap()
    else {
        panic!("inactive replacement replans as a run");
    };
    let replay = MachineOperations::create_container(
        &client,
        machine_id,
        ContainerKind::ServiceContainer,
        &test_project(),
        spec,
        None,
    )
    .await
    .unwrap();
    assert_eq!(replay.container_id, new_id);
    MachineOperations::start_container(&client, machine_id, &replay.container_id)
        .await
        .unwrap();
    let captured = keys.lock().unwrap();
    let [first_key, retry_key] = captured.as_slice() else {
        panic!("expected create and replay");
    };
    assert_eq!(first_key, retry_key);
    assert_eq!(
        state.lock().unwrap().len(),
        2,
        "replay must not allocate a second v2"
    );
    server.abort();
}
