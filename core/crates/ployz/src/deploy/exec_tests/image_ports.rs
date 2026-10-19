use super::*;

fn exposing(ports: &[&str]) -> ImagePulled {
    ImagePulled {
        exposed_ports: ports
            .iter()
            .map(|key| ExposedPort::from_docker(key).unwrap())
            .collect(),
    }
}

fn routed(container_port: u16) -> ResolvedServiceSpec {
    let mut web = spec(None, None, None);
    web.name = ServiceName::parse("web").unwrap();
    web.container.image = "nginx".into();
    web.ports = vec![
        serde_json::from_value(serde_json::json!({
            "mode": "ingress",
            "hostname": "web.example.com",
            "load_balancer_port": 443,
            "container_port": container_port,
            "http_protocol": "http",
        }))
        .unwrap(),
    ];
    web
}

/// Run `plan` with every Machine reporting `reply` for each pull, and answer the
/// Deploy's outcome with the warnings it raised.
async fn deploy(
    plan: Vec<DeployOperation>,
    reply: ImagePulled,
) -> (DeployOutcome<ExecutionError>, Vec<DeployWarning>) {
    let replies = plan
        .iter()
        .map(|operation| (operation.machine_id(), reply.clone()))
        .collect();
    deploy_with(plan, replies).await
}

/// Run `plan` with each Machine reporting its own reply for each pull.
async fn deploy_with(
    plan: Vec<DeployOperation>,
    replies: BTreeMap<MachineId, ImagePulled>,
) -> (DeployOutcome<ExecutionError>, Vec<DeployWarning>) {
    let steps = plan
        .iter()
        .enumerate()
        .map(|(index, operation)| {
            let hex = char::from_digit(u32::try_from(index).unwrap() + 10, 16).unwrap();
            created(
                Call::Run(operation.machine_id(), ContainerKind::ServiceContainer),
                &container(hex),
            )
        })
        .collect();
    let client = Scripted {
        pull_replies: replies,
        ..Scripted::new(steps)
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome = super::super::execute_operation_sequence(
        &crate::deploy::DeployPlan::for_execution_test(plan, test_namespace()),
        &run_id(),
        &client,
        &CancellationToken::new(),
        Some(tx),
    )
    .await;
    client.assert_done();
    let mut warnings = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let DeployEvent::Warning { warning } = event {
            warnings.push(warning);
        }
    }
    (outcome, warnings)
}

#[tokio::test]
async fn an_image_exposing_another_port_warns_and_the_deploy_still_completes() {
    let (outcome, warnings) = deploy(
        vec![run(&machine('1'), routed(8080), true)],
        exposing(&["80/tcp"]),
    )
    .await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        warnings,
        [DeployWarning::ImagePortMismatch {
            service: ServiceName::parse("web").unwrap(),
            image: "nginx".into(),
            exposed: vec![80],
            routed: vec![8080],
        }]
    );
    assert_eq!(
        warnings.first().map(ToString::to_string).as_deref(),
        Some(
            "web's image nginx exposes 80; its domains send to 8080. Set the domain port or PORT."
        )
    );
}

#[tokio::test]
async fn a_domain_sending_to_an_exposed_port_raises_no_warning() {
    let (outcome, warnings) = deploy(
        vec![run(&machine('1'), routed(80), true)],
        exposing(&["80/tcp", "443/tcp"]),
    )
    .await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[tokio::test]
async fn an_image_that_exposes_nothing_raises_no_warning() {
    let (_, warnings) = deploy(
        vec![run(&machine('1'), routed(8080), true)],
        ImagePulled::default(),
    )
    .await;

    assert!(warnings.is_empty(), "{warnings:?}");
}

#[tokio::test]
async fn only_udp_exposed_raises_no_warning() {
    let (_, warnings) = deploy(
        vec![run(&machine('1'), routed(8080), true)],
        exposing(&["53/udp"]),
    )
    .await;

    assert!(warnings.is_empty(), "{warnings:?}");
}

#[tokio::test]
async fn a_service_without_domains_raises_no_warning() {
    let mut web = routed(8080);
    web.ports.clear();
    let (_, warnings) = deploy(vec![run(&machine('1'), web, true)], exposing(&["80/tcp"])).await;

    assert!(warnings.is_empty(), "{warnings:?}");
}

#[tokio::test]
async fn two_machines_pulling_the_same_image_warn_once() {
    let (outcome, warnings) = deploy(
        vec![
            run(&machine('1'), routed(8080), true),
            run(&machine('2'), routed(8080), true),
        ],
        exposing(&["80/tcp"]),
    )
    .await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}

#[tokio::test]
async fn one_machine_whose_image_misses_the_domain_port_warns_with_its_ports_only() {
    let (first, second) = (machine('1'), machine('2'));
    let (outcome, warnings) = deploy_with(
        vec![
            run(&first, routed(8080), true),
            run(&second, routed(8080), true),
        ],
        [
            (first, exposing(&["80/tcp"])),
            (second, exposing(&["8080/tcp"])),
        ]
        .into(),
    )
    .await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    assert_eq!(
        warnings,
        [DeployWarning::ImagePortMismatch {
            service: ServiceName::parse("web").unwrap(),
            image: "nginx".into(),
            exposed: vec![80],
            routed: vec![8080],
        }]
    );
}
