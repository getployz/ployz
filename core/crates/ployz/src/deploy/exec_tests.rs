use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Mutex,
};

use ployz_core::{
    ContainerRuntimeObservation, DependencyHealthFailure, DockerVolumeId, DockerVolumeName,
    HealthFailure, HealthObservation, LastHealthCheck, MembershipObservation, Namespace,
    RpcErrorCode, ServiceName, ServiceResult, UpdateOrder,
};

use crate::deploy::{DeployOutcome, FailedOperation};

use super::*;

fn test_namespace() -> Namespace {
    Namespace::parse("app").unwrap()
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
        &crate::deploy::DeployPlan::for_execution_test(plan.to_vec(), test_namespace()),
        &run_id(),
        client,
        cancellation,
        None,
    )
    .await
}

/// Where each named Service ended, in plan order.
fn ended(results: &[(&str, ServiceResult)]) -> Vec<ServiceOutcome> {
    results
        .iter()
        .map(|(name, result)| ServiceOutcome {
            service: ServiceName::parse(*name).unwrap(),
            result: *result,
        })
        .collect()
}

fn api() -> QualifiedService {
    QualifiedService::new(test_namespace(), ServiceName::parse("api").unwrap())
}

fn run_id() -> DeploymentLogId {
    DeploymentLogId::parse("run").unwrap()
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Call {
    Prepare(MachineId),
    List(QualifiedService),
    /// Running a new Container of this kind.
    Run(MachineId, ContainerKind),
    /// Running an existing Container.
    Restart(MachineId, ContainerId),
    Inspect(MachineId, ContainerId),
    /// Stopped behind the default drain.
    Stop(MachineId, ContainerId),
    /// Stopped with no drain.
    StopNow(MachineId, ContainerId),
    /// Stopped with no drain and no grace.
    Kill(MachineId, ContainerId),
    /// Gone behind the default drain.
    Remove(MachineId, ContainerId),
    /// Gone with no drain.
    Discard(MachineId, ContainerId),
    /// Gone with no drain, by the slot of a new Container of this kind.
    DiscardSlot(MachineId, ContainerKind),
    RemoveVolume(DockerVolumeId),
}

#[derive(Clone)]
enum Reply {
    Ok,
    Listed(Vec<ContainerObservation>),
    Created(ContainerId),
    Settled(ContainerId, HealthVerdict),
    CreatedLater(ContainerId),
    CreatedAfter(ContainerId, std::time::Duration),
    Observed(
        ContainerRuntimeObservation,
        Option<ployz_core::HealthcheckSpec>,
        Option<LastHealthCheck>,
    ),
    ObservedIngress(ContainerRuntimeObservation),
    Pending,
    Error(RpcError),
}

struct Step(Call, Reply);

struct AmendmentStall {
    requested: tokio::sync::Notify,
    released: tokio::sync::Notify,
    spent: std::sync::atomic::AtomicBool,
}

impl AmendmentStall {
    fn new() -> Self {
        Self {
            requested: tokio::sync::Notify::new(),
            released: tokio::sync::Notify::new(),
            spent: std::sync::atomic::AtomicBool::new(false),
        }
    }

    async fn hold_amendment(&self) {
        self.requested.notify_one();
        self.released.notified().await;
    }

    async fn release_on_first_start(&self) {
        if self.spent.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        self.requested.notified().await;
        self.released.notify_one();
    }
}

struct Scripted {
    steps: Mutex<VecDeque<Step>>,
    amendment_stall: Option<AmendmentStall>,
    observations: Option<Vec<ContainerObservation>>,
    slots: Mutex<Vec<SlotKey>>,
    answers: Mutex<Vec<AnswerWhen>>,
    deploys: Mutex<Vec<Option<ployz_core::DeploymentLogId>>>,
    cancel_on_prepare: Option<CancellationToken>,
    /// This call cancels the Deploy once it is made.
    cancel_on: Option<(Call, CancellationToken)>,
    runs_global: bool,
    pull_error: Option<RpcError>,
    pulled: Mutex<BTreeSet<MachineId>>,
    /// No Machine acks this run's pointer of this Service.
    unrouted: Option<ServiceName>,
    /// This Machine never acks this run's pointers.
    unrouted_machine: Option<MachineId>,
    superseded: Option<ServiceName>,
    /// The row lands but the reply never arrives.
    lost_reply: bool,
    unreadable_flip: bool,
    /// The write fails in transit and never lands.
    dropped_publish: bool,
    /// A write that holds copies lands but its reply never arrives.
    lost_hold_reply: bool,
    /// This many writes that release a held copy fail in transit.
    dropped_releases: Mutex<usize>,
    /// No Machine routes a pin.
    unrouted_pin: bool,
    /// No Machine routes a row naming one of these Containers.
    dead: Vec<ContainerId>,
    /// Another Deploy writes this row just before this run's pin.
    rival: Option<ServingPointer>,
    pointers: Mutex<BTreeMap<QualifiedService, ServingPointer>>,
    publications: Mutex<Vec<PublishServingPointerRequest>>,
    /// Each drained removal with the number of flips before it.
    removed_after: Mutex<Vec<(ContainerId, usize)>>,
}

impl Scripted {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            amendment_stall: None,
            observations: None,
            slots: Mutex::new(Vec::new()),
            answers: Mutex::new(Vec::new()),
            deploys: Mutex::new(Vec::new()),
            cancel_on_prepare: None,
            cancel_on: None,
            runs_global: false,
            pull_error: None,
            pulled: Mutex::new(BTreeSet::new()),
            unrouted: None,
            unrouted_machine: None,
            superseded: None,
            lost_reply: false,
            unreadable_flip: false,
            dropped_publish: false,
            lost_hold_reply: false,
            dropped_releases: Mutex::new(0),
            unrouted_pin: false,
            dead: Vec::new(),
            rival: None,
            pointers: Mutex::new(BTreeMap::new()),
            publications: Mutex::new(Vec::new()),
            removed_after: Mutex::new(Vec::new()),
        }
    }

    /// This run's flips, without the pins written before any start.
    fn publications(&self) -> Vec<PublishServingPointerRequest> {
        self.writes(true)
    }

    fn pins(&self) -> Vec<PublishServingPointerRequest> {
        self.writes(false)
    }

    fn writes(&self, flips: bool) -> Vec<PublishServingPointerRequest> {
        self.publications
            .lock()
            .unwrap()
            .iter()
            .filter(|write| (write.pointer.deploy == run_id()) == flips)
            .cloned()
            .collect()
    }

    fn answers(&self) -> Vec<AnswerWhen> {
        self.answers.lock().unwrap().clone()
    }

    fn next(&self, call: Call) -> Reply {
        if let Call::Remove(_, container) = call {
            let writes = self.publications().len();
            self.removed_after.lock().unwrap().push((container, writes));
        }
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
    async fn prepare_volumes(
        &self,
        machine_id: &MachineId,
        _specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<ployz_core::PreparedVolumes, RpcError> {
        unit(self.next(Call::Prepare(*machine_id)))?;
        if let Some(cancel) = &self.cancel_on_prepare {
            cancel.cancel();
        }
        Ok(ployz_core::PreparedVolumes { names: Vec::new() })
    }
    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        if let Some(observations) = &self.observations {
            return Ok(observations.clone());
        }
        let scripted = matches!(
            self.steps.lock().unwrap().front(),
            Some(Step(Call::List(listed), _)) if listed == service
        );
        if !scripted {
            return Ok(Vec::new());
        }
        match self.next(Call::List(service.clone())) {
            Reply::Listed(containers) => Ok(containers),
            Reply::Error(error) => Err(error),
            Reply::Ok
            | Reply::Created(_)
            | Reply::Settled(..)
            | Reply::CreatedLater(_)
            | Reply::CreatedAfter(..)
            | Reply::Observed(..)
            | Reply::ObservedIngress(_)
            | Reply::Pending => {
                panic!("scripted list requires Listed or Error")
            }
        }
    }

    async fn ensure_container(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
    ) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError> {
        let machine = *machine_id;
        let mut settled = false;
        let (call, of) = match want {
            ContainerWant::Running {
                container: RunTarget::New(new),
            } => {
                let call = Call::Run(machine, new.slot.kind);
                settled = new.answer == AnswerWhen::Settled;
                self.answers.lock().unwrap().push(new.answer);
                self.deploys.lock().unwrap().push(new.deployment_id);
                self.slots.lock().unwrap().push(new.slot.clone());
                (call, ContainerLocator::Key(new.slot))
            }
            ContainerWant::Running {
                container: RunTarget::Existing(locator),
            } => (Call::Restart(machine, id_of(&locator)), locator),
            ContainerWant::Stopped {
                container,
                drain,
                stop,
            } => {
                let id = id_of(&container);
                let call = match (drain, stop.grace_period_seconds) {
                    (Drain::DEFAULT, None) => Call::Stop(machine, id),
                    (Drain::NONE, None) => Call::StopNow(machine, id),
                    (Drain::NONE, Some(0)) => Call::Kill(machine, id),
                    other => panic!("unscripted stop {other:?}"),
                };
                (call, container)
            }
            ContainerWant::Gone {
                container, drain, ..
            } => {
                let call = match (&container, drain) {
                    (ContainerLocator::Id(id), Drain::DEFAULT) => Call::Remove(machine, *id),
                    (ContainerLocator::Id(id), Drain::NONE) => Call::Discard(machine, *id),
                    (ContainerLocator::Key(slot), Drain::NONE) => {
                        Call::DiscardSlot(machine, slot.kind)
                    }
                    other => panic!("unscripted removal {other:?}"),
                };
                (call, container)
            }
        };
        if let (Call::Run(..), Some(stall)) = (&call, &self.amendment_stall) {
            stall.release_on_first_start().await;
        }
        if let Some((at, cancel)) = &self.cancel_on
            && *at == call
        {
            cancel.cancel();
        }
        let healthy = settled.then_some(HealthVerdict::Healthy);
        let present = |container_id: &ContainerId| {
            ContainerNow::Present(observation(machine_id, container_id, running()))
        };
        match self.next(call) {
            Reply::Created(container_id) => Ok((present(&container_id), healthy)),
            Reply::Settled(container_id, verdict) => {
                assert!(settled, "only a Settled creation answers a verdict");
                Ok((present(&container_id), Some(verdict)))
            }
            Reply::CreatedLater(container_id) => {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                Ok((present(&container_id), healthy))
            }
            Reply::CreatedAfter(container_id, delay) => {
                tokio::time::sleep(delay).await;
                Ok((present(&container_id), healthy))
            }
            Reply::Ok => Ok((ContainerNow::Absent { of }, None)),
            Reply::Error(error) => Err(error),
            Reply::Pending => std::future::pending().await,
            Reply::Listed(_) | Reply::Observed(..) | Reply::ObservedIngress(_) => {
                panic!("scripted ensure requires Created, Ok or Error")
            }
        }
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerDetails, RpcError> {
        match self.next(Call::Inspect(*machine_id, *container_id)) {
            Reply::Observed(runtime, healthcheck, last_check) => {
                let mut container = observation(machine_id, container_id, runtime);
                container
                    .try_update(|parts| parts.effective_healthcheck = healthcheck)
                    .unwrap();
                Ok(ContainerDetails {
                    container,
                    environment: None,
                    image_id: None,
                    last_check,
                    run: None,
                })
            }
            Reply::ObservedIngress(runtime) => {
                let mut container = observation(machine_id, container_id, runtime);
                container
                    .try_update(|parts| {
                        parts.namespace = Namespace::system();
                        parts.resolved_spec.name = ServiceName::parse("ingress").unwrap();
                    })
                    .unwrap();
                Ok(ContainerDetails {
                    container,
                    environment: None,
                    image_id: None,
                    last_check: None,
                    run: None,
                })
            }
            Reply::Pending => std::future::pending().await,
            Reply::Error(error) => Err(error),
            Reply::Ok
            | Reply::Listed(_)
            | Reply::Created(_)
            | Reply::Settled(..)
            | Reply::CreatedLater(_)
            | Reply::CreatedAfter(..) => {
                panic!("scripted inspect requires Observed or Error")
            }
        }
    }

    async fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError> {
        unit(self.next(Call::RemoveVolume(id.clone())))
    }

    async fn runs_global(
        &self,
        _machine_id: &MachineId,
        _slot: &SlotKey,
        _spec: &ResolvedServiceSpec,
    ) -> Result<bool, RpcError> {
        Ok(self.runs_global)
    }

    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        _spec: &ResolvedServiceSpec,
    ) -> Result<(), RpcError> {
        self.pulled.lock().unwrap().insert(*machine_id);
        self.pull_error.clone().map_or(Ok(()), Err)
    }

    async fn read_pointer(
        &self,
        service: &QualifiedService,
    ) -> Result<ServingPointerRead, RpcError> {
        let pointer = self.pointers.lock().unwrap().get(service).cloned();
        if self.unreadable_flip && pointer.as_ref().is_some_and(|row| row.deploy == run_id()) {
            return Err(error("read timed out"));
        }
        // Lagging ingress routes every row up to this run's, never this run's.
        let ours = pointer
            .as_ref()
            .is_some_and(|pointer| pointer.deploy == run_id());
        let routed = if ours {
            self.unrouted.as_ref() != Some(&service.name)
        } else {
            !self.unrouted_pin
        } && pointer
            .as_ref()
            .is_none_or(|pointer| !pointer.containers.iter().any(|id| self.dead.contains(id)));
        let lagging = ours.then_some(self.unrouted_machine).flatten();
        let acks = match &pointer {
            Some(pointer) if routed => self
                .pulled
                .lock()
                .unwrap()
                .iter()
                .filter(|machine| lagging.as_ref() != Some(*machine))
                .map(|machine| (*machine, pointer.deploy.clone()))
                .collect(),
            _ => BTreeMap::new(),
        };
        let all_routed = pointer.is_some() && routed && lagging.is_none();
        Ok(ServingPointerRead {
            pointer,
            acks,
            all_routed,
        })
    }

    async fn publish_pointer(
        &self,
        request: PublishServingPointerRequest,
    ) -> Result<ServingPointerPublication, RpcError> {
        self.publications.lock().unwrap().push(request.clone());
        let flip = request.pointer.deploy == run_id();
        if let Some(stall) = &self.amendment_stall
            && !flip
            && !request.pointer.retiring.is_empty()
        {
            stall.hold_amendment().await;
        }
        if flip && self.dropped_publish {
            return Err(error("connection reset"));
        }
        let releases = request.read.as_ref().is_some_and(ServingPointer::is_held)
            && !request.pointer.is_held();
        if !flip && releases {
            let mut dropped = self.dropped_releases.lock().unwrap();
            if *dropped > 0 {
                *dropped -= 1;
                return Err(error("connection reset"));
            }
        }
        let mut pointers = self.pointers.lock().unwrap();
        if let Some(rival) = self.rival.clone().filter(|_| !flip) {
            pointers.insert(request.service.clone(), rival);
        }
        let current = pointers.get(&request.service).cloned();
        if flip && self.superseded.as_ref() == Some(&request.service.name)
            || current != request.read
        {
            return Ok(ServingPointerPublication::Superseded { current });
        }
        let holds = !flip && request.pointer.is_held();
        pointers.insert(request.service, request.pointer);
        if holds && self.lost_hold_reply {
            return Err(error("reply lost"));
        }
        if flip && self.lost_reply {
            return Err(error("reply lost"));
        }
        Ok(ServingPointerPublication::Published)
    }
}

