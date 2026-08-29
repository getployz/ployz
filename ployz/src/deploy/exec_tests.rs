use std::{collections::VecDeque, sync::Mutex};

use ployz_core::{
    ContainerRuntimeObservation, DependencyHealthFailure, DockerVolumeId, DockerVolumeName,
    HealthFailure, HealthObservation, MembershipObservation, ProjectName, RpcErrorCode,
    ServiceName,
};

use crate::deploy::{DeployOutcome, DeploySnapshot, FailedOperation};

use super::health::parse_monitor_period;
use super::*;

fn test_project() -> ProjectName {
    ProjectName::parse("app").unwrap()
}

#[test]
fn dependency_health_requires_only_rpc_eligible_observations() {
    assert!(omission_requires_observation(None));
    for membership in [MembershipObservation::Up, MembershipObservation::Suspect] {
        assert!(omission_requires_observation(Some(&membership)));
    }
    for membership in [MembershipObservation::Down, MembershipObservation::Unknown] {
        assert!(!omission_requires_observation(Some(&membership)));
    }
}

async fn execute_with<C: MachineOperations>(
    plan: &[DeployOperation],
    client: &C,
    cancellation: &CancellationToken,
) -> DeployOutcome<ExecutionError> {
    super::execute_operation_sequence(
        crate::deploy::pending_rows(plan, &DeploySnapshot::default()),
        client,
        cancellation,
        None,
        &test_project(),
    )
    .await
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Call {
    Wait(Vec<ContainerId>, ContainerObservationCondition),
    List(QualifiedService),
    Create(MachineId, ContainerKind),
    Start(MachineId, ContainerId),
    Inspect(MachineId, ContainerId),
    Stop(MachineId, ContainerId),
    StopWithGrace(MachineId, ContainerId, i32),
    Remove(MachineId, ContainerId),
    RemoveVolume(DockerVolumeId),
}

#[derive(Clone)]
enum Reply {
    Ok,
    Listed(Vec<ContainerObservation>),
    Created(ContainerId),
    CreatedLater(ContainerId),
    Observed(
        ContainerRuntimeObservation,
        Option<ployz_core::HealthcheckSpec>,
    ),
    Pending,
    Error(RpcError),
}

struct Step(Call, Reply);

struct Scripted {
    steps: Mutex<VecDeque<Step>>,
}

impl Scripted {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
        }
    }

    fn next(&self, call: Call) -> Reply {
        let Step(expected, reply) = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected call: {call:?}"));
        assert_eq!(call, expected);
        reply
    }

    fn assert_done(&self) {
        assert!(self.steps.lock().unwrap().is_empty());
    }
}

impl MachineOperations for Scripted {
    async fn wait_for_container_observations(
        &self,
        container_ids: &[ContainerId],
        condition: ContainerObservationCondition,
        _cancellation: &CancellationToken,
    ) -> Result<(), RpcError> {
        unit(self.next(Call::Wait(container_ids.to_vec(), condition)))
    }

    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        match self.next(Call::List(service.clone())) {
            Reply::Listed(containers) => Ok(containers),
            Reply::Error(error) => Err(error),
            Reply::Ok
            | Reply::Created(_)
            | Reply::CreatedLater(_)
            | Reply::Observed(_, _)
            | Reply::Pending => {
                panic!("scripted list requires Listed or Error")
            }
        }
    }

    async fn create_container(
        &self,
        machine_id: &MachineId,
        kind: ContainerKind,
        _project_name: &ProjectName,
        _spec: &ResolvedServiceSpec,
    ) -> Result<ContainerCreated, RpcError> {
        match self.next(Call::Create(*machine_id, kind)) {
            Reply::Created(container_id) => Ok(ContainerCreated {
                display_name: container_id.to_string(),
                container_id,
            }),
            Reply::CreatedLater(container_id) => {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                Ok(ContainerCreated {
                    display_name: container_id.to_string(),
                    container_id,
                })
            }
            Reply::Error(error) => Err(error),
            Reply::Pending => std::future::pending().await,
            Reply::Ok | Reply::Listed(_) | Reply::Observed(_, _) => {
                panic!("scripted create requires Created or Error")
            }
        }
    }

    async fn start_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError> {
        unit(self.next(Call::Start(*machine_id, *container_id)))
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerObservation, RpcError> {
        match self.next(Call::Inspect(*machine_id, *container_id)) {
            Reply::Observed(runtime, healthcheck) => {
                let mut observation = observation(machine_id, container_id, runtime);
                observation.effective_healthcheck = healthcheck;
                Ok(observation)
            }
            Reply::Pending => std::future::pending().await,
            Reply::Error(error) => Err(error),
            Reply::Ok | Reply::Listed(_) | Reply::Created(_) | Reply::CreatedLater(_) => {
                panic!("scripted inspect requires Observed or Error")
            }
        }
    }

    async fn stop_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        grace_period_seconds: Option<i32>,
    ) -> Result<(), RpcError> {
        let call = grace_period_seconds.map_or_else(
            || Call::Stop(*machine_id, *container_id),
            |grace| Call::StopWithGrace(*machine_id, *container_id, grace),
        );
        unit(self.next(call))
    }

    async fn remove_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError> {
        unit(self.next(Call::Remove(*machine_id, *container_id)))
    }

    async fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError> {
        unit(self.next(Call::RemoveVolume(id.clone())))
    }
}

fn unit(reply: Reply) -> Result<(), RpcError> {
    match reply {
        Reply::Ok => Ok(()),
        Reply::Error(error) => Err(error),
        Reply::Listed(_)
        | Reply::Created(_)
        | Reply::CreatedLater(_)
        | Reply::Observed(_, _)
        | Reply::Pending => {
            panic!("scripted mutation requires Ok or Error")
        }
    }
}

