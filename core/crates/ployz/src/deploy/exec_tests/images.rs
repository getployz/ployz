use super::*;

fn run(machine_id: MachineId, spec: &ResolvedServiceSpec) -> DeployOperation {
    DeployOperation::RunContainer {
        machine_id,
        spec: spec.clone(),
        skip_health_monitor: true,
    }
}

#[tokio::test]
async fn images_are_pulled_once_per_machine_before_any_operation() {
    let first = machine('1');
    let second = machine('2');
    let service = spec(None, None, None);
    let mut always = service.clone();
    always.container.pull_policy = ployz_core::PullPolicy::Always;
    let operations = vec![
        run(first, &service),
        run(first, &always),
        run(second, &service),
    ];
    let client = Scripted::new(vec![
        ok(pull(first, &service)),
        ok(pull(second, &service)),
        created(
            Call::Create(first, ContainerKind::ServiceContainer),
            &container('a'),
        ),
        ok(Call::Start(first, container('a'))),
        serving(container('a')),
        created(
            Call::Create(first, ContainerKind::ServiceContainer),
            &container('b'),
        ),
        ok(Call::Start(first, container('b'))),
        serving(container('b')),
        created(
            Call::Create(second, ContainerKind::ServiceContainer),
            &container('c'),
        ),
        ok(Call::Start(second, container('c'))),
        serving(container('c')),
    ]);

    let outcome = execute_with(&operations, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        DeployOutcome::Success {
            completed: operations
        }
    );
    client.assert_done();
}

#[tokio::test]
async fn a_failed_pull_runs_nothing_and_names_the_first_operation_needing_the_image() {
    let first = machine('1');
    let second = machine('2');
    let service = spec(None, None, None);
    let mut hook = spec(None, None, Some(5_000));
    hook.container.image = "busybox:1.37.0".into();
    let run_first = run(first, &service);
    let run_hook = DeployOperation::RunHook {
        machine_id: second,
        spec: hook.clone(),
        old_hook_containers: Vec::new(),
    };
    let run_second = run(second, &service);
    let operations = vec![run_first.clone(), run_hook.clone(), run_second.clone()];
    let client = Scripted::new(vec![
        ok(pull(first, &service)),
        failed(pull(second, &hook), "registry refused"),
    ]);

    let outcome = execute_with(&operations, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        DeployOutcome::Failed {
            completed: Vec::new(),
            failed: FailedOperation::Operation {
                operation: run_hook,
                error: ExecutionError::Machine {
                    action: MachineAction::PullImage,
                    error: error("registry refused"),
                },
            },
            unexecuted: vec![run_first, run_second],
        }
    );
    client.assert_done();
}
