use super::*;
use ployz_core::OperationStatus;
use ployz_core::RunRotation;
use ployz_core::labels::CREATION_KEY;

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
            purpose: ployz_core::RemoveContainerPurpose::Rollout,
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
        created(Call::Run(first, ContainerKind::ServiceContainer), &new_run),
        observed(Call::Inspect(first, old), running()),
        ok(Call::Stop(first, old)),
        observed(Call::Inspect(first, bind_conflict), running()),
        ok(Call::Stop(first, bind_conflict)),
        created(
            Call::Run(first, ContainerKind::ServiceContainer),
            &replacement,
        ),
        ok(Call::StopNow(second, hook)),
        ok(Call::Discard(second, hook)),
        created(Call::Run(first, ContainerKind::PreDeployHook), &new_hook),
        observed(Call::Inspect(first, new_hook), exited(0)),
        created(Call::Run(second, ContainerKind::ServiceContainer), &nested),
        ok(Call::RemoveVolume(DockerVolumeId {
            machine_id: first,
            name: DockerVolumeName::parse("data").unwrap(),
        })),
        observed(Call::Inspect(first, old), running()),
        ok(Call::Remove(first, old)),
        observed(Call::Inspect(first, removed), running()),
        ok(Call::Remove(first, removed)),
    ]);
    let client = client.with_copy(&first, &old, CopyRotation::In).with_copy(
        &first,
        &removed,
        CopyRotation::In,
    );

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        DeployOutcome::Success {
            completed: operations,
            services: ended(&[("api", ServiceResult::New)]),
        }
    );
    for copy in [new_run, replacement, nested] {
        assert_eq!(client.rotation(&copy), Some(CopyRotation::In));
    }
    for copy in [old, removed] {
        assert_eq!(client.rotation(&copy), None, "the old copies are removed");
    }
    client.assert_done();
}

