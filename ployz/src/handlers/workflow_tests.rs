use std::{
    collections::{BTreeMap, VecDeque},
    fs,
};

use ployz_core::{
    AdvertisedEndpoint, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, Machine, MachineId, MachineName, MachineSubnet, ManagementAddress,
    MembershipObservation, PortPublication, ResolvedServiceSpec, WireGuardPublicKey,
};

use super::*;

struct Scripted {
    events: Vec<WorkflowStage>,
    snapshot: DeploySnapshot,
    push_failures: VecDeque<bool>,
    pushed: Vec<String>,
    confirmation_required: bool,
    confirmed: bool,
    confirmation_error: bool,
    executions: usize,
    execution_outcome: Option<DeployOutcome<ExecutionError>>,
}

impl Scripted {
    fn new(snapshot: DeploySnapshot) -> Self {
        Self {
            events: Vec::new(),
            snapshot,
            push_failures: VecDeque::new(),
            pushed: Vec::new(),
            confirmation_required: false,
            confirmed: true,
            confirmation_error: false,
            executions: 0,
            execution_outcome: None,
        }
    }
}

impl WorkflowIo for Scripted {
    fn record(&mut self, stage: WorkflowStage) {
        self.events.push(stage);
    }

    async fn machines(&mut self) -> Result<Vec<ployz_core::MachineObservation>, Error> {
        Ok(self.snapshot.machines.clone())
    }

    async fn push(
        &mut self,
        service: &BuildService,
        _machines: &[ployz_core::MachineObservation],
    ) -> Result<(), Error> {
        if self.push_failures.pop_front().unwrap_or(false) {
            Err(format!("{} failed", service.name).into())
        } else {
            self.pushed.push(service.name.clone());
            Ok(())
        }
    }

    async fn snapshot(
        &mut self,
        _machines: Option<Vec<ployz_core::MachineObservation>>,
    ) -> Result<DeploySnapshot, Error> {
        Ok(self.snapshot.clone())
    }

    fn render(&mut self, _operations: &[DeployOperation]) {}

    fn confirmation_required(&self) -> bool {
        self.confirmation_required
    }

    fn confirm(&mut self) -> Result<bool, Error> {
        if self.confirmation_error {
            Err("confirmation requires a terminal".into())
        } else {
            Ok(self.confirmed)
        }
    }

    async fn execute(&mut self, operations: &[DeployOperation]) -> DeployOutcome<ExecutionError> {
        self.executions += 1;
        self.execution_outcome
            .take()
            .unwrap_or_else(|| DeployOutcome {
                completed: operations.to_vec(),
                failed: None,
                unexecuted: Vec::new(),
            })
    }
}

#[test]
fn run_normalizes_supported_inputs_and_rejects_l4_ingress() {
    let command = crate::cli::command();
    let matches = command
        .try_get_matches_from([
            "ployz",
            "run",
            "--name",
            "api",
            "--env",
            "A=b",
            "--publish",
            "8080:80@host",
            "--volume",
            "data:/data:ro",
            "--volume",
            "cache:/cache:volume-nocopy",
            "alpine",
            "echo",
            "hello",
        ])
        .unwrap();
    let spec = run_spec(super::leaf_matches(&matches)).unwrap();
    assert_eq!(spec.name.as_str(), "api");
    assert_eq!(spec.container.command, ["echo", "hello"]);
    assert_eq!(
        spec.container.environment.get("A").map(String::as_str),
        Some("b")
    );
    assert!(matches!(
        spec.ports.first(),
        Some(PortPublication::Host { .. })
    ));
    assert!(spec.mounts.first().is_some_and(|mount| mount.read_only));
    assert!(matches!(
        spec.volumes.get(1).map(|volume| &volume.source),
        Some(ployz_core::VolumeSource::Named { no_copy: true, .. })
    ));

    let matches = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "run",
            "--volume",
            "/tmp:/data:volume-nocopy",
            "alpine",
        ])
        .unwrap();
    assert!(run_spec(super::leaf_matches(&matches)).is_err());

    let matches = crate::cli::command()
        .try_get_matches_from(["ployz", "run", "--publish", "8080:80", "alpine"])
        .unwrap();
    assert!(run_spec(super::leaf_matches(&matches)).is_err());

    let global = crate::cli::command()
        .try_get_matches_from(["ployz", "run", "--mode", "global", "alpine"])
        .unwrap();
    assert_eq!(
        run_spec(super::leaf_matches(&global)).unwrap().mode,
        ServiceMode::Global
    );
    let global_replicas = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "run",
            "--mode",
            "global",
            "--replicas",
            "2",
            "alpine",
        ])
        .unwrap();
    assert!(run_spec(super::leaf_matches(&global_replicas)).is_err());
}

