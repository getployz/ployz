use std::{collections::BTreeSet, path::Path, process, time::Duration};

use futures_util::StreamExt;
use ployz::operator::{
    ExecMode, LogInput, ServiceArg, exec_options, merge_logs, select_exec_container,
};
use ployz_core::{
    ContainerAction, ContainerKind, ExecRequestFrame, ExecResponseFrame, LogEntry, LogOrigin,
    LogsOptions, MachineLogService, ResolvedServiceSpec, ServiceId, select_service,
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
            .create_container(machine.id, ContainerKind::ServiceContainer, spec.clone())
            .await
            .unwrap();
        client
            .change_container(
                machine.id,
                container.container_id,
                ContainerAction::Start,
                None,
                None,
            )
            .await
            .unwrap();
        created.push(container.container_id);
    }
    let observed = wait_for_service(&mut client, &service_id, 3).await;

    assert!(
        client
            .open_exec("missing", None, attached(["true"]))
            .await
            .is_err()
    );
    assert!(
        client
            .open_exec(service_id.as_str(), Some("missing"), attached(["true"]))
            .await
            .is_err()
    );

    let first = select_exec_container(&observed, None).unwrap();
    for selector in [
        first.display_name.clone(),
        first.container_id.to_string(),
        first.container_id.as_str()[..12].to_owned(),
    ] {
        let frames = run_exec(
            &mut client,
            service_id.as_str(),
            Some(&selector),
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
            service_id.as_str(),
            None,
            attached(["sh", "-c", "exit 127"]),
            Vec::new(),
        )
        .await
        .unwrap()
        .contains(&ExecResponseFrame::Exit(127))
    );
    let defaults = exec_options(
        Vec::new(),
        ExecMode {
            detach: false,
            no_tty: true,
            stdout_terminal: false,
            stdin_terminal: false,
        },
    )
    .unwrap();
    assert!(
        run_exec(&mut client, service_id.as_str(), None, defaults, Vec::new(),)
            .await
            .unwrap()
            .contains(&ExecResponseFrame::Exit(0))
    );

    let stdin = run_exec(
        &mut client,
        service_id.as_str(),
        None,
        attached(["sh", "-c", "read line; printf 'got:%s' \"$line\""]),
        vec![ExecRequestFrame::Stdin(b"hello\n".to_vec())],
    )
    .await
    .unwrap();
    assert!(stdin.contains(&ExecResponseFrame::Stdout(b"got:hello".to_vec())));
    let tty = run_exec(
        &mut client,
        service_id.as_str(),
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
        service_id.as_str(),
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
        client
            .open_exec(
                service_id.as_str(),
                None,
                detached(["command-that-does-not-exist"]),
            )
            .await
            .is_err()
    );

    assert_service_logs(&mut client, &service_id, &observed, &machines).await;
    assert_machine_logs(&mut client, &machines).await;

    client
        .change_service(service_id.as_str(), ContainerAction::Remove, None, None)
        .await
        .unwrap();
    assert_eq!(created.len(), 3);
}

async fn assert_service_logs(
    client: &mut ployz::connect::Client,
    service_id: &ServiceId,
    observed: &ployz_core::ServiceObservation,
    machines: &[ployz_core::Machine; 2],
) {
    let args = [ServiceArg {
        service: service_id.to_string(),
        containers: Vec::new(),
    }];
    let entries = collect_logs(
        client
            .open_service_logs(&args, &[], log_options(), false, CancellationToken::new())
            .await
            .unwrap()
            .inputs,
    )
    .await;
    let actual = entries
        .iter()
        .filter_map(|entry| match &entry.metadata.origin {
            LogOrigin::Service { container_id, .. }
                if matches!(
                    entry.stream,
                    ployz_core::LogStream::Stdout | ployz_core::LogStream::Stderr
                ) =>
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
                    if *container_id == container.container_id
                        && matches!(
                            entry.stream,
                            ployz_core::LogStream::Stdout | ployz_core::LogStream::Stderr
                        ) =>
                {
                    Some(entry.stream)
                }
                LogOrigin::Service { .. } | LogOrigin::Machine { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            streams,
            [ployz_core::LogStream::Stdout, ployz_core::LogStream::Stderr]
        );
        assert_eq!(
            actual
                .iter()
                .filter(|id| **id == container.container_id)
                .count(),
            2
        );
    }

    let compose_opened = client
        .open_service_logs(
            &[
                args[0].clone(),
                ServiceArg {
                    service: "disabled-but-undeployed".into(),
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
    assert_eq!(compose_opened.skipped_services, ["disabled-but-undeployed"]);
    assert_eq!(compose_opened.inputs.len(), 3);
    assert!(
        client
            .open_service_logs(
                &[ServiceArg {
                    service: "disabled-but-undeployed".into(),
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

    let selected = observed.containers.first().unwrap();
    for selector in [
        selected.container_id.to_string(),
        selected.display_name.clone(),
        selected.container_id.as_str()[..12].to_owned(),
    ] {
        let inputs = client
            .open_service_logs(
                &[ServiceArg {
                    service: service_id.to_string(),
                    containers: vec![selector],
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
        client
            .open_service_logs(
                &[ServiceArg {
                    service: service_id.to_string(),
                    containers: vec!["missing".into()],
                }],
                &[],
                log_options(),
                false,
                CancellationToken::new(),
            )
            .await
            .is_err()
    );
    let selected_machine = client
        .open_service_logs(
            &args,
            &[machines[0].name.to_string()],
            log_options(),
            false,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(selected_machine.inputs.len(), 2);
    assert!(
        client
            .open_service_logs(
                &args,
                &["missing".into()],
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
            client
                .open_machine_logs(
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
        client
            .open_machine_logs(
                &["unsupported".into()],
                &[],
                log_options(),
                CancellationToken::new(),
            )
            .await
            .is_err()
    );
    let cancellation = CancellationToken::new();
    let inputs = client
        .open_machine_logs(
            &[],
            &[machines[0].name.to_string()],
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
    service: &str,
    container: Option<&str>,
    options: ployz_core::ExecOptions,
    input: Vec<ExecRequestFrame>,
) -> Result<Vec<ExecResponseFrame>, ployz::operator::OperatorError> {
    let mut session = client.open_exec(service, container, options).await?;
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
            if let Ok(live) = client.live_services().await
                && let Ok(service) = select_service(&live.services, service_id.as_str())
                && service.containers.len() == containers
            {
                return service.clone();
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
        ExecMode {
            detach: false,
            no_tty: true,
            stdout_terminal: false,
            stdin_terminal: false,
        },
    )
    .unwrap()
}

fn tty<const N: usize>(command: [&str; N]) -> ployz_core::ExecOptions {
    exec_options(
        command.into_iter().map(ToOwned::to_owned).collect(),
        ExecMode {
            detach: false,
            no_tty: false,
            stdout_terminal: true,
            stdin_terminal: true,
        },
    )
    .unwrap()
}

fn detached<const N: usize>(command: [&str; N]) -> ployz_core::ExecOptions {
    exec_options(
        command.into_iter().map(ToOwned::to_owned).collect(),
        ExecMode {
            detach: true,
            no_tty: false,
            stdout_terminal: true,
            stdin_terminal: true,
        },
    )
    .unwrap()
}

fn log_options() -> LogsOptions {
    LogsOptions {
        follow: false,
        tail: -1,
        since: String::new(),
        until: String::new(),
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
