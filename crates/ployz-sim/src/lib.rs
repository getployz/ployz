//! Deterministic mini-simulator and invariant checks for Ployz core orchestration.
//!
//! This crate is for model and control-plane behavior: deploy records, machine
//! membership, volume ownership, projected gateway/DNS state, and the boundary
//! between durable intent, stored status, and live observations. It intentionally
//! does not try to prove Docker, ZFS, WireGuard, NATS, kernel networking, or DNS
//! sockets. Those belong in backend contract and E2E tests.
//!
//! The simulator follows the TigerBeetle-inspired testing shape used in this
//! repository: explicit clocks, seeded scheduling, bounded fake backends,
//! seed-reproducible product workloads, final convergence passes, and invariant
//! checkers that state externally meaningful contracts directly.

use ployz_dns::project_dns;
use ployz_gateway::routes;
use ployz_orchestrator::mesh::MeshNetwork;
use ployz_orchestrator::mesh::wireguard::MemoryWireGuard;
use ployz_store_api::memory::{MemoryService, MemoryStore};
use ployz_store_api::{
    DeployCommit, DeployRepository, InstanceStatusRepository, MachineRegistry,
    RoutingSnapshotReader, StoreDriver,
};
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    DeployId, DeployRecord, DeployState, DrainState, InstanceId, InstancePhase,
    InstanceStatusRecord, MachineId, MachineLifecycle, MachineMembership, OverlayIp, PublicKey,
    RoutingState, ServiceReleaseRecord, ServiceRevisionRecord, VolumeRecord, WireGuardPeerSpec,
};
use ployz_types::spec::{
    ContainerSpec, HttpRoute, Namespace, NetworkMode, Placement, PortProtocol, PullPolicy,
    Resources, RestartPolicy, RolloutStrategy, RouteSpec, ServicePort, ServiceSpec, VolumeScope,
};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

pub const MAX_SIM_STEPS: usize = 10_000;
pub type FakeWireGuard = MemoryWireGuard;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SimInstant {
    unix_secs: u64,
}

impl SimInstant {
    #[must_use]
    pub fn unix_secs(self) -> u64 {
        self.unix_secs
    }
}

#[derive(Debug, Clone)]
pub struct FakeClock {
    now: SimInstant,
}

impl FakeClock {
    #[must_use]
    pub fn new(unix_secs: u64) -> Self {
        Self {
            now: SimInstant { unix_secs },
        }
    }

    #[must_use]
    pub fn now(&self) -> SimInstant {
        self.now
    }

    pub fn advance(&mut self, delta_secs: u64) -> Result<SimInstant> {
        let Some(next) = self.now.unix_secs.checked_add(delta_secs) else {
            return Err(Error::operation("sim.clock.advance", "clock overflow"));
        };
        self.now = SimInstant { unix_secs: next };
        Ok(self.now)
    }

    fn set(&mut self, instant: SimInstant) {
        self.now = instant;
    }
}

#[derive(Debug, Clone)]
pub enum SimEvent {
    UpsertMachine(MachineMembership),
    RemoveMachine(MachineId),
    CommitDeploy(Box<DeployCommit>),
    RecordInstance(InstanceStatusRecord),
    RemoveInstance(InstanceId),
    SetInstanceRunning(InstanceStatusRecord),
    SyncWireGuardFromStore,
    RuntimeStart(InstanceStatusRecord),
    RuntimeStop(InstanceId),
    CheckInvariants,
}

impl SimEvent {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::UpsertMachine(_) => "upsert_machine",
            Self::RemoveMachine(_) => "remove_machine",
            Self::CommitDeploy(_) => "commit_deploy",
            Self::RecordInstance(_) => "record_instance",
            Self::RemoveInstance(_) => "remove_instance",
            Self::SetInstanceRunning(_) => "set_instance_running",
            Self::SyncWireGuardFromStore => "sync_wireguard_from_store",
            Self::RuntimeStart(_) => "runtime_start",
            Self::RuntimeStop(_) => "runtime_stop",
            Self::CheckInvariants => "check_invariants",
        }
    }
}

#[derive(Debug, Clone)]
struct ScheduledEvent {
    ready_at: SimInstant,
    sequence: u64,
    event: SimEvent,
}

impl ScheduledEvent {
    fn cmp_ready(a: &Self, b: &Self) -> Ordering {
        match a.ready_at.cmp(&b.ready_at) {
            Ordering::Equal => a.sequence.cmp(&b.sequence),
            order @ Ordering::Less | order @ Ordering::Greater => order,
        }
    }
}

#[derive(Debug)]
pub struct SeededEventScheduler {
    rng: StdRng,
    next_sequence: u64,
    queue: Vec<ScheduledEvent>,
}

impl SeededEventScheduler {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            rng: StdRng::seed_from_u64(seed),
            next_sequence: 0,
            queue: Vec::new(),
        }
    }

    pub fn schedule_at(&mut self, ready_at: SimInstant, event: SimEvent) -> Result<()> {
        let Some(sequence) = self.next_sequence.checked_add(1) else {
            return Err(Error::operation(
                "sim.scheduler.schedule",
                "sequence overflow",
            ));
        };
        let scheduled = ScheduledEvent {
            ready_at,
            sequence: self.next_sequence,
            event,
        };
        self.next_sequence = sequence;
        self.queue.push(scheduled);
        Ok(())
    }

    pub fn schedule_with_jitter(
        &mut self,
        now: SimInstant,
        max_delay_secs: u64,
        event: SimEvent,
    ) -> Result<()> {
        let delay = if max_delay_secs == 0 {
            0
        } else {
            self.rng.random_range(0..=max_delay_secs)
        };
        let Some(ready_at) = now.unix_secs.checked_add(delay) else {
            return Err(Error::operation(
                "sim.scheduler.schedule",
                "ready time overflow",
            ));
        };
        self.schedule_at(
            SimInstant {
                unix_secs: ready_at,
            },
            event,
        )
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    fn pop_next(&mut self) -> Option<ScheduledEvent> {
        let index = self
            .queue
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| ScheduledEvent::cmp_ready(a, b))
            .map(|(index, _)| index)?;
        Some(self.queue.swap_remove(index))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeContainer {
    pub instance_id: InstanceId,
    pub machine_id: MachineId,
    pub phase: InstancePhase,
}

#[derive(Debug, Default)]
pub struct FakeRuntime {
    containers: HashMap<InstanceId, RuntimeContainer>,
}

impl FakeRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(&mut self, record: &InstanceStatusRecord) -> Result<()> {
        let container = RuntimeContainer {
            instance_id: record.instance_id.clone(),
            machine_id: record.machine_id.clone(),
            phase: record.phase,
        };
        self.containers
            .insert(record.instance_id.clone(), container);
        Ok(())
    }

    pub fn stop(&mut self, instance_id: &InstanceId) -> Result<()> {
        self.containers.remove(instance_id);
        Ok(())
    }

    #[must_use]
    pub fn containers(&self) -> Vec<RuntimeContainer> {
        let mut containers = self.containers.values().cloned().collect::<Vec<_>>();
        containers.sort_by(|left, right| left.instance_id.0.cmp(&right.instance_id.0));
        containers
    }
}

pub struct FakeNatsStore {
    pub driver: StoreDriver,
    pub memory: Arc<MemoryStore>,
    pub service: Arc<MemoryService>,
}

pub type SimStore = FakeNatsStore;

impl FakeNatsStore {
    #[must_use]
    pub fn new() -> Self {
        let memory = Arc::new(MemoryStore::new());
        let service = Arc::new(MemoryService::new());
        Self {
            driver: StoreDriver::memory_with(Arc::clone(&memory), Arc::clone(&service)),
            memory,
            service,
        }
    }
}

impl Default for FakeNatsStore {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeNatsMessage {
    pub sequence: u64,
    pub subject: String,
    pub payload: Vec<u8>,
}

pub struct FakeNats {
    pub store: FakeNatsStore,
    subjects: BTreeMap<String, Vec<FakeNatsMessage>>,
    next_sequence: u64,
}

impl FakeNats {
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: FakeNatsStore::new(),
            subjects: BTreeMap::new(),
            next_sequence: 0,
        }
    }

    pub fn publish(&mut self, subject: impl Into<String>, payload: Vec<u8>) -> Result<u64> {
        let subject = subject.into();
        let Some(next_sequence) = self.next_sequence.checked_add(1) else {
            return Err(Error::operation("sim.nats.publish", "sequence overflow"));
        };
        let sequence = self.next_sequence;
        self.next_sequence = next_sequence;
        let message = FakeNatsMessage {
            sequence,
            subject: subject.clone(),
            payload,
        };
        self.subjects.entry(subject).or_default().push(message);
        Ok(sequence)
    }

    #[must_use]
    pub fn messages(&self, subject: &str) -> &[FakeNatsMessage] {
        self.subjects.get(subject).map(Vec::as_slice).unwrap_or(&[])
    }

    #[must_use]
    pub fn drain_subject(&mut self, subject: &str) -> Vec<FakeNatsMessage> {
        self.subjects.remove(subject).unwrap_or_default()
    }
}