fn unit(reply: Reply) -> Result<(), RpcError> {
    match reply {
        Reply::Ok => Ok(()),
        Reply::Error(error) => Err(error),
        Reply::Listed(_)
        | Reply::Created(_)
        | Reply::Settled(..)
        | Reply::CreatedLater(_)
        | Reply::CreatedAfter(..)
        | Reply::Observed(..)
        | Reply::ObservedIngress(_)
        | Reply::Pending => {
            panic!("scripted mutation requires Ok or Error")
        }
    }
}

fn id_of(locator: &ContainerLocator) -> ContainerId {
    match locator {
        ContainerLocator::Id(id) => *id,
        ContainerLocator::Key(slot) => panic!("unscripted slot {slot:?}"),
    }
}

#[path = "exec_tests/dispatch.rs"]
mod dispatch;
#[path = "exec_tests/flip.rs"]
mod flip;
#[path = "exec_tests/health.rs"]
mod health;
#[path = "exec_tests/hooks.rs"]
mod hooks;
#[path = "exec_tests/moving.rs"]
mod moving;
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
        command: vec!["true".into()].try_into().unwrap(),
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
        deadline_millis: None,
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
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        container_id: *container_id,
        display_name: container_id.to_string(),
        created_at_unix_nanos: 0,
        machine_id: *machine_id,
        namespace: test_namespace(),
        kind: ContainerKind::ServiceContainer,
        runtime,
        effective_healthcheck: None,
        resolved_spec: spec,
        address: None,
        labels: Default::default(),
    })
    .unwrap()
}