#[test]
fn resolved_scale_input_changes_only_replicas() {
    let requested: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "alpine", "pull_policy": "missing" }
    }))
    .unwrap();
    let resolved = ResolvedServiceSpec {
        service_id: ServiceId::random(),
        name: requested.name.clone(),
        mode: requested.mode.clone(),
        container: requested.container.clone(),
        placement: requested.placement.clone(),
        ports: requested.ports.clone(),
        volumes: requested.volumes.clone(),
        mounts: requested.mounts.clone(),
        configs: requested.configs.clone(),
        pre_deploy: None,
        caddy_config: None,
        update: Default::default(),
    };
    let mut scaled = requested_from_resolved(&resolved);
    scaled.mode = ServiceMode::Replicated {
        replicas: NonZeroU32::new(3).unwrap(),
    };
    let mut expected = requested_from_resolved(&resolved);
    expected.mode = scaled.mode.clone();
    assert_eq!(scaled, expected);
}

#[tokio::test]
async fn run_forms_share_normalization_and_skip_confirmation() {
    let root = crate::cli::command()
        .try_get_matches_from(["ployz", "run", "--name", "api", "alpine"])
        .unwrap();
    let nested = crate::cli::command()
        .try_get_matches_from(["ployz", "service", "run", "--name", "api", "alpine"])
        .unwrap();
    let root = run_spec(super::leaf_matches(&root)).unwrap();
    let nested = run_spec(super::leaf_matches(&nested)).unwrap();
    assert_eq!(root, nested);

    for requested in [&root, &nested] {
        let mut io = Scripted::new(DeploySnapshot {
            machines: vec![machine()],
            ..Default::default()
        });
        run_connected(&mut io, requested).await.unwrap();
        assert_eq!(
            io.events,
            [
                WorkflowStage::Snapshot,
                WorkflowStage::Plan,
                WorkflowStage::Render,
                WorkflowStage::Execute,
            ]
        );
    }
}