impl Default for FakeNats {
    fn default() -> Self {
        Self::new()
    }
}

pub struct MiniSimulator {
    pub clock: FakeClock,
    pub scheduler: SeededEventScheduler,
    pub store: SimStore,
    pub runtime: FakeRuntime,
    pub wireguard: Arc<FakeWireGuard>,
    committed_namespaces: HashSet<Namespace>,
    wireguard_synced: bool,
    history: Vec<SimStep>,
}

impl MiniSimulator {
    #[must_use]
    pub fn new(seed: u64, start_unix_secs: u64) -> Self {
        Self {
            clock: FakeClock::new(start_unix_secs),
            scheduler: SeededEventScheduler::new(seed),
            store: SimStore::new(),
            runtime: FakeRuntime::new(),
            wireguard: Arc::new(MemoryWireGuard::new()),
            committed_namespaces: HashSet::new(),
            wireguard_synced: false,
            history: Vec::new(),
        }
    }

    pub fn schedule_now(&mut self, event: SimEvent) -> Result<()> {
        self.scheduler.schedule_at(self.clock.now(), event)
    }

    pub async fn run_until_idle(&mut self) -> Result<SimRunResult> {
        self.run_until_idle_with_policy(InvariantCheckPolicy::AfterEveryEvent)
            .await
    }

    pub async fn run_scripted_until_idle(&mut self) -> Result<SimRunResult> {
        self.run_until_idle_with_policy(InvariantCheckPolicy::AtObservationPoints)
            .await
    }

    async fn run_until_idle_with_policy(
        &mut self,
        policy: InvariantCheckPolicy,
    ) -> Result<SimRunResult> {
        let mut steps = 0usize;
        let mut report = InvariantReport::default();
        while let Some(scheduled) = self.scheduler.pop_next() {
            if steps >= MAX_SIM_STEPS {
                return Err(Error::operation(
                    "sim.run",
                    format!("exceeded {MAX_SIM_STEPS} scheduled steps"),
                ));
            }
            steps += 1;
            self.clock.set(scheduled.ready_at);
            let step = SimStep {
                instant: scheduled.ready_at,
                sequence: scheduled.sequence,
                event: scheduled.event.name(),
            };
            let check_after_event = policy.should_check(&scheduled.event);
            self.apply_event(scheduled.event).await?;
            if check_after_event {
                report = check_simulator_invariants(self).await;
            }
            self.history.push(step);
            if check_after_event && !report.is_ok() {
                return Ok(SimRunResult {
                    steps,
                    report,
                    history: self.history.clone(),
                });
            }
        }
        Ok(SimRunResult {
            steps,
            report,
            history: self.history.clone(),
        })
    }

    async fn apply_event(&mut self, event: SimEvent) -> Result<()> {
        match event {
            SimEvent::UpsertMachine(record) => {
                self.wireguard_synced = false;
                self.store.driver.upsert_self_machine(&record).await
            }
            SimEvent::RemoveMachine(id) => {
                self.wireguard_synced = false;
                self.store.driver.delete_machine(&id).await
            }
            SimEvent::CommitDeploy(commit) => {
                self.committed_namespaces.insert(commit.namespace.clone());
                self.store.driver.commit_deploy(&commit).await
            }
            SimEvent::RecordInstance(record) => {
                self.store.driver.record_instance_status(&record).await
            }
            SimEvent::RemoveInstance(instance_id) => {
                self.store.driver.remove_instance_status(&instance_id).await
            }
            SimEvent::SetInstanceRunning(record) => {
                self.store.driver.record_instance_status(&record).await?;
                self.runtime.start(&record)
            }
            SimEvent::SyncWireGuardFromStore => {
                let state = self.store.driver.load_routing_state().await?;
                let peers = state
                    .machines
                    .iter()
                    .filter(|machine| machine.lifecycle == MachineLifecycle::Active)
                    .map(WireGuardPeerSpec::from)
                    .collect::<Vec<_>>();
                self.wireguard.set_peers(&peers).await?;
                self.wireguard_synced = true;
                Ok(())
            }
            SimEvent::RuntimeStart(record) => self.runtime.start(&record),
            SimEvent::RuntimeStop(instance_id) => self.runtime.stop(&instance_id),
            SimEvent::CheckInvariants => Ok(()),
        }
    }

    pub fn schedule_committed_web_service(&mut self, namespace: Namespace) -> Result<()> {
        let revision = fixture::revision(namespace.clone());
        let release = fixture::release(namespace.clone());
        let instance = fixture::instance(namespace.clone(), InstancePhase::Ready);
        let deploy = fixture::deploy(namespace.clone());

        self.schedule_now(SimEvent::UpsertMachine(fixture::machine(
            1,
            MachineLifecycle::Active,
        )))?;
        self.schedule_now(SimEvent::SetInstanceRunning(instance))?;
        self.schedule_now(SimEvent::CommitDeploy(Box::new(DeployCommit {
            namespace,
            revisions: vec![revision],
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            releases: vec![release],
            volumes: Vec::new(),
            deploy,
        })))?;
        self.schedule_now(SimEvent::SyncWireGuardFromStore)
    }