#[tokio::test]
async fn volume_ensure_failure_is_the_container_operation_failure() {
    let machine_id = machine('1');
    let operation = run(&machine_id, spec(None, None, None), true);
    let client = Scripted::new(vec![
        failed(
            Call::Run(machine_id, ContainerKind::ServiceContainer),
            "Volume Ensure failed after creating data",
        ),
        ok(Call::DiscardSlot(
            machine_id,
            ContainerKind::ServiceContainer,
        )),
    ]);

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
            ..
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
                let inspect = observed(Call::Inspect(machine, *container_id), running());
                if index == failed_index {
                    vec![inspect, failed(Call::Stop(machine, *container_id), "boom")]
                } else {
                    vec![inspect, ok(Call::Stop(machine, *container_id))]
                }
            })
            .collect();
        let client = Scripted::new(steps);
        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        let DeployOutcome::Failed {
            completed,
            failed,
            unexecuted,
            ..
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
async fn a_failed_run_removes_its_slot_and_keeps_the_run_error() {
    for cleanup in [
        ok(Call::DiscardSlot(
            machine('1'),
            ContainerKind::ServiceContainer,
        )),
        failed(
            Call::DiscardSlot(machine('1'), ContainerKind::ServiceContainer),
            "cleanup failed",
        ),
    ] {
        let machine = machine('1');
        let plan = vec![run(&machine, spec(None, None, None), false)];
        let client = Scripted::new(vec![
            failed(
                Call::Run(machine, ContainerKind::ServiceContainer),
                "start failed",
            ),
            cleanup,
        ]);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::CreateContainer,
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
async fn a_slot_held_by_another_spec_is_not_removed() {
    let machine = machine('1');
    let mut occupied = error("slot holds another spec");
    occupied.code = RpcErrorCode::Conflict;
    let client = Scripted::new(vec![Step(
        Call::Run(machine, ContainerKind::ServiceContainer),
        Reply::Error(occupied),
    )]);
    let outcome = execute_with(
        &[run(&machine, spec(None, None, None), false)],
        &client,
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(outcome, DeployOutcome::Failed { .. }));
    client.assert_done();
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
            purpose: ployz_core::RemoveContainerPurpose::Rollout,
        },
        DeployOperation::RemoveVolume { id: volume.clone() },
        stop(&machine, &suffix),
    ];
    let client = Scripted::new(vec![
        Step(
            Call::Inspect(machine, stopped),
            Reply::Error(missing.clone()),
        ),
        observed(Call::Inspect(machine, removed), running()),
        ok(Call::Remove(machine, removed)),
        Step(Call::RemoveVolume(volume), Reply::Error(missing)),
        observed(Call::Inspect(machine, suffix), running()),
        ok(Call::Stop(machine, suffix)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

#[tokio::test]
async fn global_start_failure_leaves_nothing_of_the_keyed_container() {
    let machine = machine('1');
    let mut service = spec(None, None, None);
    service.mode = ployz_core::ServiceMode::Global;
    let client = Scripted::new(vec![
        failed(
            Call::Run(machine, ContainerKind::ServiceContainer),
            "start failed",
        ),
        ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
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
async fn a_private_image_is_pulled_with_its_services_credentials_only() {
    use ployz_core::{OpaquePayload, PullImageRequest, RegistryAuth, RpcRequestBody};
    use std::sync::Arc;
    use tonic::{Request, Response};

    let captured = Arc::new(Mutex::new(Vec::<PullImageRequest>::new()));
    let requests = captured.clone();
    let (mut client, server) =
        crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
            let requests = requests.clone();
            async move {
                let RpcRequestBody::PullImage(request) =
                    rpc.into_inner().decode_request().unwrap().body
                else {
                    panic!("only pull is expected");
                };
                requests.lock().unwrap().push(request);
                Ok(Response::new(
                    ployz_core::RpcResponse::from(ployz_core::ImagePulled::default())
                        .encode()
                        .unwrap(),
                ))
            }
        })
        .await;
    let mut private = spec(None, None, None);
    private.container.pull_policy = ployz_core::PullPolicy::Always;
    let mut public = private.clone();
    public.name = ployz_core::ServiceName::parse("other").unwrap();
    let auth = RegistryAuth {
        username: Some("octocat".into()),
        password: "token".into(),
    };
    client.registry_auth = std::collections::BTreeMap::from([(private.name.clone(), auth.clone())]);
    for specification in [&private, &public] {
        MachineOperations::ensure_image(&client, &machine('1'), specification)
            .await
            .unwrap();
    }
    server.abort();
    let sent = captured
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.registry_auth.clone())
        .collect::<Vec<_>>();
    assert_eq!(sent, [Some(auth), None]);
}

/// What the Machine is asked to want for a Container removed for `purpose`.
async fn removal_sent(purpose: ployz_core::RemoveContainerPurpose) -> Vec<ContainerWant> {
    use ployz_core::{EnsureContainerRequest, EnsuredContainer, OpaquePayload, RpcRequestBody};
    use std::sync::Arc;
    use tonic::{Request, Response};

    let captured = Arc::new(Mutex::new(Vec::<EnsureContainerRequest>::new()));
    let requests = captured.clone();
    let (client, server) =
        crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
            let requests = requests.clone();
            async move {
                let response = match rpc.into_inner().decode_request().unwrap().body {
                    RpcRequestBody::InspectContainer(_) => {
                        ployz_core::RpcResponse::from(ContainerDetails {
                            container: observation(&machine('1'), &container('a'), running()),
                            environment: None,
                            image_id: None,
                            last_check: None,
                            run: None,
                        })
                    }
                    RpcRequestBody::EnsureContainer(request) => {
                        requests.lock().unwrap().push(request);
                        ployz_core::RpcResponse::from(EnsuredContainer::Ensured {
                            health: None,
                            now: ContainerNow::Absent {
                                of: ContainerLocator::Id(container('a')),
                            },
                        })
                    }
                    other @ (RpcRequestBody::DescribeContract(_)
                    | RpcRequestBody::UpdateContainerLimits(_)
                    | RpcRequestBody::Inspect(_)
                    | RpcRequestBody::Enroll(_)
                    | RpcRequestBody::Register(_)
                    | RpcRequestBody::SetManagementClient(_)
                    | RpcRequestBody::SetPushTarget(_)
                    | RpcRequestBody::SelectEndpoint(_)
                    | RpcRequestBody::ListMachines(_)
                    | RpcRequestBody::UpdateMachine(_)
                    | RpcRequestBody::RequestMachineUpgrade(_)
                    | RpcRequestBody::Leave(_)
                    | RpcRequestBody::Forget(_)
                    | RpcRequestBody::ListContainers(_)
                    | RpcRequestBody::InspectStorage(_)
                    | RpcRequestBody::PrepareVolumes(_)
                    | RpcRequestBody::ListVolumes(_)
                    | RpcRequestBody::RemoveVolume(_)
                    | RpcRequestBody::InspectVolumeCopy(_)
                    | RpcRequestBody::SeedVolume(_)
                    | RpcRequestBody::SetVolumeCopy(_)
                    | RpcRequestBody::SetVolumeSize(_)
                    | RpcRequestBody::ListImages(_)
                    | RpcRequestBody::PruneImages(_)
                    | RpcRequestBody::EnsureImageIngest(_)
                    | RpcRequestBody::PullImage(_)
                    | RpcRequestBody::PullImageFromMachine(_)
                    | RpcRequestBody::MintBuildGrant(_)
                    | RpcRequestBody::InspectBuildGrant(_)
                    | RpcRequestBody::PublishCertificateMaterial(_)
                    | RpcRequestBody::ReadRoutes(_)
                    | RpcRequestBody::MetricHistory(_)
                    | RpcRequestBody::LogHistory(_)
                    | RpcRequestBody::TailLogs(_)
                    | RpcRequestBody::RuntimeWatch(_)) => {
                        panic!("only inspect and ensure are expected: {other:?}")
                    }
                };
                Ok(Response::new(response.encode().unwrap()))
            }
        })
        .await;
    execute_with(
        &[DeployOperation::RemoveContainer {
            machine_id: machine('1'),
            container_id: container('a'),
            purpose,
        }],
        &client,
        &CancellationToken::new(),
    )
    .await;
    server.abort();
    captured
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.want.clone())
        .collect()
}

#[tokio::test]
async fn retiring_a_container_keeps_its_anonymous_volumes() {
    let wants = removal_sent(ployz_core::RemoveContainerPurpose::Retire).await;
    assert!(
        matches!(
            wants.as_slice(),
            [ContainerWant::Gone {
                anonymous_volumes: ployz_core::AnonymousVolumes::Keep,
                ..
            }]
        ),
        "{wants:?}"
    );
}

#[tokio::test]
async fn a_rollout_removes_a_containers_anonymous_volumes_with_it() {
    let wants = removal_sent(ployz_core::RemoveContainerPurpose::Rollout).await;
    assert!(
        matches!(
            wants.as_slice(),
            [ContainerWant::Gone {
                anonymous_volumes: ployz_core::AnonymousVolumes::Remove,
                ..
            }]
        ),
        "{wants:?}"
    );
}

#[test]
fn global_containers_keep_their_revision_keys_and_the_rest_key_by_run_and_operation() {
    use ployz_core::ServiceMode;

    let namespace = test_namespace();
    let mut service = spec(None, None, None);
    service.mode = ServiceMode::Global;
    let mut revision = service.clone();
    revision.container.image = "alpine:new".into();
    let mut other_inputs = service.clone();
    other_inputs.update.monitor_millis = Some(500);
    let mut replica = service.clone();
    replica.mode = ServiceMode::Replicated {
        replicas: std::num::NonZeroU32::new(1).unwrap(),
    };
    let operations = [run(&machine('1'), replica.clone(), true)];
    let slots = Slots::new("run", &namespace, &operations);
    let key = |kind, spec: &ResolvedServiceSpec, index, replacing| {
        slots
            .slot(kind, spec, index, replacing)
            .creation_key
            .as_str()
            .to_owned()
    };
    let service_key =
        |spec: &ResolvedServiceSpec| key(ContainerKind::ServiceContainer, spec, 0, None);

    let first = service_key(&service);
    assert_eq!(first, crate::cluster::global_creation_key(&service));
    assert_eq!(
        first,
        key(ContainerKind::ServiceContainer, &service, 3, None)
    );
    assert_ne!(
        first,
        service_key(&revision),
        "explicit revisions can coexist"
    );
    assert_eq!(
        first,
        service_key(&other_inputs),
        "changed non-revision inputs reach keyed conflict validation"
    );
    let replacement = key(
        ContainerKind::ServiceContainer,
        &service,
        0,
        Some(container('f')),
    );
    assert_eq!(replacement, format!("{first}:replace:{}", container('f')));

    let hook = key(ContainerKind::PreDeployHook, &service, 0, None);
    let replicated = service_key(&replica);
    assert!(hook.starts_with("run:"));
    assert_eq!(hook, replicated, "kind already separates a hook's slot");
    assert_ne!(
        replicated,
        key(ContainerKind::ServiceContainer, &replica, 1, None),
        "each operation has its own slot"
    );
    let resent = Slots::new("run", &namespace, &operations);
    assert_eq!(
        replicated,
        resent
            .slot(ContainerKind::ServiceContainer, &replica, 0, None)
            .creation_key
            .as_str(),
        "a resent operation of the same run reaches the same slot"
    );
    let another_run = Slots::new("other", &namespace, &operations);
    assert_ne!(
        replicated,
        another_run
            .slot(ContainerKind::ServiceContainer, &replica, 0, None)
            .creation_key
            .as_str()
    );
    let another_plan = Slots::new(
        "run",
        &namespace,
        &[run(&machine('2'), replica.clone(), true)],
    );
    assert_ne!(
        replicated,
        another_plan
            .slot(ContainerKind::ServiceContainer, &replica, 0, None)
            .creation_key
            .as_str()
    );
}

#[tokio::test]
async fn global_replacement_scopes_creation_to_the_old_container_and_retires_it_explicitly() {
    let machine = machine('1');
    let old = container('a');
    let new = container('b');
    let mut service = spec(None, None, None);
    service.mode = ployz_core::ServiceMode::Global;
    let client = Scripted {
        observations: Some(vec![serving(&machine, &old, None)]),
        ..Scripted::new(vec![
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Remove(machine, old)),
        ])
    };
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
    assert_eq!(client.rotation(&new), Some(CopyRotation::In));
    assert_eq!(
        client.rotation(&old),
        None,
        "the old copy is retired explicitly"
    );
    let slots = client.slots.lock().unwrap();
    let [slot] = slots.as_slice() else {
        panic!("expected one new Container");
    };
    assert!(
        slot.creation_key
            .as_str()
            .ends_with(&format!(":replace:{old}"))
    );
    client.assert_done();
}

#[tokio::test]
async fn global_stop_first_retry_replays_retained_candidate_with_no_free_endpoints() {
    use crate::deploy::{DeployIntent, DeploySnapshot, PlanOptions, plan_deploy};
    use ployz_core::{
        BridgeEndpointCapacity, ContainerDetails, ContainerList, EnsuredContainer, Machine,
        MachineList, MachineName, MachineObservation, MembershipObservation, OpaquePayload,
        RequestedServiceSpec, RoutesRead, RpcRequestBody, RpcResponse, ServiceMode,
        WireGuardPublicKey,
    };
    use std::{collections::BTreeMap, sync::Arc};
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
        build_concurrency: None,
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
    let listed_target = target.clone();
    let (client, server) =
        crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
            let remote = remote.clone();
            let captured = captured.clone();
            let fake_target = listed_target.clone();
            async move {
                let mut remote = remote.lock().unwrap();
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "fixture exercises only replacement failure and retry primitives"
                )]
                let response = match rpc.into_inner().decode_request().unwrap().body {
                    RpcRequestBody::InspectContainer(inspect) => {
                        RpcResponse::from(ContainerDetails {
                            image_id: None,
                            last_check: None,
                            run: None,
                            container: remote
                                .iter()
                                .find(|c| c.container_id == inspect.container_id)
                                .unwrap()
                                .clone(),
                            environment: None,
                        })
                    }
                    RpcRequestBody::ListMachines(_) => RpcResponse::from(MachineList {
                        enrollment: None,
                        machines: vec![MachineObservation::new(
                            fake_target.clone(),
                            MembershipObservation::Up,
                        )],
                    }),
                    RpcRequestBody::ReadRoutes(_) => RpcResponse::from(RoutesRead {
                        complete: true,
                        ..RoutesRead::default()
                    }),
                    RpcRequestBody::PullImage(_) => {
                        RpcResponse::from(ployz_core::ImagePulled::default())
                    }
                    RpcRequestBody::ListContainers(_) => RpcResponse::from(ContainerList {
                        containers: remote.clone(),
                    }),
                    RpcRequestBody::EnsureContainer(ensure) => match ensure.want {
                        ContainerWant::Stopped {
                            container: ContainerLocator::Id(id),
                            ..
                        } => {
                            let stopped = remote.iter_mut().find(|c| c.container_id == id).unwrap();
                            stopped
                                .try_update(|parts| {
                                    parts.runtime = ContainerRuntimeObservation::Exited { code: 0 }
                                })
                                .unwrap();
                            RpcResponse::from(EnsuredContainer::Ensured {
                                health: None,
                                now: ContainerNow::Present(stopped.clone()),
                            })
                        }
                        // Restoring v1 fails too, so the retry replans the candidate as a run.
                        ContainerWant::Running {
                            container: RunTarget::Existing(_),
                            ..
                        } => RpcResponse::from(error("start failed")),
                        ContainerWant::Running {
                            container: RunTarget::New(new),
                            ..
                        } => {
                            let key = new.slot.creation_key.as_str().to_owned();
                            captured.lock().unwrap().push(key.clone());
                            if let Some(existing) = remote
                                .iter_mut()
                                .find(|c| c.labels.get(CREATION_KEY) == Some(&key))
                            {
                                assert_eq!(existing.resolved_spec, new.resolved_spec);
                                existing
                                    .try_update(|parts| {
                                        parts.runtime = ContainerRuntimeObservation::Running {
                                            health: HealthObservation::Healthy,
                                        }
                                    })
                                    .unwrap();
                                RpcResponse::from(EnsuredContainer::Ensured {
                                    health: None,
                                    now: ContainerNow::Present(existing.clone()),
                                })
                            } else {
                                let mut candidate = observation(
                                    &machine('1'),
                                    &new_id,
                                    ContainerRuntimeObservation::Created,
                                );
                                candidate
                                    .try_update(|parts| {
                                        parts.resolved_spec = new.resolved_spec;
                                        parts.created_at_unix_nanos = 1;
                                        parts.labels.insert(CREATION_KEY.into(), key);
                                    })
                                    .unwrap();
                                remote.push(candidate);
                                RpcResponse::from(error("start failed"))
                            }
                        }
                        other => panic!("unexpected want: {other:?}"),
                    },
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
        test_namespace(),
        requested,
        PlanOptions {
            skip_health_monitor: true,
            ..Default::default()
        },
    );
    let first = plan_deploy(&intent, &snapshot(1)).unwrap();
    assert!(matches!(
        execute_operation_sequence(&first, &run_id(), &client, &CancellationToken::new(), None)
            .await,
        DeployOutcome::Failed { .. }
    ));
    assert_eq!(
        state.lock().unwrap().len(),
        2,
        "failed start retains v2 beside stopped v1"
    );
    let retry =
        plan_deploy(&intent, &snapshot(2)).expect("retained candidate needs no new endpoint");
    let DeployOperation::RunContainer {
        machine_id, spec, ..
    } = retry.operations().first().unwrap()
    else {
        panic!("inactive replacement replans as a run");
    };
    let replay = MachineOperations::ensure_container(
        &client,
        machine_id,
        ContainerWant::Running {
            container: RunTarget::New(NewContainer {
                r#gen: ployz_core::Gen::FIRST,
                slot: slot_key(
                    &test_namespace(),
                    ContainerKind::ServiceContainer,
                    crate::cluster::global_creation_key(spec),
                ),
                resolved_spec: spec.clone(),
                deployment_id: None,
                registry_auth: None,
                image_prepared: true,
                answer: ployz_core::AnswerWhen::Started,
            }),
            rotation: RunRotation::Pending,
        },
    )
    .await
    .unwrap();
    assert_eq!(replay.0.present().map(|c| c.container_id), Some(new_id));
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

#[tokio::test]
async fn proxy_copies_never_routed_always_removable() {
    let (first, second) = (machine('1'), machine('2'));
    let (old, new_first, new_second) = (container('a'), container('b'), container('c'));
    let mut proxy = spec(None, None, None);
    proxy.name = ServiceName::parse("ingress").unwrap();
    let plan = vec![
        DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id: first,
            old_container_id: old,
            spec: proxy.clone(),
            skip_health_monitor: true,
        }),
        run(&second, proxy, true),
    ];
    let client = Scripted::new(vec![
        created(
            Call::Run(first, ContainerKind::ServiceContainer),
            &new_first,
        ),
        created(
            Call::Run(second, ContainerKind::ServiceContainer),
            &new_second,
        ),
        observed_ingress(Call::Inspect(first, old), running()),
        ok(Call::Remove(first, old)),
    ]);

    let outcome = super::super::execute_operation_sequence(
        &crate::deploy::DeployPlan::for_execution_test(plan, Namespace::system()),
        &run_id(),
        &client,
        &CancellationToken::new(),
        None,
    )
    .await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    for new in [new_first, new_second] {
        assert_eq!(
            client.rotation(&new),
            Some(CopyRotation::Pending),
            "a proxy copy stays Pending for life"
        );
    }
    assert_eq!(
        client.moves(),
        [Move::Gone(old)],
        "a proxy copy is never brought In or sent Out, and leaves by Gone"
    );
    client.assert_done();
}