fn serving(container_id: ContainerId) -> Step {
    ok(Call::Wait(
        vec![container_id],
        ContainerObservationCondition::Serving,
    ))
}

fn dropped(container_id: ContainerId) -> Step {
    ok(Call::Wait(
        vec![container_id],
        ContainerObservationCondition::Dropped,
    ))
}

#[path = "exec_tests/dispatch.rs"]
mod dispatch;
#[path = "exec_tests/health.rs"]
mod health;
#[path = "exec_tests/hooks.rs"]
mod hooks;
#[path = "exec_tests/replacement.rs"]
mod replacement;
#[path = "exec_tests/restart.rs"]
mod restart;

fn run(
    machine_id: &MachineId,
    spec: ResolvedServiceSpec,
    skip_health_monitor: bool,
) -> DeployOperation {
    DeployOperation::RunContainer {
        machine_id: *machine_id,
        spec,
        skip_health_monitor,
    }
}

fn replacement(
    machine_id: &MachineId,
    old_container_id: &ContainerId,
    order: UpdateOrder,
) -> DeployOperation {
    let mut spec = spec(Some(0), Some(healthcheck()), None);
    spec.update.order = order;
    DeployOperation::ReplaceContainer(ReplacementOperation {
        machine_id: *machine_id,
        old_container_id: *old_container_id,
        spec,
        skip_health_monitor: false,
    })
}

fn hook(machine_id: &MachineId, spec: ResolvedServiceSpec) -> DeployOperation {
    DeployOperation::RunHook {
        machine_id: *machine_id,
        spec,
        old_hook_containers: Vec::new(),
    }
}

fn stop(machine_id: &MachineId, container_id: &ContainerId) -> DeployOperation {
    DeployOperation::StopContainer {
        machine_id: *machine_id,
        container_id: *container_id,
        purpose: ployz_core::StopContainerPurpose::Lifecycle,
    }
}

fn spec(
    monitor_millis: Option<u64>,
    healthcheck: Option<ployz_core::HealthcheckSpec>,
    hook_timeout_millis: Option<u64>,
) -> ResolvedServiceSpec {
    let mut spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": service_id(),
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "pull_policy": "missing",
            "healthcheck": healthcheck,
        },
        "update": {
            "order": "start_first",
            "monitor_millis": monitor_millis,
        }
    }))
    .unwrap();
    spec.pre_deploy = hook_timeout_millis.map(|timeout_millis| ployz_core::PreDeployHook {
        command: vec!["true".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: Some(timeout_millis),
        user: None,
    });
    spec
}

fn configured_healthcheck() -> ployz_core::ConfiguredHealthcheck {
    ployz_core::ConfiguredHealthcheck {
        test: ployz_core::HealthcheckCommand::parse(["CMD", "true"]).unwrap(),
        interval_millis: Some(1_000),
        timeout_millis: Some(1_000),
        start_period_millis: None,
        start_interval_millis: None,
        retries: Some(1),
    }
}

fn healthcheck() -> ployz_core::HealthcheckSpec {
    ployz_core::HealthcheckSpec::Configured(configured_healthcheck())
}

fn observation(
    machine_id: &MachineId,
    container_id: &ContainerId,
    runtime: ContainerRuntimeObservation,
) -> ContainerObservation {
    let spec = spec(None, None, None);
    ContainerObservation {
        container_id: *container_id,
        display_name: container_id.to_string(),
        created_at_unix_nanos: 0,
        machine_id: *machine_id,
        project_name: test_project(),
        service_id: spec.service_id,
        service_name: spec.name.clone(),
        kind: ContainerKind::ServiceContainer,
        runtime,
        effective_healthcheck: None,
        resolved_spec: spec,
        address: None,
        labels: Default::default(),
    }
}

fn machine(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

fn container(hex: char) -> ContainerId {
    ContainerId::parse(hex.to_string().repeat(64)).unwrap()
}

fn service_id() -> ployz_core::ServiceId {
    ployz_core::ServiceId::parse("f".repeat(32)).unwrap()
}

fn running() -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running {
        health: HealthObservation::NotConfigured,
    }
}

fn healthy() -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running {
        health: HealthObservation::Healthy,
    }
}

fn starting() -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running {
        health: HealthObservation::Starting,
    }
}

fn unhealthy() -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running {
        health: HealthObservation::Unhealthy,
    }
}

fn exited(code: i64) -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Exited { code }
}

fn error(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Internal,
        message: message.into(),
        details: serde_json::Value::Null,
    }
}

fn ok(call: Call) -> Step {
    Step(call, Reply::Ok)
}

fn created(call: Call, container_id: &ContainerId) -> Step {
    Step(call, Reply::Created(*container_id))
}

fn created_later(call: Call, container_id: &ContainerId) -> Step {
    Step(call, Reply::CreatedLater(*container_id))
}

fn observed(call: Call, runtime: ContainerRuntimeObservation) -> Step {
    Step(call, Reply::Observed(runtime, None))
}

fn observed_with_healthcheck(
    call: Call,
    runtime: ContainerRuntimeObservation,
    healthcheck: ployz_core::HealthcheckSpec,
) -> Step {
    Step(call, Reply::Observed(runtime, Some(healthcheck)))
}

fn listed(service: &QualifiedService, containers: Vec<ContainerObservation>) -> Step {
    Step(Call::List(service.clone()), Reply::Listed(containers))
}

fn failed(call: Call, message: &str) -> Step {
    Step(call, Reply::Error(error(message)))
}

fn unavailable(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: message.into(),
        details: serde_json::Value::Null,
    }
}

fn failed_unavailable(call: Call, message: &str) -> Step {
    Step(call, Reply::Error(unavailable(message)))
}