    #[must_use]
    pub fn history(&self) -> &[SimStep] {
        &self.history
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductSwarmOptions {
    pub seed: u64,
    pub ticks: u32,
    pub machines_max: u8,
}

impl ProductSwarmOptions {
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        let mut rng = StdRng::seed_from_u64(seed);
        Self {
            seed,
            ticks: rng.random_range(24..=80),
            machines_max: rng.random_range(3..=8),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductSwarmResult {
    pub seed: u64,
    pub ticks: u32,
    pub operations: Vec<ProductOperation>,
    pub report: InvariantReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProductOperation {
    DeployReady,
    MarkFailed,
    RecoverReady,
    Drain,
    RestoreServing,
    MoveVolume,
    AddMachine,
    RemoveIdleMachine,
    SyncWireGuard,
}

impl ProductOperation {
    pub const ALL: [Self; 9] = [
        Self::DeployReady,
        Self::MarkFailed,
        Self::RecoverReady,
        Self::Drain,
        Self::RestoreServing,
        Self::MoveVolume,
        Self::AddMachine,
        Self::RemoveIdleMachine,
        Self::SyncWireGuard,
    ];
}

pub struct ProductSwarm {
    options: ProductSwarmOptions,
    rng: StdRng,
    namespace: Namespace,
    sim: MiniSimulator,
    active_machines: Vec<MachineId>,
    next_machine_index: u8,
    instance: InstanceStatusRecord,
    volume: VolumeRecord,
    deployed: bool,
}

impl ProductSwarm {
    #[must_use]
    pub fn new(options: ProductSwarmOptions) -> Self {
        let namespace = Namespace::default_ns();
        let instance = fixture::instance(namespace.clone(), InstancePhase::Ready);
        let volume = fixture::volume(namespace.clone(), MachineId("machine-1".into()));
        Self {
            options,
            rng: StdRng::seed_from_u64(options.seed),
            namespace,
            sim: MiniSimulator::new(options.seed, 1),
            active_machines: Vec::new(),
            next_machine_index: 1,
            instance,
            volume,
            deployed: false,
        }
    }

    pub async fn run(mut self) -> Result<ProductSwarmResult> {
        self.bootstrap().await?;
        let mut operations = Vec::new();
        let mut report = self.check().await;
        if !report.is_ok() {
            return Ok(ProductSwarmResult {
                seed: self.options.seed,
                ticks: 0,
                operations,
                report,
            });
        }

        for tick in 0..self.options.ticks {
            let operation = self.next_operation();
            operations.push(operation);
            self.apply_operation(operation).await?;
            report = self.check().await;
            if !report.is_ok() {
                return Ok(ProductSwarmResult {
                    seed: self.options.seed,
                    ticks: tick + 1,
                    operations,
                    report,
                });
            }
        }

        self.apply_operation(ProductOperation::RecoverReady).await?;
        self.apply_operation(ProductOperation::SyncWireGuard)
            .await?;
        operations.push(ProductOperation::RecoverReady);
        operations.push(ProductOperation::SyncWireGuard);
        report = self.check().await;

        Ok(ProductSwarmResult {
            seed: self.options.seed,
            ticks: self.options.ticks,
            operations,
            report,
        })
    }

    async fn bootstrap(&mut self) -> Result<()> {
        self.add_machine().await?;
        self.add_machine().await?;
        self.apply_operation(ProductOperation::DeployReady).await?;
        self.apply_operation(ProductOperation::SyncWireGuard).await
    }

    fn next_operation(&mut self) -> ProductOperation {
        let roll = self.rng.random_range(0..100);
        match roll {
            0..=16 => ProductOperation::DeployReady,
            17..=27 => ProductOperation::MarkFailed,
            28..=39 => ProductOperation::RecoverReady,
            40..=48 => ProductOperation::Drain,
            49..=58 => ProductOperation::RestoreServing,
            59..=70 => ProductOperation::MoveVolume,
            71..=80 => ProductOperation::AddMachine,
            81..=90 => ProductOperation::RemoveIdleMachine,
            91..=99 => ProductOperation::SyncWireGuard,
            _ => unreachable!("roll is in 0..100"),
        }
    }

    async fn apply_operation(&mut self, operation: ProductOperation) -> Result<()> {
        match operation {
            ProductOperation::DeployReady => self.deploy_ready().await,
            ProductOperation::MarkFailed => self.mark_failed().await,
            ProductOperation::RecoverReady | ProductOperation::RestoreServing => {
                self.recover_ready().await
            }
            ProductOperation::Drain => self.drain().await,
            ProductOperation::MoveVolume => self.move_volume().await,
            ProductOperation::AddMachine => self.add_machine().await,
            ProductOperation::RemoveIdleMachine => self.remove_idle_machine().await,
            ProductOperation::SyncWireGuard => {
                self.sim.apply_event(SimEvent::SyncWireGuardFromStore).await
            }
        }
    }

    async fn deploy_ready(&mut self) -> Result<()> {
        self.instance.phase = InstancePhase::Ready;
        self.instance.ready = true;
        self.instance.drain_state = DrainState::None;
        self.instance.error = None;
        if !self
            .active_machines
            .iter()
            .any(|machine_id| machine_id == &self.instance.machine_id)
        {
            let Some(machine_id) = self.active_machines.first().cloned() else {
                return Err(Error::operation(
                    "sim.product.deploy",
                    "cannot deploy without an active machine",
                ));
            };
            self.instance.machine_id = machine_id;
        }
        let deploy_id = if self.deployed {
            "swarm-redeploy"
        } else {
            "swarm-deploy"
        };
        self.deployed = true;
        self.sim
            .apply_event(SimEvent::RecordInstance(self.instance.clone()))
            .await?;
        self.sim
            .apply_event(SimEvent::RuntimeStart(self.instance.clone()))
            .await?;
        self.sim
            .apply_event(SimEvent::CommitDeploy(Box::new(swarm_commit(
                self.namespace.clone(),
                vec![fixture::revision(self.namespace.clone())],
                vec![fixture::release(self.namespace.clone())],
                vec![self.volume.clone()],
                Vec::new(),
                Vec::new(),
                deploy_id,
            ))))
            .await
    }

    async fn mark_failed(&mut self) -> Result<()> {
        self.instance.phase = InstancePhase::Failed;
        self.instance.ready = false;
        self.instance.drain_state = DrainState::None;
        self.instance.error = Some(String::from("simulated runtime failure"));
        self.sim
            .apply_event(SimEvent::RecordInstance(self.instance.clone()))
            .await?;
        self.sim
            .apply_event(SimEvent::RuntimeStop(self.instance.instance_id.clone()))
            .await
    }

    async fn recover_ready(&mut self) -> Result<()> {
        self.instance.phase = InstancePhase::Ready;
        self.instance.ready = true;
        self.instance.drain_state = DrainState::None;
        self.instance.error = None;
        self.sim
            .apply_event(SimEvent::RuntimeStart(self.instance.clone()))
            .await?;
        self.sim
            .apply_event(SimEvent::RecordInstance(self.instance.clone()))
            .await
    }

    async fn drain(&mut self) -> Result<()> {
        self.instance.phase = InstancePhase::Draining;
        self.instance.ready = false;
        self.instance.drain_state = DrainState::Requested;
        self.instance.error = None;
        self.sim
            .apply_event(SimEvent::RecordInstance(self.instance.clone()))
            .await?;
        self.sim
            .apply_event(SimEvent::RuntimeStop(self.instance.instance_id.clone()))
            .await
    }

    async fn move_volume(&mut self) -> Result<()> {
        if self.active_machines.is_empty() {
            return Ok(());
        }
        let index = self.rng.random_range(0..self.active_machines.len());
        let Some(machine_id) = self.active_machines.get(index).cloned() else {
            return Ok(());
        };
        self.volume.machine_id = machine_id;
        self.volume.last_modified_by_deploy_id = DeployId(format!(
            "swarm-volume-{}",
            self.volume.last_modified_at.saturating_add(1)
        ));
        self.volume.last_modified_at = self.volume.last_modified_at.saturating_add(1);
        self.sim
            .apply_event(SimEvent::CommitDeploy(Box::new(swarm_commit(
                self.namespace.clone(),
                Vec::new(),
                Vec::new(),
                vec![self.volume.clone()],
                Vec::new(),
                Vec::new(),
                "swarm-volume-move",
            ))))
            .await
    }

    async fn add_machine(&mut self) -> Result<()> {
        if self.active_machines.len() >= usize::from(self.options.machines_max) {
            return Ok(());
        }
        let index = self.next_machine_index;
        self.next_machine_index = self.next_machine_index.saturating_add(1);
        let machine = fixture::machine(index, MachineLifecycle::Active);
        self.active_machines.push(machine.id.clone());
        self.sim.apply_event(SimEvent::UpsertMachine(machine)).await
    }

    async fn remove_idle_machine(&mut self) -> Result<()> {
        let removable = self.active_machines.iter().position(|machine_id| {
            machine_id != &self.instance.machine_id && machine_id != &self.volume.machine_id
        });
        let Some(index) = removable else {
            return Ok(());
        };
        let machine_id = self.active_machines.remove(index);
        self.sim
            .apply_event(SimEvent::RemoveMachine(machine_id))
            .await
    }

    async fn check(&self) -> InvariantReport {
        check_simulator_invariants(&self.sim).await
    }
}

fn swarm_commit(
    namespace: Namespace,
    revisions: Vec<ServiceRevisionRecord>,
    releases: Vec<ServiceReleaseRecord>,
    volumes: Vec<VolumeRecord>,
    removed_services: Vec<String>,
    removed_volumes: Vec<String>,
    deploy_id: &str,
) -> DeployCommit {
    let mut deploy = fixture::deploy(namespace.clone());
    deploy.deploy_id = DeployId(deploy_id.into());
    DeployCommit {
        namespace,
        revisions,
        removed_services,
        removed_volumes,
        releases,
        volumes,
        deploy,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvariantCheckPolicy {
    AfterEveryEvent,
    AtObservationPoints,
}

impl InvariantCheckPolicy {
    fn should_check(self, event: &SimEvent) -> bool {
        match self {
            Self::AfterEveryEvent => true,
            Self::AtObservationPoints => matches!(event, SimEvent::CheckInvariants),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimStep {
    pub instant: SimInstant,
    pub sequence: u64,
    pub event: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimRunResult {
    pub steps: usize,
    pub report: InvariantReport,
    pub history: Vec<SimStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantViolation {
    pub invariant: &'static str,
    pub message: String,
}

impl InvariantViolation {
    #[must_use]
    pub fn new(invariant: &'static str, message: impl Into<String>) -> Self {
        Self {
            invariant,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InvariantReport {
    pub violations: Vec<InvariantViolation>,
}

impl InvariantReport {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.violations.is_empty()
    }

    fn extend(&mut self, violations: Vec<InvariantViolation>) {
        self.violations.extend(violations);
    }
}

pub async fn check_simulator_invariants(sim: &MiniSimulator) -> InvariantReport {
    let state = match sim.store.driver.load_routing_state().await {
        Ok(state) => state,
        Err(error) => {
            return InvariantReport {
                violations: vec![InvariantViolation::new(
                    "store_read",
                    format!("load routing state failed: {error}"),
                )],
            };
        }
    };

    let mut report = check_cluster_invariants(&state);
    for namespace in &sim.committed_namespaces {
        match sim.store.driver.list_volumes(namespace).await {
            Ok(volumes) => report.extend(check_volume_ownership_records(&state, &volumes)),
            Err(error) => report.violations.push(InvariantViolation::new(
                "volume_ownership",
                format!("list volumes for namespace '{namespace}' failed: {error}"),
            )),
        }
    }
    report.extend(check_runtime_projection(&state, &sim.runtime));
    if sim.wireguard_synced {
        report.extend(check_wireguard_projection(
            &state,
            &sim.wireguard.current_peers(),
        ));
    }
    report
}

#[must_use]
pub fn check_cluster_invariants(state: &RoutingState) -> InvariantReport {
    check_cluster_model_invariants(state, &[])
}

#[must_use]
pub fn check_cluster_model_invariants(
    state: &RoutingState,
    volumes: &[VolumeRecord],
) -> InvariantReport {
    let mut report = InvariantReport::default();
    report.extend(check_membership(state));
    report.extend(check_endpoint_selections(state));
    report.extend(check_volume_ownership_records(state, volumes));
    report.extend(check_deploy_projection(state));
    report.extend(check_gateway_projection(state));
    report.extend(check_dns_projection(state));
    report.extend(check_intent_status_live_observation_separation(state));
    report
}

#[must_use]
pub fn check_membership(state: &RoutingState) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();
    let mut ids = HashSet::new();
    let mut public_keys = HashSet::new();
    let mut overlay_ips = HashSet::new();
    let mut subnets = HashMap::new();

    for machine in &state.machines {
        if !ids.insert(machine.id.clone()) {
            violations.push(InvariantViolation::new(
                "membership",
                format!("duplicate machine id '{}'", machine.id),
            ));
        }
        if !public_keys.insert(machine.public_key.clone()) {
            violations.push(InvariantViolation::new(
                "membership",
                format!("duplicate public key for machine '{}'", machine.id),
            ));
        }
        if !overlay_ips.insert(machine.overlay_ip) {
            violations.push(InvariantViolation::new(
                "membership",
                format!(
                    "duplicate overlay ip '{}' for machine '{}'",
                    machine.overlay_ip, machine.id
                ),
            ));
        }
        match machine.lifecycle {
            MachineLifecycle::Active => {
                if machine.subnet.is_none() {
                    violations.push(InvariantViolation::new(
                        "membership",
                        format!("active machine '{}' has no subnet", machine.id),
                    ));
                }
            }
            MachineLifecycle::Standby => {
                if machine.subnet.is_some() {
                    violations.push(InvariantViolation::new(
                        "membership",
                        format!("standby machine '{}' still owns a subnet", machine.id),
                    ));
                }
            }
            MachineLifecycle::Draining => {}
        }
        if let Some(subnet) = machine.subnet
            && let Some(previous) = subnets.insert(subnet, machine.id.clone())
        {
            violations.push(InvariantViolation::new(
                "membership",
                format!(
                    "subnet '{}' owned by both '{}' and '{}'",
                    subnet, previous, machine.id
                ),
            ));
        }
    }

    violations
}

#[must_use]
pub fn check_endpoint_selections(state: &RoutingState) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();
    let machine_ids = machine_ids(state);
    for machine in &state.machines {
        if machine.lifecycle == MachineLifecycle::Standby && !machine.endpoints.is_empty() {
            violations.push(InvariantViolation::new(
                "endpoint_selection",
                format!("standby machine '{}' has selected endpoints", machine.id),
            ));
        }
        for endpoint in &machine.endpoints {
            if endpoint.trim().is_empty() {
                violations.push(InvariantViolation::new(
                    "endpoint_selection",
                    format!("machine '{}' has an empty endpoint", machine.id),
                ));
            }
        }
    }
    for instance in &state.instances {
        if !machine_ids.contains(&instance.machine_id) {
            violations.push(InvariantViolation::new(
                "endpoint_selection",
                format!(
                    "instance '{}' points at missing machine '{}'",
                    instance.instance_id, instance.machine_id
                ),
            ));
        }
    }
    violations
}

#[must_use]
pub fn check_volume_ownership(state: &RoutingState) -> Vec<InvariantViolation> {
    check_volume_ownership_records(state, &[])
}

#[must_use]
pub fn check_volume_ownership_records(
    state: &RoutingState,
    volume_records: &[VolumeRecord],
) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();
    let machine_ids = machine_ids(state);
    let mut volumes = HashSet::new();

    for volume in volume_records {
        check_volume_record(volume, &machine_ids, &mut volumes, &mut violations);
    }

    violations
}

#[must_use]
pub fn check_deploy_projection(state: &RoutingState) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();
    let machines = machine_ids(state);
    let revisions = revision_keys(state);
    let instances = state
        .instances
        .iter()
        .map(|instance| (instance.instance_id.clone(), instance))
        .collect::<HashMap<_, _>>();

    for release in &state.releases {
        match &release.release.routing {
            ployz_types::model::ServiceRoutingPolicy::Direct { revision_hash } => {
                let key = (
                    release.namespace.clone(),
                    release.service.clone(),
                    revision_hash.clone(),
                );
                if !revisions.contains(&key) {
                    violations.push(InvariantViolation::new(
                        "deploy_projection",
                        format!(
                            "release '{}/{}' routes to missing revision '{}'",
                            release.namespace, release.service, revision_hash
                        ),
                    ));
                }
            }
            ployz_types::model::ServiceRoutingPolicy::Split { allocations } => {
                let mut percent_total = 0u16;
                for allocation in allocations {
                    percent_total += u16::from(allocation.percent);
                    let key = (
                        release.namespace.clone(),
                        release.service.clone(),
                        allocation.revision_hash.clone(),
                    );
                    if !revisions.contains(&key) {
                        violations.push(InvariantViolation::new(
                            "deploy_projection",
                            format!(
                                "release '{}/{}' allocates to missing revision '{}'",
                                release.namespace, release.service, allocation.revision_hash
                            ),
                        ));
                    }
                }
                if percent_total != 100 {
                    violations.push(InvariantViolation::new(
                        "deploy_projection",
                        format!(
                            "release '{}/{}' split totals {} percent",
                            release.namespace, release.service, percent_total
                        ),
                    ));
                }
            }
        }

        let mut slot_ids = HashSet::new();
        for slot in &release.release.slots {
            if !machines.contains(&slot.machine_id) {
                violations.push(InvariantViolation::new(
                    "deploy_projection",
                    format!(
                        "release '{}/{}' slot '{}' points at missing machine '{}'",
                        release.namespace, release.service, slot.slot_id, slot.machine_id
                    ),
                ));
            }
            let revision_key = (
                release.namespace.clone(),
                release.service.clone(),
                slot.revision_hash.clone(),
            );
            if !revisions.contains(&revision_key) {
                violations.push(InvariantViolation::new(
                    "deploy_projection",
                    format!(
                        "release '{}/{}' slot '{}' points at missing revision '{}'",
                        release.namespace, release.service, slot.slot_id, slot.revision_hash
                    ),
                ));
            }
            match instances.get(&slot.active_instance_id) {
                Some(instance) => {
                    if instance.namespace != release.namespace
                        || instance.service != release.service
                        || instance.revision_hash != slot.revision_hash
                        || instance.machine_id != slot.machine_id
                    {
                        violations.push(InvariantViolation::new(
                            "deploy_projection",
                            format!(
                                "release '{}/{}' slot '{}' does not match active instance '{}'",
                                release.namespace,
                                release.service,
                                slot.slot_id,
                                slot.active_instance_id
                            ),
                        ));
                    }
                }
                None => {
                    violations.push(InvariantViolation::new(
                        "deploy_projection",
                        format!(
                            "release '{}/{}' slot '{}' points at missing instance '{}'",
                            release.namespace,
                            release.service,
                            slot.slot_id,
                            slot.active_instance_id
                        ),
                    ));
                }
            }
            if !slot_ids.insert(slot.slot_id.clone()) {
                violations.push(InvariantViolation::new(
                    "deploy_projection",
                    format!(
                        "release '{}/{}' has duplicate slot '{}'",
                        release.namespace, release.service, slot.slot_id
                    ),
                ));
            }
        }
    }

    violations
}

#[must_use]
pub fn check_gateway_projection(state: &RoutingState) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();
    let projected = match routes::project(state.clone()) {
        Ok(projected) => projected,
        Err(error) => {
            return vec![InvariantViolation::new(
                "gateway_projection",
                format!("gateway projection failed: {error}"),
            )];
        }
    };
    let ready_instances = routable_instance_ids(state);

    for route in projected.http_routes {
        for backend in route.backends {
            if !ready_instances.contains(&backend.instance_id) {
                violations.push(InvariantViolation::new(
                    "gateway_projection",
                    format!(
                        "http route for '{}/{}' includes non-routable instance '{}'",
                        route.namespace, route.service, backend.instance_id
                    ),
                ));
            }
        }
    }
    for route in projected.tcp_routes {
        for backend in route.backends {
            if !ready_instances.contains(&backend.instance_id) {
                violations.push(InvariantViolation::new(
                    "gateway_projection",
                    format!(
                        "tcp route for '{}/{}' includes non-routable instance '{}'",
                        route.namespace, route.service, backend.instance_id
                    ),
                ));
            }
        }
    }

    violations
}

#[must_use]
pub fn check_dns_projection(state: &RoutingState) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();
    let projected = project_dns(state);
    let ready_services = ready_service_keys(state);

    for (namespace, by_service) in projected.services {
        for (service, ips) in by_service {
            if !ready_services.contains(&(namespace.clone(), service.clone())) {
                violations.push(InvariantViolation::new(
                    "dns_projection",
                    format!("dns advertises non-routable service '{namespace}/{service}'"),
                ));
            }
            if ips.is_empty() {
                violations.push(InvariantViolation::new(
                    "dns_projection",
                    format!("dns service '{namespace}/{service}' has no addresses"),
                ));
            }
        }
    }

    violations
}

#[must_use]
pub fn check_intent_status_live_observation_separation(
    state: &RoutingState,
) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();
    let machine_ids = machine_ids(state);

    for instance in &state.instances {
        if instance.ready && instance.phase != InstancePhase::Ready {
            violations.push(InvariantViolation::new(
                "intent_status_live_observation",
                format!(
                    "instance '{}' is ready while phase is '{}'",
                    instance.instance_id, instance.phase
                ),
            ));
        }
        if instance.drain_state == DrainState::Complete && instance.phase == InstancePhase::Ready {
            violations.push(InvariantViolation::new(
                "intent_status_live_observation",
                format!(
                    "instance '{}' is drain-complete but still ready",
                    instance.instance_id
                ),
            ));
        }
        if !machine_ids.contains(&instance.machine_id) {
            violations.push(InvariantViolation::new(
                "intent_status_live_observation",
                format!(
                    "status instance '{}' references missing intent machine '{}'",
                    instance.instance_id, instance.machine_id
                ),
            ));
        }
    }

    violations
}

#[must_use]
pub fn check_runtime_projection(
    state: &RoutingState,
    runtime: &FakeRuntime,
) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();
    let live_containers = runtime
        .containers()
        .into_iter()
        .map(|container| (container.instance_id.clone(), container))
        .collect::<HashMap<_, _>>();
    let status_instances = state
        .instances
        .iter()
        .map(|instance| (instance.instance_id.clone(), instance))
        .collect::<HashMap<_, _>>();
    for container in live_containers.values() {
        match status_instances.get(&container.instance_id) {
            Some(status) => {
                if status.machine_id != container.machine_id {
                    violations.push(InvariantViolation::new(
                        "runtime_projection",
                        format!(
                            "runtime container '{}' is on machine '{}' but status says '{}'",
                            container.instance_id, container.machine_id, status.machine_id
                        ),
                    ));
                }
                if status.phase != container.phase {
                    violations.push(InvariantViolation::new(
                        "runtime_projection",
                        format!(
                            "runtime container '{}' phase '{}' disagrees with status phase '{}'",
                            container.instance_id, container.phase, status.phase
                        ),
                    ));
                }
            }
            None => {
                violations.push(InvariantViolation::new(
                    "runtime_projection",
                    format!(
                        "runtime has live container '{}' with no status row",
                        container.instance_id
                    ),
                ));
            }
        }
    }
    for instance in &state.instances {
        if instance.phase == InstancePhase::Ready
            && instance.ready
            && instance.drain_state == DrainState::None
            && !live_containers.contains_key(&instance.instance_id)
        {
            violations.push(InvariantViolation::new(
                "runtime_projection",
                format!(
                    "ready status '{}' has no matching live runtime container",
                    instance.instance_id
                ),
            ));
        }
    }
    violations
}

#[must_use]
pub fn check_wireguard_projection(
    state: &RoutingState,
    peers: &[WireGuardPeerSpec],
) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();
    let active = state
        .machines
        .iter()
        .filter(|machine| machine.lifecycle == MachineLifecycle::Active)
        .map(|machine| machine.id.clone())
        .collect::<HashSet<_>>();
    let mut peer_ids = HashSet::new();
    for peer in peers {
        let id = peer.id().clone();
        if !active.contains(&id) {
            violations.push(InvariantViolation::new(
                "wireguard_projection",
                format!("wireguard includes non-active peer '{id}'"),
            ));
        }
        if !peer_ids.insert(id.clone()) {
            violations.push(InvariantViolation::new(
                "wireguard_projection",
                format!("wireguard includes duplicate peer '{id}'"),
            ));
        }
    }
    for id in active {
        if !peer_ids.contains(&id) {
            violations.push(InvariantViolation::new(
                "wireguard_projection",
                format!("active machine '{id}' missing from wireguard peers"),
            ));
        }
    }
    violations
}

fn check_volume_record(
    volume: &VolumeRecord,
    machine_ids: &HashSet<MachineId>,
    volumes: &mut HashSet<(Namespace, String)>,
    violations: &mut Vec<InvariantViolation>,
) {
    if !volumes.insert((volume.namespace.clone(), volume.volume_name.clone())) {
        violations.push(InvariantViolation::new(
            "volume_ownership",
            format!(
                "duplicate volume '{}/{}'",
                volume.namespace, volume.volume_name
            ),
        ));
    }
    if !machine_ids.contains(&volume.machine_id) {
        violations.push(InvariantViolation::new(
            "volume_ownership",
            format!(
                "volume '{}/{}' points at missing machine '{}'",
                volume.namespace, volume.volume_name, volume.machine_id
            ),
        ));
    }
    if volume.scope == VolumeScope::Single && volume.attached_services.len() > 1 {
        violations.push(InvariantViolation::new(
            "volume_ownership",
            format!(
                "single-owner volume '{}/{}' is attached to {} services",
                volume.namespace,
                volume.volume_name,
                volume.attached_services.len()
            ),
        ));
    }
}

fn machine_ids(state: &RoutingState) -> HashSet<MachineId> {
    state
        .machines
        .iter()
        .map(|machine| machine.id.clone())
        .collect()
}

fn revision_keys(state: &RoutingState) -> HashSet<(Namespace, String, String)> {
    state
        .revisions
        .iter()
        .map(|revision| {
            (
                revision.namespace.clone(),
                revision.service.clone(),
                revision.revision_hash.clone(),
            )
        })
        .collect()
}

fn routable_instance_ids(state: &RoutingState) -> HashSet<InstanceId> {
    state
        .instances
        .iter()
        .filter(|instance| {
            instance.phase == InstancePhase::Ready
                && instance.ready
                && instance.drain_state == DrainState::None
                && instance.overlay_ip.is_some()
        })
        .map(|instance| instance.instance_id.clone())
        .collect()
}

fn ready_service_keys(state: &RoutingState) -> HashSet<(Namespace, String)> {
    state
        .instances
        .iter()
        .filter(|instance| {
            instance.phase == InstancePhase::Ready
                && instance.ready
                && instance.drain_state == DrainState::None
                && instance.overlay_ip.is_some()
        })
        .map(|instance| (instance.namespace.clone(), instance.service.clone()))
        .collect()
}

pub mod fixture {
    use super::*;
    use ployz_types::model::{
        DeployId, ServiceRelease, ServiceReleaseSlot, ServiceRoutingPolicy, SlotId,
        StorageParticipation,
    };

    #[must_use]
    pub fn machine(index: u8, lifecycle: MachineLifecycle) -> MachineMembership {
        let subnet = match lifecycle {
            MachineLifecycle::Active | MachineLifecycle::Draining => Some(
                format!("10.42.{index}.0/24")
                    .parse()
                    .expect("valid fixture subnet"),
            ),
            MachineLifecycle::Standby => None,
        };
        MachineMembership {
            id: MachineId(format!("machine-{index}")),
            public_key: PublicKey([index; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, u16::from(index))),
            topology: ployz_types::model::MachineTopology::local(),
            subnet,
            bridge_ip: None,
            endpoints: vec![format!("192.0.2.{index}:51820")],
            lifecycle,
            storage: true,
            storage_participation: StorageParticipation::default_authority(),
            created_at: 1,
            updated_at: 1,
            labels: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn service_spec() -> ServiceSpec {
        ServiceSpec {
            name: "web".into(),
            placement: Placement::Replicated { count: 1 },
            template: ContainerSpec {
                image: "example/web:latest".into(),
                command: None,
                entrypoint: None,
                env: BTreeMap::new(),
                mounts: Vec::new(),
                cap_add: Vec::new(),
                cap_drop: Vec::new(),
                privileged: false,
                user: None,
                stop_grace_period: None,
                pid_mode: None,
                pull_policy: PullPolicy::IfNotPresent,
                resources: Resources::empty(),
                sysctls: BTreeMap::new(),
            },
            network: NetworkMode::Overlay,
            service_ports: vec![ServicePort {
                name: "http".into(),
                container_port: 8080,
                protocol: PortProtocol::Tcp,
            }],
            publish: Vec::new(),
            routes: vec![RouteSpec::Http(HttpRoute {
                service_port: "http".into(),
                hostnames: vec!["web.example.test".into()],
                path_prefix: "/".into(),
            })],
            readiness: None,
            rollout: RolloutStrategy::Recreate,
            labels: BTreeMap::new(),
            restart: RestartPolicy::unless_stopped(),
        }
    }

    #[must_use]
    pub fn revision(namespace: Namespace) -> ServiceRevisionRecord {
        let service = service_spec();
        let spec_json = service
            .canonical_revision_json()
            .expect("valid fixture service");
        ServiceRevisionRecord {
            namespace,
            service: service.name,
            revision_hash: "rev-a".into(),
            spec_json,
            created_by: MachineId("machine-1".into()),
            created_at: 1,
        }
    }

    #[must_use]
    pub fn instance(namespace: Namespace, phase: InstancePhase) -> InstanceStatusRecord {
        InstanceStatusRecord {
            instance_id: InstanceId("inst-web-1".into()),
            namespace,
            service: "web".into(),
            slot_id: SlotId("slot-1".into()),
            machine_id: MachineId("machine-1".into()),
            revision_hash: "rev-a".into(),
            deploy_id: DeployId("deploy-1".into()),
            docker_container_id: "container-1".into(),
            overlay_ip: Some(Ipv4Addr::new(10, 42, 1, 10)),
            backend_ports: BTreeMap::from([(String::from("http"), 8080)]),
            phase,
            ready: phase == InstancePhase::Ready,
            drain_state: DrainState::None,
            error: None,
            started_at: 1,
            updated_at: 1,
        }
    }

    #[must_use]
    pub fn release(namespace: Namespace) -> ServiceReleaseRecord {
        ServiceReleaseRecord {
            namespace,
            service: "web".into(),
            release: ServiceRelease {
                primary_revision_hash: "rev-a".into(),
                referenced_revision_hashes: vec!["rev-a".into()],
                routing: ServiceRoutingPolicy::Direct {
                    revision_hash: "rev-a".into(),
                },
                slots: vec![ServiceReleaseSlot {
                    slot_id: SlotId("slot-1".into()),
                    machine_id: MachineId("machine-1".into()),
                    active_instance_id: InstanceId("inst-web-1".into()),
                    revision_hash: "rev-a".into(),
                }],
                updated_by_deploy_id: DeployId("deploy-1".into()),
                updated_at: 1,
            },
        }
    }

    #[must_use]
    pub fn deploy(namespace: Namespace) -> DeployRecord {
        DeployRecord {
            deploy_id: DeployId("deploy-1".into()),
            namespace,
            coordinator_machine_id: MachineId("machine-1".into()),
            manifest_hash: "manifest-a".into(),
            state: DeployState::Committed,
            started_at: 1,
            committed_at: Some(1),
            finished_at: Some(1),
            summary_json: "{}".into(),
        }
    }

    #[must_use]
    pub fn volume(namespace: Namespace, machine_id: MachineId) -> VolumeRecord {
        VolumeRecord {
            namespace,
            volume_name: "data".into(),
            scope: VolumeScope::Single,
            machine_id,
            quota: "1G".into(),
            mode: "0750".into(),
            owner: "1000:1000".into(),
            attached_services: vec!["web".into()],
            created_at: 1,
            created_by_deploy_id: DeployId("deploy-1".into()),
            last_modified_at: 1,
            last_modified_by_deploy_id: DeployId("deploy-1".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture;
    use super::*;
    use ployz_types::model::DeployId;

    fn commit(
        namespace: Namespace,
        revisions: Vec<ServiceRevisionRecord>,
        releases: Vec<ServiceReleaseRecord>,
        volumes: Vec<VolumeRecord>,
        removed_services: Vec<String>,
        removed_volumes: Vec<String>,
        deploy_id: &str,
    ) -> DeployCommit {
        let mut deploy = fixture::deploy(namespace.clone());
        deploy.deploy_id = DeployId(deploy_id.into());
        DeployCommit {
            namespace,
            revisions,
            removed_services,
            removed_volumes,
            releases,
            volumes,
            deploy,
        }
    }

    async fn routing_state(sim: &MiniSimulator) -> RoutingState {
        sim.store
            .driver
            .load_routing_state()
            .await
            .expect("routing state")
    }

    fn assert_gateway_backends(state: &RoutingState, expected: usize) {
        let snapshot = routes::project(state.clone()).expect("gateway projection");
        let backend_count = snapshot
            .http_routes
            .iter()
            .flat_map(|route| route.backends.iter())
            .count();
        assert_eq!(backend_count, expected);
    }

    fn assert_dns_addresses(state: &RoutingState, namespace: &Namespace, expected: usize) {
        let snapshot = project_dns(state);
        let address_count = snapshot
            .lookup_service(namespace, "web")
            .map(<[Ipv4Addr]>::len)
            .unwrap_or(0);
        assert_eq!(address_count, expected);
    }

    async fn assert_exposure(sim: &MiniSimulator, namespace: &Namespace, expected: usize) {
        let state = routing_state(sim).await;
        assert_gateway_backends(&state, expected);
        assert_dns_addresses(&state, namespace, expected);
    }

    #[test]
    fn scheduler_orders_same_seed_deterministically() {
        let mut first = SeededEventScheduler::new(7);
        let mut second = SeededEventScheduler::new(7);
        for index in 0..8 {
            let event = SimEvent::RemoveMachine(MachineId(format!("m-{index}")));
            first
                .schedule_with_jitter(SimInstant { unix_secs: 10 }, 5, event.clone())
                .expect("schedule first");
            second
                .schedule_with_jitter(SimInstant { unix_secs: 10 }, 5, event)
                .expect("schedule second");
        }

        let mut first_order = Vec::new();
        let mut second_order = Vec::new();
        while let Some(event) = first.pop_next() {
            first_order.push((event.ready_at, event.sequence));
        }
        while let Some(event) = second.pop_next() {
            second_order.push((event.ready_at, event.sequence));
        }

        assert_eq!(first_order, second_order);
    }

    #[test]
    fn fake_nats_orders_messages_by_publish_sequence() {
        let mut nats = FakeNats::new();

        let first = nats
            .publish("ployz.test.events", b"first".to_vec())
            .expect("publish first");
        let second = nats
            .publish("ployz.test.events", b"second".to_vec())
            .expect("publish second");

        assert_eq!(first, 0);
        assert_eq!(second, 1);
        assert_eq!(nats.messages("ployz.test.events").len(), 2);
        let drained = nats.drain_subject("ployz.test.events");
        let [first, _second] = drained.as_slice() else {
            panic!("expected two drained messages");
        };
        assert_eq!(first.payload, b"first");
        assert!(nats.messages("ployz.test.events").is_empty());
    }

    #[test]
    fn membership_invariant_catches_duplicate_subnet() {
        let mut left = fixture::machine(1, MachineLifecycle::Active);
        let mut right = fixture::machine(2, MachineLifecycle::Active);
        right.subnet = left.subnet;
        left.endpoints.clear();
        right.endpoints.clear();
        let state = RoutingState {
            machines: vec![left, right],
            revisions: Vec::new(),
            releases: Vec::new(),
            instances: Vec::new(),
        };

        let violations = check_membership(&state);

        assert!(
            violations
                .iter()
                .any(|violation| violation.invariant == "membership"
                    && violation.message.contains("owned by both"))
        );
    }

    #[test]
    fn deploy_projection_catches_missing_revision_and_instance() {
        let namespace = Namespace::default_ns();
        let state = RoutingState {
            machines: vec![fixture::machine(1, MachineLifecycle::Active)],
            revisions: Vec::new(),
            releases: vec![fixture::release(namespace)],
            instances: Vec::new(),
        };

        let violations = check_deploy_projection(&state);

        assert!(
            violations
                .iter()
                .any(|violation| violation.message.contains("missing revision"))
        );
        assert!(
            violations
                .iter()
                .any(|violation| violation.message.contains("missing instance"))
        );
    }

    #[test]
    fn volume_ownership_catches_missing_machine_and_multi_attach_single_volume() {
        let namespace = Namespace::default_ns();
        let mut volume = fixture::volume(namespace, MachineId("missing-machine".into()));
        volume.attached_services.push("worker".into());
        let state = RoutingState {
            machines: vec![fixture::machine(1, MachineLifecycle::Active)],
            revisions: Vec::new(),
            releases: Vec::new(),
            instances: Vec::new(),
        };

        let violations = check_volume_ownership_records(&state, &[volume]);

        assert!(
            violations
                .iter()
                .any(|violation| violation.message.contains("missing machine"))
        );
        assert!(
            violations
                .iter()
                .any(|violation| violation.message.contains("single-owner volume"))
        );
    }

    #[test]
    fn endpoint_and_status_invariants_catch_bad_observations() {
        let namespace = Namespace::default_ns();
        let mut standby = fixture::machine(1, MachineLifecycle::Standby);
        standby.endpoints = vec!["192.0.2.1:51820".into()];
        let mut instance = fixture::instance(namespace, InstancePhase::Starting);
        instance.ready = true;
        let state = RoutingState {
            machines: vec![standby],
            revisions: Vec::new(),
            releases: Vec::new(),
            instances: vec![instance],
        };

        let report = check_cluster_invariants(&state);

        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.invariant == "endpoint_selection")
        );
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.invariant == "intent_status_live_observation")
        );
    }

    #[tokio::test]
    async fn simulator_runs_store_runtime_wireguard_and_projection_invariants() {
        let namespace = Namespace::default_ns();
        let mut sim = MiniSimulator::new(11, 1);
        sim.schedule_committed_web_service(namespace.clone())
            .expect("schedule fixture service");

        let result = sim.run_until_idle().await.expect("sim runs");

        assert_eq!(result.report.violations, Vec::new());
        assert_eq!(result.steps, 4);
        assert_eq!(
            result.history.last().map(|step| step.event),
            Some("sync_wireguard_from_store")
        );
        assert_eq!(sim.wireguard.current_peers().len(), 1);
        assert_eq!(sim.runtime.containers().len(), 1);

        let state = sim
            .store
            .driver
            .load_routing_state()
            .await
            .expect("routing state");
        let gateway = routes::project(state.clone()).expect("gateway projection");
        let dns = project_dns(&state);
        assert_eq!(gateway.http_routes.len(), 1);
        assert_eq!(
            dns.lookup_service(&namespace, "web").map(|ips| ips.len()),
            Some(1)
        );
    }

    #[tokio::test]
    async fn simulator_reports_live_runtime_without_status() {
        let namespace = Namespace::default_ns();
        let instance = fixture::instance(namespace, InstancePhase::Ready);
        let mut sim = MiniSimulator::new(12, 1);
        sim.schedule_now(SimEvent::RuntimeStart(instance))
            .expect("schedule runtime");

        let result = sim.run_until_idle().await.expect("sim runs");

        assert!(
            result
                .report
                .violations
                .iter()
                .any(|violation| violation.invariant == "runtime_projection")
        );
        assert_eq!(
            result.history.last().map(|step| step.event),
            Some("runtime_start")
        );
    }

    #[tokio::test]
    async fn product_deploy_lifecycle_exposes_ready_instances_only() {
        let namespace = Namespace::default_ns();
        let starting = fixture::instance(namespace.clone(), InstancePhase::Starting);
        let mut ready = fixture::instance(namespace.clone(), InstancePhase::Ready);
        ready.updated_at = 2;
        let mut draining = ready.clone();
        draining.drain_state = DrainState::Requested;
        draining.updated_at = 3;

        let mut sim = MiniSimulator::new(21, 1);
        sim.schedule_now(SimEvent::UpsertMachine(fixture::machine(
            1,
            MachineLifecycle::Active,
        )))
        .expect("schedule machine");
        sim.schedule_now(SimEvent::RecordInstance(starting.clone()))
            .expect("schedule starting status");
        sim.schedule_now(SimEvent::RuntimeStart(starting))
            .expect("schedule starting runtime");
        sim.schedule_now(SimEvent::CommitDeploy(Box::new(commit(
            namespace.clone(),
            vec![fixture::revision(namespace.clone())],
            vec![fixture::release(namespace.clone())],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            "deploy-starting",
        ))))
        .expect("schedule deploy");
        sim.schedule_now(SimEvent::CheckInvariants)
            .expect("schedule observation");

        let result = sim.run_scripted_until_idle().await.expect("sim runs");
        assert_eq!(result.report.violations, Vec::new());
        assert_exposure(&sim, &namespace, 0).await;

        sim.schedule_now(SimEvent::RecordInstance(ready.clone()))
            .expect("schedule ready status");
        sim.schedule_now(SimEvent::RuntimeStart(ready))
            .expect("schedule ready runtime");
        sim.schedule_now(SimEvent::CheckInvariants)
            .expect("schedule ready observation");

        let result = sim.run_scripted_until_idle().await.expect("sim runs");
        assert_eq!(result.report.violations, Vec::new());
        assert_exposure(&sim, &namespace, 1).await;

        sim.schedule_now(SimEvent::RecordInstance(draining))
            .expect("schedule drain status");
        sim.schedule_now(SimEvent::CheckInvariants)
            .expect("schedule drain observation");

        let result = sim.run_scripted_until_idle().await.expect("sim runs");
        assert_eq!(result.report.violations, Vec::new());
        assert_exposure(&sim, &namespace, 0).await;

        sim.schedule_now(SimEvent::RuntimeStop(InstanceId("inst-web-1".into())))
            .expect("schedule runtime stop");
        sim.schedule_now(SimEvent::RemoveInstance(InstanceId("inst-web-1".into())))
            .expect("schedule status remove");
        sim.schedule_now(SimEvent::CommitDeploy(Box::new(commit(
            namespace.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![String::from("web")],
            Vec::new(),
            "deploy-remove",
        ))))
        .expect("schedule release removal");
        sim.schedule_now(SimEvent::CheckInvariants)
            .expect("schedule removal observation");

        let result = sim.run_scripted_until_idle().await.expect("sim runs");
        assert_eq!(result.report.violations, Vec::new());
        assert_exposure(&sim, &namespace, 0).await;
    }

    #[tokio::test]
    async fn product_node_membership_changes_refresh_wireguard_without_route_loss() {
        let namespace = Namespace::default_ns();
        let instance = fixture::instance(namespace.clone(), InstancePhase::Ready);
        let mut sim = MiniSimulator::new(22, 1);
        sim.schedule_now(SimEvent::UpsertMachine(fixture::machine(
            1,
            MachineLifecycle::Active,
        )))
        .expect("schedule first machine");
        sim.schedule_now(SimEvent::UpsertMachine(fixture::machine(
            2,
            MachineLifecycle::Active,
        )))
        .expect("schedule second machine");
        sim.schedule_now(SimEvent::RecordInstance(instance.clone()))
            .expect("schedule status");
        sim.schedule_now(SimEvent::RuntimeStart(instance))
            .expect("schedule runtime");
        sim.schedule_now(SimEvent::CommitDeploy(Box::new(commit(
            namespace.clone(),
            vec![fixture::revision(namespace.clone())],
            vec![fixture::release(namespace.clone())],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            "deploy-two-node",
        ))))
        .expect("schedule deploy");
        sim.schedule_now(SimEvent::SyncWireGuardFromStore)
            .expect("schedule wireguard sync");
        sim.schedule_now(SimEvent::CheckInvariants)
            .expect("schedule observation");

        let result = sim.run_scripted_until_idle().await.expect("sim runs");
        assert_eq!(result.report.violations, Vec::new());
        assert_eq!(sim.wireguard.current_peers().len(), 2);
        assert_exposure(&sim, &namespace, 1).await;

        sim.schedule_now(SimEvent::RemoveMachine(MachineId("machine-2".into())))
            .expect("schedule machine removal");
        sim.schedule_now(SimEvent::SyncWireGuardFromStore)
            .expect("schedule wireguard resync");
        sim.schedule_now(SimEvent::CheckInvariants)
            .expect("schedule removal observation");

        let result = sim.run_scripted_until_idle().await.expect("sim runs");
        assert_eq!(result.report.violations, Vec::new());
        let peer_ids = sim
            .wireguard
            .current_peers()
            .iter()
            .map(|peer| peer.id().clone())
            .collect::<HashSet<_>>();
        assert_eq!(peer_ids, HashSet::from([MachineId("machine-1".into())]));
        assert_exposure(&sim, &namespace, 1).await;
    }

    #[tokio::test]
    async fn product_volume_ownership_must_move_before_node_removal() {
        let namespace = Namespace::default_ns();
        let instance = fixture::instance(namespace.clone(), InstancePhase::Ready);
        let mut volume = fixture::volume(namespace.clone(), MachineId("machine-1".into()));
        let mut sim = MiniSimulator::new(23, 1);
        sim.schedule_now(SimEvent::UpsertMachine(fixture::machine(
            1,
            MachineLifecycle::Active,
        )))
        .expect("schedule first machine");
        sim.schedule_now(SimEvent::UpsertMachine(fixture::machine(
            2,
            MachineLifecycle::Active,
        )))
        .expect("schedule second machine");
        sim.schedule_now(SimEvent::RecordInstance(instance.clone()))
            .expect("schedule status");
        sim.schedule_now(SimEvent::RuntimeStart(instance))
            .expect("schedule runtime");
        sim.schedule_now(SimEvent::CommitDeploy(Box::new(commit(
            namespace.clone(),
            vec![fixture::revision(namespace.clone())],
            vec![fixture::release(namespace.clone())],
            vec![volume.clone()],
            Vec::new(),
            Vec::new(),
            "deploy-volume-machine-1",
        ))))
        .expect("schedule first volume owner");
        sim.schedule_now(SimEvent::CheckInvariants)
            .expect("schedule first observation");

        let result = sim.run_scripted_until_idle().await.expect("sim runs");
        assert_eq!(result.report.violations, Vec::new());

        volume.machine_id = MachineId("machine-2".into());
        volume.last_modified_by_deploy_id = DeployId("deploy-volume-machine-2".into());
        sim.schedule_now(SimEvent::CommitDeploy(Box::new(commit(
            namespace.clone(),
            Vec::new(),
            Vec::new(),
            vec![volume],
            Vec::new(),
            Vec::new(),
            "deploy-volume-machine-2",
        ))))
        .expect("schedule volume move");
        sim.schedule_now(SimEvent::CheckInvariants)
            .expect("schedule moved observation");

        let result = sim.run_scripted_until_idle().await.expect("sim runs");
        assert_eq!(result.report.violations, Vec::new());
        let volumes = sim
            .store
            .driver
            .list_volumes(&namespace)
            .await
            .expect("volumes");
        assert_eq!(
            volumes.first().map(|volume| volume.machine_id.clone()),
            Some(MachineId("machine-2".into()))
        );

        sim.schedule_now(SimEvent::RemoveMachine(MachineId("machine-2".into())))
            .expect("schedule owner removal");
        sim.schedule_now(SimEvent::CheckInvariants)
            .expect("schedule invalid observation");

        let result = sim.run_scripted_until_idle().await.expect("sim runs");
        assert!(
            result
                .report
                .violations
                .iter()
                .any(|violation| violation.invariant == "volume_ownership"
                    && violation.message.contains("missing machine"))
        );
    }

    #[tokio::test]
    async fn product_failure_and_recovery_keep_live_observation_separate_from_status() {
        let namespace = Namespace::default_ns();
        let ready = fixture::instance(namespace.clone(), InstancePhase::Ready);
        let mut failed = ready.clone();
        failed.phase = InstancePhase::Failed;
        failed.ready = false;
        failed.error = Some(String::from("readiness probe failed"));
        failed.updated_at = 2;

        let mut sim = MiniSimulator::new(24, 1);
        sim.schedule_now(SimEvent::UpsertMachine(fixture::machine(
            1,
            MachineLifecycle::Active,
        )))
        .expect("schedule machine");
        sim.schedule_now(SimEvent::RecordInstance(ready.clone()))
            .expect("schedule ready status");
        sim.schedule_now(SimEvent::RuntimeStart(ready.clone()))
            .expect("schedule ready runtime");
        sim.schedule_now(SimEvent::CommitDeploy(Box::new(commit(
            namespace.clone(),
            vec![fixture::revision(namespace.clone())],
            vec![fixture::release(namespace.clone())],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            "deploy-ready",
        ))))
        .expect("schedule deploy");
        sim.schedule_now(SimEvent::CheckInvariants)
            .expect("schedule ready observation");

        let result = sim.run_scripted_until_idle().await.expect("sim runs");
        assert_eq!(result.report.violations, Vec::new());
        assert_exposure(&sim, &namespace, 1).await;

        sim.schedule_now(SimEvent::RecordInstance(failed))
            .expect("schedule failed status");
        sim.schedule_now(SimEvent::CheckInvariants)
            .expect("schedule failed observation");

        let result = sim.run_scripted_until_idle().await.expect("sim runs");
        assert!(
            result
                .report
                .violations
                .iter()
                .any(|violation| violation.invariant == "runtime_projection"
                    && violation.message.contains("disagrees with status phase"))
        );

        sim.schedule_now(SimEvent::RuntimeStop(ready.instance_id.clone()))
            .expect("schedule failed runtime stop");
        sim.schedule_now(SimEvent::RecordInstance(ready.clone()))
            .expect("schedule recovered status");
        sim.schedule_now(SimEvent::RuntimeStart(ready))
            .expect("schedule recovered runtime");
        sim.schedule_now(SimEvent::CheckInvariants)
            .expect("schedule recovered observation");

        let result = sim.run_scripted_until_idle().await.expect("sim runs");
        assert_eq!(result.report.violations, Vec::new());
        assert_exposure(&sim, &namespace, 1).await;
    }

    #[tokio::test]
    async fn tigerbeetle_style_product_swarm_is_reproducible_by_seed() {
        let seed = 0x5eed_51a7_u64;
        let first = ProductSwarm::new(ProductSwarmOptions::from_seed(seed))
            .run()
            .await
            .expect("first swarm runs");
        let second = ProductSwarm::new(ProductSwarmOptions::from_seed(seed))
            .run()
            .await
            .expect("second swarm runs");

        assert_eq!(first.operations, second.operations);
        assert_eq!(first.report.violations, Vec::new());
        assert_eq!(second.report.violations, Vec::new());
    }

    #[tokio::test]
    async fn tigerbeetle_style_product_swarm_checks_seeded_workload_range() {
        let mut covered = HashSet::new();
        for seed in 0_u64..64 {
            let result = ProductSwarm::new(ProductSwarmOptions::from_seed(seed))
                .run()
                .await
                .expect("swarm runs");
            covered.extend(result.operations.iter().copied());

            assert_eq!(
                result.report.violations,
                Vec::new(),
                "seed {seed} failed after {} ticks with operations {:?}",
                result.ticks,
                result.operations
            );
        }
        for operation in ProductOperation::ALL {
            assert!(
                covered.contains(&operation),
                "seed range did not cover operation {operation:?}"
            );
        }
    }

    #[test]
    fn fixture_deploy_record_uses_committed_state_without_default() {
        let deploy = fixture::deploy(Namespace::default_ns());
        assert_eq!(deploy.deploy_id, DeployId("deploy-1".into()));
        assert_eq!(deploy.state, DeployState::Committed);
    }
}