/// A running, addressed Container that `newest_shape` selects, labelled with
/// the Deploy that created it when `deploy` is given.
fn serving(
    machine_id: &MachineId,
    container_id: &ContainerId,
    deploy: Option<&str>,
) -> ContainerObservation {
    let mut observation = observation(machine_id, container_id, running());
    observation
        .try_update(|parts| {
            parts.address = Some(ployz_core::ContainerAddress("10.210.1.2".parse().unwrap()));
            if let Some(deploy) = deploy {
                parts
                    .labels
                    .insert(ployz_core::DEPLOYMENT_ID_LABEL.into(), deploy.into());
            }
        })
        .unwrap();
    observation
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

fn failing() -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running {
        health: HealthObservation::Failing,
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
        cause: Vec::new(),
    }
}

fn ok(call: Call) -> Step {
    Step(call, Reply::Ok)
}

fn created(call: Call, container_id: &ContainerId) -> Step {
    Step(call, Reply::Created(*container_id))
}

fn unhealthy_run(machine: MachineId, container_id: &ContainerId) -> Step {
    Step(
        Call::Run(machine, ContainerKind::ServiceContainer),
        Reply::Settled(
            *container_id,
            HealthVerdict::Failed {
                observation: unhealthy(),
                last_check: None,
            },
        ),
    )
}