#[tokio::test]
async fn ingress_create_cleanup_never_names_a_sibling() {
    use crate::deploy::{
        DeployIntent, DeploySnapshot, PlanOptions, SlotAction, plan_ingress_repairs,
    };
    use ployz_core::{
        Machine, MachineName, MachineObservation, MembershipObservation, WireGuardPublicKey,
    };

    let server = |hex: char| {
        MachineObservation::new(
            Machine {
                id: machine(hex),
                name: MachineName::parse(format!("server-{hex}")).unwrap(),
                subnet: format!("10.210.{hex}.0/24").parse().unwrap(),
                public_key: WireGuardPublicKey([hex as u8; 32]),
                public_ip: None,
                labels: Default::default(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
                build_concurrency: None,
            },
            MembershipObservation::Up,
        )
    };
    let ingress = ployz_core::caddy_service_spec("caddy:test".into(), Default::default());
    let proxy = |hex: char, on: char| {
        let mut observed = observation(&machine(on), &container(hex), healthy());
        observed
            .try_update(|parts| {
                parts.namespace = Namespace::system();
                parts.resolved_spec.name = ingress.name.clone();
                parts.labels.insert(CREATION_KEY.into(), hex.to_string());
            })
            .unwrap();
        observed
    };
    let snapshot = DeploySnapshot {
        machines: vec![server('1'), server('2'), server('3')],
        containers: vec![proxy('a', '1'), proxy('b', '2')],
        ..Default::default()
    };
    let intent = DeployIntent::apply_all(Namespace::system(), [&ingress], PlanOptions::default());
    let [repair] = plan_ingress_repairs(&intent, &snapshot).try_into().unwrap();
    assert_eq!(repair.machine_id, machine('3'));
    let SlotAction::Create(Ok(plan)) = repair.action else {
        panic!(
            "expected a create on the empty Server, got {:?}",
            repair.action
        );
    };
    let siblings = [
        (machine('1'), container('a')),
        (machine('2'), container('b')),
    ];
    let new = container('c');
    let mut client = Scripted {
        observations: Some(snapshot.containers.clone()),
        ..Scripted::new(vec![created(
            Call::Run(machine('3'), ContainerKind::ServiceContainer),
            &new,
        )])
    };
    for (machine_id, container_id) in &siblings {
        client = client.with_copy_at(
            &QualifiedService::system_ingress(),
            machine_id,
            container_id,
            CopyRotation::Pending,
            Gen::FIRST,
        );
    }

    let outcome = super::super::execute_operation_sequence(
        &plan,
        &run_id(),
        &client,
        &CancellationToken::new(),
        None,
    )
    .await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(
        client.moves().is_empty(),
        "no want reaches a running sibling proxy: {:?}",
        client.moves()
    );
    for (_, sibling) in siblings {
        assert_eq!(client.rotation(&sibling), Some(CopyRotation::Pending));
    }
    client.assert_done();
}

/// Only work that starts a Container reads as starting. A removal's row says
/// what it does from its first frame, for every reader of the stored phase.
#[tokio::test]
async fn stop_and_remove_rows_never_read_as_starting() {
    let machine = machine('1');
    let (stopped, removed, hook) = (container('9'), container('a'), container('b'));
    let volume = DockerVolumeId {
        machine_id: machine,
        name: DockerVolumeName::parse("data").unwrap(),
    };
    let plan = vec![
        stop(&machine, &stopped),
        DeployOperation::RemoveContainer {
            machine_id: machine,
            container_id: removed,
            purpose: ployz_core::RemoveContainerPurpose::Retire,
        },
        DeployOperation::StopHook {
            machine_id: machine,
            container_id: hook,
        },
        DeployOperation::RemoveVolume { id: volume.clone() },
    ];
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, stopped), running()),
        ok(Call::Stop(machine, stopped)),
        observed(Call::Inspect(machine, removed), running()),
        ok(Call::Remove(machine, removed)),
        ok(Call::StopNow(machine, hook)),
        ok(Call::RemoveVolume(volume)),
    ])
    .with_copy(&machine, &removed, CopyRotation::In);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let outcome = super::super::execute_operation_sequence(
        &crate::deploy::DeployPlan::for_execution_test(plan, test_namespace()),
        &run_id(),
        &client,
        &CancellationToken::new(),
        Some(tx),
    )
    .await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let mut statuses = vec![Vec::new(); 4];
    while let Ok(event) = rx.try_recv() {
        if let DeployEvent::Progress { rows, .. } = event {
            for row in rows {
                let seen: &mut Vec<_> = &mut statuses[row.index as usize];
                if seen.last() != Some(&row.status) {
                    seen.push(row.status);
                }
            }
        }
    }
    let ran = |phase| {
        vec![
            OperationStatus::Pending,
            OperationStatus::Running { phase },
            OperationStatus::Completed,
        ]
    };
    assert_eq!(
        statuses,
        [
            ran(OperationPhase::StoppingContainer),
            ran(OperationPhase::RemovingContainer),
            ran(OperationPhase::StoppingContainer),
            ran(OperationPhase::RemovingVolume),
        ]
    );
    client.assert_done();
}
