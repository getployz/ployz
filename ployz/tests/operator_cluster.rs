use std::{collections::BTreeSet, path::Path, process, time::Duration};

use futures_util::StreamExt;
use ployz::operator::{
    ExecMode, LogInput, ServiceArg, exec_options, merge_logs, open_exec, open_machine_logs,
    open_service_logs, select_exec_container,
};
use ployz_core::{
    ContainerAction, ContainerKind, ContainerSelector, ExecRequestFrame, ExecResponseFrame,
    FanoutSelector, LogBody, LogEntry, LogOrigin, LogsOptions, MachineLogService, MachineTarget,
    ProjectName, ResolvedServiceSpec, ServiceId, ServiceSelector, StartContainerRequest, op,
    select_service,
};
use ployz_testkit::{Cluster, ClusterPlan};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn exec_service_logs_and_machine_logs_cross_a_real_two_machine_cluster() {
    let plan = ClusterPlan::new(&format!("l3-operator-{}", process::id()), 2).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    let machines = cluster.initialize_two().await.unwrap();
    let mut client = ployz::connect::connect(
        Path::new("/missing-ployz-test-config"),
        Some(&cluster.api_address(0).unwrap()),
        None,
    )
    .await
    .unwrap();
    let service_id = ServiceId::random();
    let spec = service_spec(&service_id);
    let mut created = Vec::new();
    for machine in [&machines[0], &machines[0], &machines[1]] {
        let container = client
            .create_container(
                machine.id,
                ContainerKind::ServiceContainer,
                ProjectName::parse("app").unwrap(),
                spec.clone(),
            )
            .await
            .unwrap();
        client
            .call::<op::StartContainer>(
                StartContainerRequest {
                    container_id: container.container_id,
                },
                Some(&MachineTarget::from(&machine.id)),
            )
            .await
            .unwrap();
        created.push(container.container_id);
    }
    let observed = wait_for_service(&mut client, &service_id, 3).await;

    assert!(
        open_exec(
            &mut client,
            &ServiceSelector::parse("missing").unwrap(),
            None,
            attached(["true"]),
        )
        .await
        .is_err()
    );
    assert!(
        open_exec(
            &mut client,
            &ServiceSelector::from(&service_id),
            Some(&ContainerSelector::parse("missing").unwrap()),
            attached(["true"])
        )
        .await
        .is_err()
    );

    let first = select_exec_container(&observed, None)
        .unwrap()
        .as_observation();
    for selector in [
        first.display_name.clone(),
        first.container_id.to_string(),
        first.container_id.as_str()[..12].to_owned(),
    ] {
        let frames = run_exec(
            &mut client,
            &ServiceSelector::from(&service_id),
            Some(&ContainerSelector::parse(&selector).unwrap()),
            attached(["sh", "-c", "printf out; printf err >&2; exit 42"]),
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(frames.contains(&ExecResponseFrame::Stdout(b"out".to_vec())));
        assert!(frames.contains(&ExecResponseFrame::Stderr(b"err".to_vec())));
        assert!(frames.contains(&ExecResponseFrame::Exit(42)));
    }

    assert!(
        run_exec(
            &mut client,
            &ServiceSelector::from(&service_id),
            None,
            attached(["sh", "-c", "exit 127"]),
            Vec::new(),
        )
        .await
        .unwrap()
        .contains(&ExecResponseFrame::Exit(127))
    );
    let defaults = exec_options(Vec::new(), ExecMode::Attached { tty: false });
    assert!(
        run_exec(
            &mut client,
            &ServiceSelector::from(&service_id),
            None,
            defaults,
            Vec::new(),
        )
        .await
        .unwrap()
        .contains(&ExecResponseFrame::Exit(0))
    );

    let stdin = run_exec(
        &mut client,
        &ServiceSelector::from(&service_id),
        None,
        attached(["sh", "-c", "read line; printf 'got:%s' \"$line\""]),
        vec![ExecRequestFrame::Stdin(b"hello\n".to_vec())],
    )
    .await
    .unwrap();
    assert!(stdin.contains(&ExecResponseFrame::Stdout(b"got:hello".to_vec())));
    let tty = run_exec(
        &mut client,
        &ServiceSelector::from(&service_id),
        None,
        tty(["sh", "-c", "printf tty-output"]),
        vec![ExecRequestFrame::Resize {
            width: 120,
            height: 40,
        }],
    )
    .await
    .unwrap();
    assert!(tty.contains(&ExecResponseFrame::Exit(0)), "{tty:?}");
    assert!(
        tty.iter()
            .any(|frame| matches!(frame, ExecResponseFrame::Stdout(_)))
    );

    let detached_frames = run_exec(
        &mut client,
        &ServiceSelector::from(&service_id),
        None,
        detached(["sleep", "1"]),
        Vec::new(),
    )
    .await
    .unwrap();
    assert!(matches!(
        detached_frames.as_slice(),
        [ExecResponseFrame::ExecId(_)]
    ));
    assert!(
        open_exec(
            &mut client,
            &ServiceSelector::from(&service_id),
            None,
            detached(["command-that-does-not-exist"]),
        )
        .await
        .is_err()
    );

    assert_service_logs(&mut client, &service_id, &observed, &machines).await;
    assert_machine_logs(&mut client, &machines).await;

    let live = client.live_services().await.unwrap();
    let services = live.services();
    let service = select_service(&services, &ServiceSelector::from(&service_id)).unwrap();
    client
        .change_observed_service(service, ContainerAction::Remove, None, None)
        .await;
    assert_eq!(created.len(), 3);
}

async fn assert_service_logs(
    client: &mut ployz::connect::Client,
    service_id: &ServiceId,
    observed: &ployz_core::ServiceObservation,
    machines: &[ployz_core::Machine; 2],
) {
    let args = [ServiceArg {
        service: ServiceSelector::from(service_id),
        containers: Vec::new(),
    }];
    let entries = collect_logs(
        open_service_logs(
            client,
            &args,
            &[],
            log_options(),
            false,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .inputs,
    )
    .await;
    let actual = entries
        .iter()
        .filter_map(|entry| match &entry.metadata.origin {
            LogOrigin::Service { container_id, .. }
                if matches!(entry.body, LogBody::Stdout(_) | LogBody::Stderr(_)) =>
            {
                Some(*container_id)
            }
            LogOrigin::Service { .. } | LogOrigin::Machine { .. } => None,
        })
        .collect::<Vec<_>>();
    for container in &observed.containers {
        let streams = entries
            .iter()
            .filter_map(|entry| match &entry.metadata.origin {
                LogOrigin::Service { container_id, .. }
                    if *container_id == container.as_observation().container_id =>
                {
                    match &entry.body {
                        LogBody::Stdout(_) => Some("stdout"),
                        LogBody::Stderr(_) => Some("stderr"),
                        LogBody::Heartbeat | LogBody::Error(_) => None,
                    }
                }
                LogOrigin::Service { .. } | LogOrigin::Machine { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(streams, ["stdout", "stderr"]);
        assert_eq!(
            actual
                .iter()
                .filter(|id| **id == container.as_observation().container_id)
                .count(),
            2
        );
    }

    let compose_opened = open_service_logs(
        client,
        &[
            args[0].clone(),
            ServiceArg {
                service: ServiceSelector::parse("disabled-but-undeployed").unwrap(),
                containers: Vec::new(),
            },
        ],
        &[],
        log_options(),
        true,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        compose_opened.skipped_services,
        [ServiceSelector::parse("disabled-but-undeployed").unwrap()]
    );
    assert_eq!(compose_opened.inputs.len(), 3);
    assert!(
        open_service_logs(
            client,
            &[ServiceArg {
                service: ServiceSelector::parse("disabled-but-undeployed").unwrap(),
                containers: Vec::new(),
            }],
            &[],
            log_options(),
            true,
            CancellationToken::new(),
        )
        .await
        .is_err()
    );

    let selected = observed.containers.first().unwrap().as_observation();
    for selector in [
        selected.container_id.to_string(),
        selected.display_name.clone(),
        selected.container_id.as_str()[..12].to_owned(),
    ] {
        let inputs = open_service_logs(
            client,
            &[ServiceArg {
                service: ServiceSelector::from(service_id),
                containers: vec![ContainerSelector::parse(&selector).unwrap()],
            }],
            &[],
            log_options(),
            false,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .inputs;
        assert_eq!(inputs.len(), 1);
    }
    assert!(
        open_service_logs(
            client,
            &[ServiceArg {
                service: ServiceSelector::from(service_id),
                containers: vec![ContainerSelector::parse("missing").unwrap()],
            }],
            &[],
            log_options(),
            false,
            CancellationToken::new(),
        )
        .await
        .is_err()
    );
    let selected_machine = open_service_logs(
        client,
        &args,
        &[FanoutSelector::parse(machines[0].name.as_str()).unwrap()],
        log_options(),
        false,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(selected_machine.inputs.len(), 2);
    assert!(
        open_service_logs(
            client,
            &args,
            &[FanoutSelector::parse("missing").unwrap()],
            log_options(),
            false,
            CancellationToken::new(),
        )
        .await
        .is_err()
    );
}

async fn assert_machine_logs(
    client: &mut ployz::connect::Client,
    machines: &[ployz_core::Machine; 2],
) {
    for service in [
        MachineLogService::Ployz,
        MachineLogService::Docker,
        MachineLogService::Corrosion,
    ] {
        let entries = collect_logs(
            open_machine_logs(
                client,
                &[service.to_string()],
                &[],
                log_options(),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        )
        .await;
        let tagged = entries
            .iter()
            .filter_map(|entry| match entry.metadata.origin {
                LogOrigin::Machine { service: actual } if actual == service => {
                    Some(entry.metadata.machine_id)
                }
                LogOrigin::Service { .. } | LogOrigin::Machine { .. } => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            tagged,
            machines
                .iter()
                .map(|machine| machine.id)
                .collect::<BTreeSet<_>>()
        );
    }
    assert!(
        open_machine_logs(
            client,
            &["unsupported".into()],
            &[],
            log_options(),
            CancellationToken::new(),
        )
        .await
        .is_err()
    );
    let cancellation = CancellationToken::new();
    let inputs = open_machine_logs(
        client,
        &[],
        &[FanoutSelector::parse(machines[0].name.as_str()).unwrap()],
        LogsOptions {
            follow: true,
            ..log_options()
        },
        cancellation.clone(),
    )
    .await
    .unwrap();
    assert_eq!(inputs.len(), 1);
    let mut output = merge_logs(inputs, cancellation.clone());
    cancellation.cancel();
    assert!(output.recv().await.is_none());
}

async fn run_exec(
    client: &mut ployz::connect::Client,
    service: &ServiceSelector,
    container: Option<&ContainerSelector>,
    options: ployz_core::ExecOptions,
    input: Vec<ExecRequestFrame>,
) -> Result<Vec<ExecResponseFrame>, ployz::operator::OperatorError> {
    let mut session = open_exec(client, service, container, options).await?;
    for frame in input {
        session.input.send(frame.encode()?).await.unwrap();
    }
    drop(session.input);
    let mut frames = Vec::new();
    while let Some(frame) = session.output.next().await {
        frames.push(ExecResponseFrame::decode(&frame?)?);
    }
    Ok(frames)
}

async fn collect_logs(inputs: Vec<LogInput>) -> Vec<LogEntry> {
    let mut output = merge_logs(inputs, CancellationToken::new());
    let mut entries = Vec::new();
    while let Some(entry) = output.recv().await {
        entries.push(entry.unwrap());
    }
    entries
}

async fn wait_for_service(
    client: &mut ployz::connect::Client,
    service_id: &ServiceId,
    containers: usize,
) -> ployz_core::ServiceObservation {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(live) = client.live_services().await {
                let services = live.services();
                if let Ok(service) = select_service(&services, &ServiceSelector::from(service_id))
                    && service.containers.len() == containers
                {
                    return service.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap()
}

fn attached<const N: usize>(command: [&str; N]) -> ployz_core::ExecOptions {
    exec_options(
        command.into_iter().map(ToOwned::to_owned).collect(),
        ExecMode::Attached { tty: false },
    )
}

fn tty<const N: usize>(command: [&str; N]) -> ployz_core::ExecOptions {
    exec_options(
        command.into_iter().map(ToOwned::to_owned).collect(),
        ExecMode::Attached { tty: true },
    )
}

fn detached<const N: usize>(command: [&str; N]) -> ployz_core::ExecOptions {
    exec_options(
        command.into_iter().map(ToOwned::to_owned).collect(),
        ExecMode::Detached,
    )
}

fn log_options() -> LogsOptions {
    LogsOptions {
        follow: false,
        tail: -1,
        since_unix_seconds: None,
        until_unix_seconds: None,
    }
}

fn service_spec(service_id: &ServiceId) -> ResolvedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "service_id": service_id,
        "name": "operator-streams",
        "mode": { "mode": "replicated", "replicas": 3 },
        "container": {
            "image": "alpine:3.23.3",
            "command": [
                "sh",
                "-c",
                "printf 'container-out\\n'; sleep 0.05; printf 'container-err\\n' >&2; sleep 300"
            ],
            "pull_policy": "missing"
        }
    }))
    .unwrap()
}