fn created_later(call: Call, container_id: &ContainerId) -> Step {
    Step(call, Reply::CreatedLater(*container_id))
}

fn observed(call: Call, runtime: ContainerRuntimeObservation) -> Step {
    Step(call, Reply::Observed(runtime, None, None))
}

fn observed_ingress(call: Call, runtime: ContainerRuntimeObservation) -> Step {
    Step(call, Reply::ObservedIngress(runtime))
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
        cause: Vec::new(),
    }
}

fn failed_unavailable(call: Call, message: &str) -> Step {
    Step(call, Reply::Error(unavailable(message)))
}

#[tokio::test]
async fn storage_preparation_failure_leaves_all_application_operations_unexecuted() {
    let first = machine('1');
    let second = machine('2');
    let service = provisioned_spec();
    let mut plan = [first, second]
        .map(|machine_id| DeployOperation::PrepareVolumes {
            machine_id,
            specs: vec![ployz_core::ServiceStorageSpec::from(&service)],
        })
        .into_iter()
        .collect::<Vec<_>>();
    let operations = vec![
        run(&first, service.clone(), true),
        run(&second, service, true),
    ];
    plan.extend(operations.clone());
    let client = Scripted::new(vec![
        ok(Call::Prepare(first)),
        Step(
            Call::Prepare(second),
            Reply::Error(error("insufficient storage")),
        ),
    ]);
    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;
    let DeployOutcome::Failed {
        completed,
        unexecuted,
        failed,
        ..
    } = outcome
    else {
        panic!("preparation must fail")
    };
    assert!(
        matches!(completed.as_slice(), [DeployOperation::PrepareVolumes { machine_id, .. }] if *machine_id == first)
    );
    assert_eq!(unexecuted, operations);
    assert!(matches!(
        failed,
        FailedOperation::Operation {
            error: ExecutionError::Machine {
                action: MachineAction::PrepareVolumes,
                ..
            },
            ..
        }
    ));
    client.assert_done();
}

