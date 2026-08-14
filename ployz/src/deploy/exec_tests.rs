use std::{collections::VecDeque, sync::Mutex};

use ployz_core::{
    ContainerRuntimeObservation, DockerVolumeName, HealthObservation, ServiceVolumeReference,
    VolumeSource,
};

use crate::deploy::FailedOperation;

use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
enum Call {
    CreateVolume(MachineId),
    Create(MachineId, ContainerKind),
    Start(MachineId, ContainerId),
    Inspect(MachineId, ContainerId),
    Stop(MachineId, ContainerId),
    StopWithGrace(MachineId, ContainerId, i32),
    Remove(MachineId, ContainerId),
}

#[derive(Clone)]
enum Reply {
    Ok,
    Created(ContainerId),
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
    async fn create_volume(
        &self,
        machine_id: &MachineId,
        _volume: &ServiceVolume,
    ) -> Result<(), RpcError> {
        unit(self.next(Call::CreateVolume(machine_id.clone())))
    }

    async fn create_container(
        &self,
        machine_id: &MachineId,
        kind: ContainerKind,
        _spec: &ResolvedServiceSpec,
    ) -> Result<ContainerCreated, RpcError> {
        match self.next(Call::Create(machine_id.clone(), kind)) {
            Reply::Created(container_id) => Ok(ContainerCreated {
                display_name: container_id.to_string(),
                container_id,
            }),
            Reply::Error(error) => Err(error),
            Reply::Ok | Reply::Observed(_, _) | Reply::Pending => {
                panic!("scripted create requires Created or Error")
            }
        }
    }

    async fn start_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError> {
        unit(self.next(Call::Start(machine_id.clone(), container_id.clone())))
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerObservation, RpcError> {
        match self.next(Call::Inspect(machine_id.clone(), container_id.clone())) {
            Reply::Observed(runtime, healthcheck) => {
                let mut observation = observation(machine_id, container_id, runtime);
                observation.effective_healthcheck = healthcheck;
                Ok(observation)
            }
            Reply::Pending => std::future::pending().await,
            Reply::Error(error) => Err(error),
            Reply::Ok | Reply::Created(_) => {
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
            || Call::Stop(machine_id.clone(), container_id.clone()),
            |grace| Call::StopWithGrace(machine_id.clone(), container_id.clone(), grace),
        );
        unit(self.next(call))
    }

    async fn remove_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError> {
        unit(self.next(Call::Remove(machine_id.clone(), container_id.clone())))
    }
}

fn unit(reply: Reply) -> Result<(), RpcError> {
    match reply {
        Reply::Ok => Ok(()),
        Reply::Error(error) => Err(error),
        Reply::Created(_) | Reply::Observed(_, _) | Reply::Pending => {
            panic!("scripted mutation requires Ok or Error")
        }
    }
}

#[path = "exec_tests/dispatch.rs"]
mod dispatch;
#[path = "exec_tests/health.rs"]
mod health;
#[path = "exec_tests/hooks.rs"]
mod hooks;
#[path = "exec_tests/replacement.rs"]
mod replacement;

fn plan(operations: Vec<DeployOperation>) -> DeployPlan {
    DeployPlan {
        service_id: service_id(),
        is_new_service: true,
        operation: DeployOperation::Sequence { operations },
    }
}

fn run(
    machine_id: &MachineId,
    spec: ResolvedServiceSpec,
    skip_health_monitor: bool,
) -> DeployOperation {
    DeployOperation::RunContainer {
        machine_id: machine_id.clone(),
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
        machine_id: machine_id.clone(),
        old_container_id: old_container_id.clone(),
        spec,
        skip_health_monitor: false,
    })
}

fn hook(machine_id: &MachineId, spec: ResolvedServiceSpec) -> DeployOperation {
    DeployOperation::RunHook {
        machine_id: machine_id.clone(),
        spec,
        old_hook_containers: Vec::new(),
    }
}

fn stop(machine_id: &MachineId, container_id: &ContainerId) -> DeployOperation {
    DeployOperation::StopContainer {
        machine_id: machine_id.clone(),
        container_id: container_id.clone(),
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

fn healthcheck() -> ployz_core::HealthcheckSpec {
    ployz_core::HealthcheckSpec {
        test: vec!["CMD".into(), "true".into()],
        interval_millis: Some(1_000),
        timeout_millis: Some(1_000),
        start_period_millis: None,
        start_interval_millis: None,
        retries: Some(1),
        disabled: false,
    }
}

fn volume() -> ServiceVolume {
    ServiceVolume {
        reference: ServiceVolumeReference::parse("data").unwrap(),
        source: VolumeSource::Named {
            name: DockerVolumeName::parse("data").unwrap(),
            external: false,
            driver: None,
            labels: Default::default(),
            no_copy: false,
            subpath: None,
        },
    }
}

fn observation(
    machine_id: &MachineId,
    container_id: &ContainerId,
    runtime: ContainerRuntimeObservation,
) -> ContainerObservation {
    let spec = spec(None, None, None);
    ContainerObservation {
        container_id: container_id.clone(),
        display_name: container_id.to_string(),
        machine_id: machine_id.clone(),
        service_id: spec.service_id.clone(),
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
    Step(call, Reply::Created(container_id.clone()))
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

fn failed(call: Call, message: &str) -> Step {
    Step(call, Reply::Error(error(message)))
}
