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
    let mut global = spec(None, None, None);
    global.mode = ployz_core::ServiceMode::Global;
    for (service, cleanup) in [
        (
            spec(None, None, None),
            ok(Call::Remove(machine('1'), container('a'))),
        ),
        (
            spec(None, None, None),
            failed(Call::Remove(machine('1'), container('a')), "cleanup failed"),
        ),
        (global, ok(Call::Remove(machine('1'), container('a')))),
    ] {
        let machine = machine('1');
        let created_id = container('a');
        let plan = vec![run(&machine, service, false)];
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
async fn a_private_image_is_created_with_its_services_credentials_only() {
    use ployz_core::{
        CreateContainerRequest, OpaquePayload, RegistryAuth, RpcRequestBody, RpcResponse,
    };
    use std::sync::Arc;
    use tonic::{Request, Response};

    let captured = Arc::new(Mutex::new(Vec::<CreateContainerRequest>::new()));
    let requests = captured.clone();
    let (mut client, server) =
        crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
            let requests = requests.clone();
            async move {
                let RpcRequestBody::CreateContainer(request) =
                    rpc.into_inner().decode_request().unwrap().body
                else {
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
    let mut private = spec(None, None, None);
    private.container.pull_policy = ployz_core::PullPolicy::Always;
    let mut public = private.clone();
    public.name = ployz_core::ServiceName::parse("other").unwrap();
    let auth = RegistryAuth {
        username: Some("octocat".into()),
        password: "token".into(),
    };
    client.registry_auth = std::collections::BTreeMap::from([(private.name.clone(), auth.clone())]);
    for (kind, specification) in [
        (ContainerKind::ServiceContainer, &private),
        (ContainerKind::PreDeployHook, &private),
        (ContainerKind::ServiceContainer, &public),
    ] {
        MachineOperations::create_container(
            &client,
            &machine('1'),
            kind,
            &test_namespace(),
            specification,
            &DeployRun::new().creation_key(0),
        )
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
    assert_eq!(sent, [Some(auth.clone()), Some(auth), None]);
}

#[tokio::test]
async fn every_create_is_keyed_by_its_run_and_no_later_run_shares_a_key() {
    let machine = machine('1');
    let old = container('a');
    let mut global = spec(None, None, None);
    global.mode = ployz_core::ServiceMode::Global;
    let hook_spec = spec(None, None, Some(5_000));
    let plan = vec![
        hook(&machine, hook_spec.clone()),
        DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id: machine,
            old_container_id: old,
            spec: global.clone(),
            skip_health_monitor: true,
        }),
        run(&machine, spec(None, None, None), true),
        run(&machine, spec(None, None, None), true),
    ];
    let mut runs = Vec::new();
    for _ in 0..2 {
        let (hook_id, new, first, second) = (
            container('b'),
            container('c'),
            container('d'),
            container('e'),
        );
        let client = Scripted::new(vec![
            ok(pull(machine, &hook_spec)),
            created(
                Call::Create(machine, ContainerKind::PreDeployHook),
                &hook_id,
            ),
            ok(Call::Start(machine, hook_id)),
            Step(
                Call::Inspect(machine, hook_id),
                Reply::Observed(ContainerRuntimeObservation::Exited { code: 0 }, None),
            ),
            created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
            ok(Call::Start(machine, new)),
            serving(new),
            ok(Call::Stop(machine, old)),
            ok(Call::Remove(machine, old)),
            dropped(old),
            created(
                Call::Create(machine, ContainerKind::ServiceContainer),
                &first,
            ),
            ok(Call::Start(machine, first)),
            serving(first),
            created(
                Call::Create(machine, ContainerKind::ServiceContainer),
                &second,
            ),
            ok(Call::Start(machine, second)),
            serving(second),
        ]);
        assert!(matches!(
            execute_with(&plan, &client, &CancellationToken::new()).await,
            DeployOutcome::Success { .. }
        ));
        client.assert_done();
        runs.push(client.keys.into_inner().unwrap());
    }
    let [earlier, later] = runs.as_slice() else {
        unreachable!("two runs executed");
    };
    let keys: std::collections::BTreeSet<_> =
        earlier.iter().chain(later).map(|key| &key.0).collect();
    assert_eq!(
        keys.len(),
        8,
        "each create in a run has its own key and a later run, even of an identical Global, shares none: {runs:?}"
    );
}

#[tokio::test]
async fn the_client_sends_the_runs_key_without_looking_for_a_reusable_container() {
    use ployz_core::{CreateContainerRequest, OpaquePayload, RpcRequestBody, RpcResponse};
    use std::sync::Arc;
    use tonic::{Request, Response};

    let captured = Arc::new(Mutex::new(Vec::<CreateContainerRequest>::new()));
    let requests = captured.clone();
    let (client, server) =
        crate::connect::test_support::rpc_client(move |rpc: Request<OpaquePayload>| {
            let requests = requests.clone();
            async move {
                let RpcRequestBody::CreateContainer(request) =
                    rpc.into_inner().decode_request().unwrap().body
                else {
                    panic!("a create sends only CreateContainer");
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
    let mut global = spec(None, None, None);
    global.mode = ployz_core::ServiceMode::Global;
    let key = DeployRun::new().creation_key(3);
    MachineOperations::create_container(
        &client,
        &machine('1'),
        ContainerKind::ServiceContainer,
        &test_namespace(),
        &global,
        &key,
    )
    .await
    .unwrap();
    server.abort();
    let sent = captured.lock().unwrap();
    let [request] = sent.as_slice() else {
        panic!("expected one create: {sent:?}");
    };
    assert_eq!(request.creation_key.as_ref(), Some(&key.0));
}