fn provisioned_spec() -> ResolvedServiceSpec {
    let mut service = spec(None, None, None);
    let mut source = ployz_core::RawVolumeSource::Provisioned {
        name: DockerVolumeName::parse("data").unwrap(),
        maximum_bytes: ployz_core::ProvisionedVolumeMaximumBytes::new(
            std::num::NonZeroU64::new(ployz_core::STORAGE_GIB).unwrap(),
        ),
        labels: Default::default(),
    }
    .admit()
    .unwrap();
    source.scope_to_namespace(&test_namespace());
    let reference = ployz_core::ServiceVolumeReference::parse("data").unwrap();
    service
        .set_volume_graph(
            ployz_core::ServiceVolumeGraph::parse(
                vec![ployz_core::ServiceVolume {
                    reference: reference.clone(),
                    source,
                }],
                vec![ployz_core::ServiceMount {
                    volume: reference,
                    target: ployz_core::ContainerPath::parse("/data").unwrap(),
                    read_only: false,
                    no_copy: false,
                    subpath: None,
                }],
            )
            .unwrap()
            .try_into()
            .unwrap(),
        )
        .unwrap();
    service
}

#[tokio::test]
async fn application_failure_and_cancellation_retain_prepared_storage() {
    let target = machine('1');
    let service = provisioned_spec();
    let operations = vec![
        DeployOperation::PrepareVolumes {
            machine_id: target,
            specs: vec![ployz_core::ServiceStorageSpec::from(&service)],
        },
        run(&target, service, true),
    ];
    for cancel_after_prepare in [false, true] {
        let cancellation = CancellationToken::new();
        let mut steps = vec![ok(Call::Prepare(target))];
        if !cancel_after_prepare {
            steps.push(Step(
                Call::Run(target, ContainerKind::ServiceContainer),
                Reply::Error(error("image failed")),
            ));
            steps.push(ok(Call::DiscardSlot(
                target,
                ContainerKind::ServiceContainer,
            )));
        }
        let mut client = Scripted::new(steps);
        client.cancel_on_prepare = cancel_after_prepare.then(|| cancellation.clone());
        let DeployOutcome::Failed {
            completed,
            failed,
            unexecuted,
            ..
        } = execute_with(&operations, &client, &cancellation).await
        else {
            panic!("expected interruption")
        };
        assert!(
            matches!(completed.as_slice(), [DeployOperation::PrepareVolumes { machine_id, .. }] if *machine_id == target)
        );
        assert!(matches!(
            failed,
            FailedOperation::Operation {
                operation: DeployOperation::RunContainer { .. },
                ..
            }
        ));
        assert!(unexecuted.is_empty());
        client.assert_done();
    }
}