#[tokio::test]
async fn deploy_attempts_every_push_and_stops_before_secrets_after_failure() {
    let mut project = crate::compose::parse_normalized(
        r#"
name: scripted
services:
  api: {image: api, environment: {TOKEN: secret://token}}
secrets: {token: {x-command: "printf resolved"}}
"#,
        ".",
    )
    .unwrap();
    let builds = [build("first"), build("second"), build("third")];
    let mut io = Scripted::new(DeploySnapshot {
        machines: vec![machine()],
        ..Default::default()
    });
    io.push_failures = [false, true, false].into();
    assert!(
        deploy_connected(&mut io, &mut project, &builds, PlanOptions::default())
            .await
            .is_err()
    );
    assert_eq!(
        io.events,
        [
            WorkflowStage::Push,
            WorkflowStage::Push,
            WorkflowStage::Push,
        ]
    );
    assert_eq!(io.pushed, ["first", "third"]);
    assert_eq!(
        project
            .services
            .get("api")
            .and_then(|service| service.container.environment.get("TOKEN"))
            .map(String::as_str),
        Some("secret://token")
    );
    assert_eq!(io.executions, 0);
}

#[tokio::test]
async fn deploy_sequence_confirmation_and_empty_plan_are_exact() {
    let root = std::env::temp_dir().join(format!("ployz-workflow-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let file = root.join("compose.yaml");
    fs::write(
        &file,
        "name: scripted\nservices: {api: {image: api, build: .}}\n",
    )
    .unwrap();
    let matches = crate::cli::command()
        .try_get_matches_from(["ployz", "deploy", "--file", file.to_str().unwrap(), "--yes"])
        .unwrap();
    let mut preparation = Vec::new();
    let (project, builds) = prepare_deploy(
        super::leaf_matches(&matches),
        |stage| preparation.push(stage),
        |_, _, _| Ok(()),
    )
    .unwrap();
    let expected = [
        WorkflowStage::Load,
        WorkflowStage::Select,
        WorkflowStage::Build,
        WorkflowStage::Push,
        WorkflowStage::ResolveSecrets,
        WorkflowStage::Snapshot,
        WorkflowStage::Plan,
        WorkflowStage::Render,
        WorkflowStage::Confirm,
        WorkflowStage::Execute,
    ];
    let mut confirmed = Scripted::new(DeploySnapshot {
        machines: vec![machine()],
        ..Default::default()
    });
    confirmed.events = preparation;
    confirmed.confirmation_required = true;
    deploy_connected(
        &mut confirmed,
        &mut project.clone(),
        &builds,
        PlanOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(confirmed.events, expected);
    assert_eq!(confirmed.executions, 1);

    let mut declined = Scripted::new(DeploySnapshot {
        machines: vec![machine()],
        ..Default::default()
    });
    declined.confirmation_required = true;
    declined.confirmed = false;
    deploy_connected(
        &mut declined,
        &mut project.clone(),
        &[],
        PlanOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(declined.events.last(), Some(&WorkflowStage::Confirm));
    assert_eq!(declined.executions, 0);

    let mut noninteractive = Scripted::new(DeploySnapshot {
        machines: vec![machine()],
        ..Default::default()
    });
    noninteractive.confirmation_required = true;
    noninteractive.confirmation_error = true;
    assert!(
        deploy_connected(
            &mut noninteractive,
            &mut project.clone(),
            &[],
            PlanOptions::default(),
        )
        .await
        .is_err()
    );
    assert_eq!(noninteractive.executions, 0);

    let mut automatic = Scripted::new(DeploySnapshot {
        machines: vec![machine()],
        ..Default::default()
    });
    let outcome = deploy_connected(
        &mut automatic,
        &mut project.clone(),
        &[],
        PlanOptions::default(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!automatic.events.contains(&WorkflowStage::Confirm));
    assert_eq!(automatic.executions, 1);
    assert!(!outcome.completed.is_empty());
    assert!(outcome.failed.is_none());
    assert!(outcome.unexecuted.is_empty());

    let mut empty = Scripted::new(DeploySnapshot::default());
    let mut empty_project =
        crate::compose::parse_normalized("name: empty\nservices: {}\n", ".").unwrap();
    deploy_connected(&mut empty, &mut empty_project, &[], PlanOptions::default())
        .await
        .unwrap();
    assert_eq!(
        empty.events,
        [
            WorkflowStage::ResolveSecrets,
            WorkflowStage::Snapshot,
            WorkflowStage::Plan,
        ]
    );
    assert_eq!(empty.executions, 0);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn scale_rejects_global_noops_matching_and_uses_one_mixed_spec() {
    let service_id = ServiceId::random();
    let global = observation(&service_id, ServiceMode::Global, "v1", '1');
    let mut io = Scripted::new(DeploySnapshot {
        machines: vec![machine()],
        containers: vec![global],
        ..Default::default()
    });
    assert!(
        scale_connected(&mut io, "api", NonZeroU32::new(2).unwrap())
            .await
            .is_err()
    );
    assert_eq!(io.events, [WorkflowStage::Snapshot]);

    let replicated = ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    };
    let mut unchanged = Scripted::new(DeploySnapshot {
        machines: vec![machine()],
        containers: vec![observation(&service_id, replicated.clone(), "v1", '1')],
        ..Default::default()
    });
    scale_connected(&mut unchanged, "api", NonZeroU32::new(1).unwrap())
        .await
        .unwrap();
    assert_eq!(unchanged.events, [WorkflowStage::Snapshot]);

    let mut incomplete = Scripted::new(DeploySnapshot {
        machines: vec![machine()],
        containers: vec![observation(
            &service_id,
            ServiceMode::Replicated {
                replicas: NonZeroU32::new(3).unwrap(),
            },
            "v1",
            '1',
        )],
        ..Default::default()
    });
    scale_connected(&mut incomplete, "api", NonZeroU32::new(3).unwrap())
        .await
        .unwrap();
    assert_eq!(
        incomplete.events,
        [
            WorkflowStage::Snapshot,
            WorkflowStage::Plan,
            WorkflowStage::Render,
            WorkflowStage::Execute,
        ]
    );

    let mut mixed = Scripted::new(DeploySnapshot {
        machines: vec![machine()],
        containers: vec![
            observation(&service_id, replicated.clone(), "v1", '1'),
            observation(&service_id, replicated, "v2", '2'),
        ],
        ..Default::default()
    });
    scale_connected(&mut mixed, "api", NonZeroU32::new(3).unwrap())
        .await
        .unwrap();
    assert_eq!(
        mixed.events,
        [
            WorkflowStage::Snapshot,
            WorkflowStage::Plan,
            WorkflowStage::Render,
            WorkflowStage::Execute,
        ]
    );
    assert_eq!(mixed.executions, 1);
}

#[tokio::test]
async fn deploy_preserves_the_exact_execution_outcome() {
    let mut project =
        crate::compose::parse_normalized("name: scripted\nservices: {api: {image: api}}\n", ".")
            .unwrap();
    let expected = DeployOutcome {
        completed: vec![stop_operation('1')],
        failed: Some(FailedOperation::Operation {
            operation: stop_operation('2'),
            error: ExecutionError::Machine {
                action: crate::deploy::MachineAction::StopContainer,
                error: ployz_core::RpcError {
                    code: ployz_core::RpcErrorCode::Unavailable,
                    message: "offline".into(),
                    details: serde_json::Value::Null,
                },
            },
        }),
        unexecuted: vec![stop_operation('3')],
    };
    let mut io = Scripted::new(DeploySnapshot {
        machines: vec![machine()],
        ..Default::default()
    });
    io.execution_outcome = Some(expected.clone());

    let actual = deploy_connected(&mut io, &mut project, &[], PlanOptions::default())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(actual, expected);
}

fn build(name: &str) -> BuildService {
    BuildService {
        name: name.into(),
        image: name.into(),
        build: serde_norway::Value::Null,
        machines: Vec::new(),
    }
}

fn machine() -> ployz_core::MachineObservation {
    ployz_core::MachineObservation {
        machine: Machine {
            id: MachineId::parse("a".repeat(32)).unwrap(),
            name: MachineName::parse("machine-1").unwrap(),
            subnet: MachineSubnet("10.210.1.0/24".parse().unwrap()),
            management_address: ManagementAddress("::1".parse().unwrap()),
            public_key: WireGuardPublicKey([1; 32]),
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
        },
        membership: MembershipObservation::Up,
        selected_endpoint: None,
    }
}

fn stop_operation(id: char) -> DeployOperation {
    DeployOperation::StopContainer {
        machine_id: machine().machine.id,
        container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
    }
}

fn observation(
    service_id: &ServiceId,
    mode: ServiceMode,
    image: &str,
    id: char,
) -> ContainerObservation {
    let requested: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "api",
        "mode": mode,
        "container": { "image": image, "pull_policy": "missing" }
    }))
    .unwrap();
    let resolved = ResolvedServiceSpec {
        service_id: service_id.clone(),
        name: requested.name.clone(),
        mode: requested.mode,
        container: requested.container,
        placement: requested.placement,
        ports: requested.ports,
        volumes: requested.volumes,
        mounts: requested.mounts,
        configs: requested.configs,
        pre_deploy: None,
        caddy_config: None,
        update: Default::default(),
    };
    ContainerObservation {
        container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        display_name: format!("api-{id}"),
        created_at_unix_nanos: 0,
        machine_id: machine().machine.id,
        service_id: service_id.clone(),
        service_name: requested.name,
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: ployz_core::HealthObservation::NotConfigured,
        },
        effective_healthcheck: None,
        resolved_spec: resolved,
        address: None,
        labels: BTreeMap::new(),
    }
}
