use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Mutex,
};

use ployz_core::{
    ContainerRefusal, ContainerRuntimeObservation, DependencyHealthFailure, DockerVolumeId,
    DockerVolumeName, HealthFailure, HealthObservation, LastHealthCheck, MembershipObservation,
    Namespace, RotationTarget, RpcErrorCode, ServiceName, ServiceResult, UpdateOrder,
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
    /// Limits of a running Container changed in place.
    UpdateLimits(MachineId, ContainerId),
}

#[derive(Clone)]
enum Reply {
    Ok,
    Listed(Vec<ContainerObservation>),
    Created(ContainerId),
    Settled(ContainerId, HealthVerdict),
    /// Created once the test's own delay ends.
    CreatedAfter(ContainerId, fn() -> tokio::time::Sleep),
    /// Absent once the test's own delay ends.
    OkAfter(fn() -> tokio::time::Sleep),
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

#[derive(Clone, Debug)]
struct Recorded {
    machine_id: MachineId,
    service: QualifiedService,
    kind: ContainerKind,
    r#gen: Gen,
    /// The Deploy label its create carried, as the daemon writes it.
    deploy: Option<DeploymentLogId>,
    rotation: CopyRotation,
    /// In before the run, so a proxy that never acks still lists it.
    in_before: bool,
    stopped: bool,
    present: bool,
}

impl Recorded {
    fn of(observed: &ContainerObservation) -> Self {
        Self {
            machine_id: observed.machine_id,
            service: observed.identity(),
            kind: observed.kind,
            r#gen: observed.copy_gen(),
            deploy: observed.deploy_label(),
            rotation: observed.rotation().clone(),
            in_before: *observed.rotation() == CopyRotation::In,
            stopped: false,
            present: true,
        }
    }

    /// How a proxy sees this copy: addressed, and serving unless stopped.
    fn proxied(&self, container_id: &ContainerId) -> ContainerObservation {
        let runtime = if self.stopped { exited(0) } else { running() };
        let mut observed = observation(&self.machine_id, container_id, runtime);
        observed
            .try_update(|parts| {
                parts.kind = self.kind;
                parts.namespace = self.service.namespace.clone();
                parts.resolved_spec.name = self.service.name.clone();
                parts.rotation = Some(self.rotation.clone());
                parts.r#gen = Some(self.r#gen);
                parts.address = Some(ployz_core::ContainerAddress("10.210.1.2".parse().unwrap()));
            })
            .unwrap();
        observed
    }

    fn is_sibling_of(&self, other: &Self) -> bool {
        self.kind == ContainerKind::ServiceContainer
            && self.present
            && self.service == other.service
    }
}

#[derive(Clone, Copy, Debug)]
enum Ack {
    /// Lists exactly the In copies on every read.
    Prompt,
    /// Shows the route from before the run for this many reads, then acks.
    After(usize),
    /// Keeps the route from before the run.
    Never,
    /// Its row has not arrived.
    Missing,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Ask {
    In,
    Held,
    Out,
}

impl Ask {
    fn of(target: &RotationTarget) -> Self {
        match target {
            RotationTarget::In { .. } => Self::In,
            RotationTarget::Held(_) => Self::Held,
            RotationTarget::Out | RotationTarget::Stopped | RotationTarget::Gone => Self::Out,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Move {
    To(ContainerId, CopyRotation),
    Gone(ContainerId),
}

#[derive(Clone, Copy, Debug)]
enum Store {
    /// Holds no rows, as a copy that appears only after the read.
    Empty,
    /// Holds a row for every recorded copy, answering Machine or not.
    Copies,
    Fails,
    Silent,
}

enum Decision {
    Met,
    Write(CopyRotation),
    Refused(ContainerRefusal),
}

struct Scripted {
    steps: Mutex<VecDeque<Step>>,
    observations: Option<Vec<ContainerObservation>>,
    observed_specs: BTreeMap<ContainerId, ResolvedServiceSpec>,
    slots: Mutex<Vec<SlotKey>>,
    answers: Mutex<Vec<AnswerWhen>>,
    deploys: Mutex<Vec<Option<ployz_core::DeploymentLogId>>>,
    /// The run each In ask carried, by copy.
    in_runs: Mutex<Vec<(ContainerId, Option<ployz_core::DeploymentLogId>)>>,
    cancel_on_prepare: Option<CancellationToken>,
    /// This call cancels the Deploy once it is made.
    cancel_on: Option<(Call, CancellationToken)>,
    /// This move cancels the Deploy once its owner applies it.
    cancel_on_move: Option<(Move, CancellationToken)>,
    runs_global: bool,
    pull_error: Option<RpcError>,
    /// What each Machine reports of each image it pulls; none when absent.
    pull_replies: BTreeMap<MachineId, ImagePulled>,
    pulled: Mutex<BTreeSet<MachineId>>,
    copies: Mutex<BTreeMap<ContainerId, Recorded>>,
    /// A Machine it omits acks promptly.
    acks: BTreeMap<MachineId, Ack>,
    /// Up Machines the entry couldn't dial, by name, which every listing leaves
    /// unanswered.
    undialed: BTreeMap<MachineId, &'static str>,
    route_reads: Mutex<BTreeMap<MachineId, usize>>,
    down: BTreeSet<MachineId>,
    /// An In ask on this copy leaves it where it is and answers this verdict.
    unhealthy_in: BTreeMap<ContainerId, HealthVerdict>,
    /// An In ask on this copy lands, but its reply never arrives.
    unanswered_in: BTreeSet<ContainerId>,
    /// An In ask on this copy lands and the copy runs, but the Machine answers
    /// with an error, as when its inspect after the write fails.
    answered_error_after_in: BTreeSet<ContainerId>,
    /// An In ask on this copy is answered by the entry Machine's routing, which no
    /// longer lists its Server.
    unrouted_in: BTreeSet<ContainerId>,
    /// A Held ask on this copy lands, but no reply to it, or to a resend,
    /// ever arrives.
    unanswered_held: BTreeSet<ContainerId>,
    /// The reason an unanswered In or Held ask fails with.
    unanswered_reason: &'static str,
    /// This many route reads fail before the next one answers.
    failed_route_reads: Mutex<usize>,
    /// A route read takes a scheduler turn, as an RPC does.
    read_latency: bool,
    /// Another writer moves this copy here just before the run's next ask of
    /// this kind on it.
    moved: Mutex<BTreeMap<(ContainerId, Ask), CopyRotation>>,
    moved_sibling: Mutex<BTreeMap<(ContainerId, Ask), (ContainerId, CopyRotation)>>,
    /// Every live proxy keeps listing a copy of this Service for this many
    /// reads after it leaves In; `usize::MAX` never drops it.
    withheld: BTreeMap<QualifiedService, usize>,
    lingering: Mutex<BTreeMap<ContainerId, usize>>,
    /// This Machine's view of every ack row shows the routes from before the
    /// run for this many of its reads; its owner checks LastIn on that view.
    lagging: Mutex<BTreeMap<MachineId, usize>>,
    /// Route reads answer that the Machine list is not replicated in full.
    incomplete: bool,
    /// Once this move is applied, every route read fails.
    reads_fail_after: Option<Move>,
    /// Once this move is applied, a route read sent to this Machine goes
    /// unanswered, as when it is cut off.
    cut_off_after: Option<(Move, MachineId)>,
    /// This many entry route reads go unanswered before the next one answers.
    blipped_entry_reads: Mutex<usize>,
    /// What each proxy last routed, kept by the real rule.
    served: Mutex<BTreeMap<MachineId, ployz_core::ProxyRoutes>>,
    /// What the entry Machine's store answers.
    store: Store,
    store_reads: Mutex<usize>,
    moves: Mutex<Vec<Move>>,
    /// Each live proxy's route once each move was applied.
    listed: Mutex<Vec<BTreeMap<MachineId, Option<BTreeSet<ContainerId>>>>>,
    /// What dialing the entry again answers; none when this client can't.
    redial: Option<Result<(), RpcError>>,
    redials: Mutex<usize>,
}

impl Scripted {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            observations: None,
            observed_specs: BTreeMap::new(),
            slots: Mutex::new(Vec::new()),
            answers: Mutex::new(Vec::new()),
            deploys: Mutex::new(Vec::new()),
            in_runs: Mutex::new(Vec::new()),
            cancel_on_prepare: None,
            cancel_on: None,
            cancel_on_move: None,
            runs_global: false,
            pull_error: None,
            pull_replies: BTreeMap::new(),
            pulled: Mutex::new(BTreeSet::new()),
            copies: Mutex::new(BTreeMap::new()),
            acks: BTreeMap::new(),
            undialed: BTreeMap::new(),
            route_reads: Mutex::new(BTreeMap::new()),
            down: BTreeSet::new(),
            unhealthy_in: BTreeMap::new(),
            unanswered_in: BTreeSet::new(),
            answered_error_after_in: BTreeSet::new(),
            unrouted_in: BTreeSet::new(),
            unanswered_held: BTreeSet::new(),
            unanswered_reason: "no answer before the deadline",
            failed_route_reads: Mutex::new(0),
            read_latency: false,
            moved: Mutex::new(BTreeMap::new()),
            moved_sibling: Mutex::new(BTreeMap::new()),
            withheld: BTreeMap::new(),
            lingering: Mutex::new(BTreeMap::new()),
            lagging: Mutex::new(BTreeMap::new()),
            incomplete: false,
            reads_fail_after: None,
            cut_off_after: None,
            blipped_entry_reads: Mutex::new(0),
            served: Mutex::new(BTreeMap::new()),
            store: Store::Empty,
            store_reads: Mutex::new(0),
            moves: Mutex::new(Vec::new()),
            listed: Mutex::new(Vec::new()),
            redial: None,
            redials: Mutex::new(0),
        }
    }

    fn with_copy(
        self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        rotation: CopyRotation,
    ) -> Self {
        self.with_copy_at(&api(), machine_id, container_id, rotation, Gen::FIRST)
    }

    fn with_copy_at(
        mut self,
        service: &QualifiedService,
        machine_id: &MachineId,
        container_id: &ContainerId,
        rotation: CopyRotation,
        r#gen: Gen,
    ) -> Self {
        self.copies.get_mut().unwrap().insert(
            *container_id,
            Recorded {
                machine_id: *machine_id,
                service: service.clone(),
                kind: ContainerKind::ServiceContainer,
                r#gen,
                deploy: None,
                in_before: rotation == CopyRotation::In,
                rotation,
                stopped: false,
                present: true,
            },
        );
        self
    }

    /// A copy of `api` that `machine_id` no longer has: a stop or remove of it is
    /// refused as not found, as a daemon whose Leave took it answers.
    fn with_copy_gone(mut self, machine_id: &MachineId, container_id: &ContainerId) -> Self {
        self = self.with_copy(machine_id, container_id, CopyRotation::Out);
        self.record(*container_id, |copy| copy.present = false);
        self
    }

    /// A route read through `machine_id`, or through the entry when `None`.
    async fn read_at(
        &self,
        machine_id: Option<&MachineId>,
        service: &QualifiedService,
    ) -> Result<RoutesRead, RpcError> {
        if self.read_latency {
            tokio::task::yield_now().await;
        }
        {
            let mut failing = self.failed_route_reads.lock().unwrap();
            if *failing > 0 {
                *failing -= 1;
                return Err(error("the route query failed"));
            }
        }
        if self
            .reads_fail_after
            .as_ref()
            .is_some_and(|applied| self.moves().contains(applied))
        {
            return Err(unavailable("the route read failed"));
        }
        if self
            .cut_off_after
            .as_ref()
            .is_some_and(|(applied, cut)| Some(cut) == machine_id && self.moves().contains(applied))
        {
            return Err(RpcError::unanswered_target(
                RpcErrorCode::Unavailable,
                "target Machine RPC timed out".into(),
            ));
        }
        let routes = if machine_id.is_some_and(|machine_id| self.lags(machine_id, true)) {
            self.before(service)
        } else {
            self.routes(service, true)
        };
        Ok(RoutesRead {
            routes,
            complete: !self.incomplete,
            down: self.down.clone(),
        })
    }

    /// Every proxy already routes the In copies of `service`, as it would
    /// before the run, so the rule's kept route applies from the first read.
    fn routing(self, service: &QualifiedService) -> Self {
        self.routes(service, false);
        self
    }

    fn with_deploy(mut self, container_id: &ContainerId, deploy: &DeploymentLogId) -> Self {
        self.copies
            .get_mut()
            .unwrap()
            .get_mut(container_id)
            .expect("deploy label on a recorded copy")
            .deploy = Some(deploy.clone());
        self
    }

    fn rotation(&self, container_id: &ContainerId) -> Option<CopyRotation> {
        self.copies
            .lock()
            .unwrap()
            .get(container_id)
            .filter(|copy| copy.present)
            .map(|copy| copy.rotation.clone())
    }

    fn moves(&self) -> Vec<Move> {
        self.moves.lock().unwrap().clone()
    }

    fn brought_in(&self) -> Vec<ContainerId> {
        self.moves()
            .into_iter()
            .filter_map(|applied| match applied {
                Move::To(id, CopyRotation::In) => Some(id),
                Move::To(..) | Move::Gone(_) => None,
            })
            .collect()
    }

    fn routes_at(&self, applied: &Move) -> BTreeMap<MachineId, Option<BTreeSet<ContainerId>>> {
        let at = self
            .moves()
            .iter()
            .position(|other| other == applied)
            .unwrap_or_else(|| panic!("{applied:?} was never applied"));
        self.listed
            .lock()
            .unwrap()
            .get(at)
            .cloned()
            .unwrap_or_else(|| panic!("no routes listed at {applied:?}"))
    }

    fn answers(&self) -> Vec<AnswerWhen> {
        self.answers.lock().unwrap().clone()
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

    fn recorded(&self, mut observed: ContainerObservation) -> ContainerObservation {
        let mut copies = self.copies.lock().unwrap();
        let copy = copies
            .entry(observed.container_id)
            .or_insert_with(|| Recorded::of(&observed));
        let (rotation, r#gen, deploy) = (copy.rotation.clone(), copy.r#gen, copy.deploy.clone());
        observed
            .try_update(|parts| {
                parts.rotation = Some(rotation);
                parts.r#gen = Some(r#gen);
                if let Some(deploy) = deploy {
                    parts
                        .labels
                        .insert(ployz_core::DEPLOYMENT_ID_LABEL.into(), deploy.to_string());
                }
            })
            .unwrap();
        observed
    }

    fn record(&self, container_id: ContainerId, change: impl FnOnce(&mut Recorded)) {
        if let Some(copy) = self.copies.lock().unwrap().get_mut(&container_id) {
            change(copy);
        }
    }

    fn applied(&self, applied: Move) {
        let (Move::To(id, _) | Move::Gone(id)) = &applied;
        let service = self
            .copies
            .lock()
            .unwrap()
            .get(id)
            .map(|copy| copy.service.clone());
        let listed = service.map_or_else(BTreeMap::new, |service| self.routes(&service, false));
        self.listed.lock().unwrap().push(listed);
        self.moves.lock().unwrap().push(applied.clone());
        if let Some((at, cancel)) = &self.cancel_on_move
            && *at == applied
        {
            cancel.cancel();
        }
    }

    fn proxies(&self) -> BTreeSet<MachineId> {
        let copies = self.copies.lock().unwrap();
        self.pulled
            .lock()
            .unwrap()
            .iter()
            .copied()
            .chain(copies.values().map(|copy| copy.machine_id))
            .chain(self.acks.keys().copied())
            .collect()
    }

    /// The Cluster as a Deploy reads it: every present copy, each on a Machine
    /// that is Up.
    fn snapshot(&self) -> crate::deploy::DeploySnapshot {
        let copies = self.copies.lock().unwrap();
        let machines = copies
            .values()
            .map(|copy| copy.machine_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|id| {
                ployz_core::MachineObservation::new(machine_record(id), MembershipObservation::Up)
            })
            .collect();
        let containers = copies
            .iter()
            .filter(|(_, copy)| copy.present)
            .map(|(id, copy)| copy.proxied(id))
            .collect();
        crate::deploy::DeploySnapshot {
            machines,
            containers,
            ..Default::default()
        }
    }

    /// `advance` counts the read toward a delayed ack. A proxy that acks
    /// routes by the real proxy rule, then still lists a withheld copy.
    fn routes(
        &self,
        service: &QualifiedService,
        advance: bool,
    ) -> BTreeMap<MachineId, Option<BTreeSet<ContainerId>>> {
        let proxies = self.proxies();
        let copies = self.copies.lock().unwrap();
        let of_service = || copies.iter().filter(|(_, copy)| copy.service == *service);
        let siblings = ployz_core::service_containers(
            of_service()
                .filter(|(_, copy)| copy.present)
                .map(|(id, copy)| copy.proxied(id)),
        );
        let mut lingering = self.lingering.lock().unwrap();
        let withheld: BTreeSet<ContainerId> = of_service()
            .filter(|(id, copy)| {
                lingering.get(*id).is_some_and(|reads| *reads > 0)
                    && !self.down.contains(&copy.machine_id)
            })
            .map(|(id, _)| *id)
            .collect();
        if advance {
            for (id, reads) in lingering.iter_mut() {
                if copies.get(id).is_some_and(|copy| copy.service == *service) {
                    *reads = reads.saturating_sub(1);
                }
            }
        }
        drop(lingering);
        let before: BTreeSet<ContainerId> = of_service()
            .filter(|(_, copy)| copy.in_before)
            .map(|(id, _)| *id)
            .collect();
        drop(copies);
        let mut served = self.served.lock().unwrap();
        let mut now = |machine: MachineId| {
            let current = served.entry(machine).or_default();
            let next =
                ployz_core::proxy_routes(&siblings, |down| self.down.contains(down), current);
            let route = next.get(service).cloned().unwrap_or_default();
            let listed = route
                .iter()
                .map(|serving| serving.as_observation().container_id)
                .chain(withheld.iter().copied())
                .collect();
            if next.contains_key(service) {
                current.insert(service.clone(), route);
            } else {
                current.remove(service);
            }
            listed
        };
        let mut reads = self.route_reads.lock().unwrap();
        proxies
            .into_iter()
            .filter(|machine| !self.down.contains(machine))
            .map(|machine| {
                let route = match self.acks.get(&machine).copied().unwrap_or(Ack::Prompt) {
                    Ack::Prompt => Some(now(machine)),
                    Ack::After(reads_before) => {
                        let read = reads.entry(machine).or_default();
                        if advance {
                            *read += 1;
                        }
                        Some(if *read > reads_before {
                            now(machine)
                        } else {
                            before.clone()
                        })
                    }
                    Ack::Never => Some(before.clone()),
                    Ack::Missing => None,
                };
                (machine, route)
            })
            .collect()
    }

    /// `advance` counts the read toward the view catching up.
    fn lags(&self, machine_id: &MachineId, advance: bool) -> bool {
        let mut lagging = self.lagging.lock().unwrap();
        let Some(reads) = lagging.get_mut(machine_id).filter(|reads| **reads > 0) else {
            return false;
        };
        if advance {
            *reads -= 1;
        }
        true
    }

    /// Every live proxy's route from before the run, as a lagging view shows it.
    fn before(
        &self,
        service: &QualifiedService,
    ) -> BTreeMap<MachineId, Option<BTreeSet<ContainerId>>> {
        let listed: BTreeSet<ContainerId> = self
            .copies
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, copy)| copy.service == *service && copy.in_before)
            .map(|(id, _)| *id)
            .collect();
        self.proxies()
            .into_iter()
            .filter(|machine| !self.down.contains(machine))
            .map(|machine| (machine, Some(listed.clone())))
            .collect()
    }

    fn decide(
        &self,
        machine_id: &MachineId,
        container_id: ContainerId,
        target: &RotationTarget,
        expect: Option<&CopyRotation>,
        allow_empty: bool,
    ) -> Decision {
        let raced = self
            .moved
            .lock()
            .unwrap()
            .remove(&(container_id, Ask::of(target)));
        if let Some(rotation) = &raced {
            self.record(container_id, |copy| copy.rotation = rotation.clone());
        }
        let raced_sibling = self
            .moved_sibling
            .lock()
            .unwrap()
            .remove(&(container_id, Ask::of(target)));
        if let Some((sibling, rotation)) = raced_sibling {
            self.record(sibling, |copy| copy.rotation = rotation);
        }
        let copy = {
            let mut copies = self.copies.lock().unwrap();
            copies
                .entry(container_id)
                .or_insert_with(|| Recorded::of(&observation(machine_id, &container_id, running())))
                .clone()
        };
        if !copy.present {
            return Decision::Refused(refusal(ContainerRefusalReason::NotFound, "no such copy"));
        }
        let met = match (&copy.rotation, target) {
            (CopyRotation::In, RotationTarget::In { .. })
            | (CopyRotation::Out, RotationTarget::Out) => true,
            (CopyRotation::Held(held), RotationTarget::Held(run)) => held == run,
            _ => false,
        };
        if met {
            return Decision::Met;
        }
        if expect.is_some_and(|expect| *expect != copy.rotation) {
            return Decision::Refused(refusal(
                ContainerRefusalReason::RotationMismatch,
                "the copy moved since it was read",
            ));
        }
        let to = match copy.rotation.edge(target) {
            ployz_core::RotationEdge::Write(to) => to,
            ployz_core::RotationEdge::Refused(refused) => {
                return Decision::Refused(edge_refusal(refused));
            }
            edge @ (ployz_core::RotationEdge::Keep | ployz_core::RotationEdge::Remove) => {
                panic!("no rotation ask reaches {edge:?}")
            }
        };
        let copies = self.copies.lock().unwrap();
        let siblings = || {
            copies.iter().filter(|(id, sibling)| {
                **id != container_id
                    && sibling.is_sibling_of(&copy)
                    && sibling.rotation == CopyRotation::In
            })
        };
        if matches!(target, RotationTarget::In { .. })
            && siblings().any(|(_, sibling)| sibling.r#gen > copy.r#gen)
        {
            return Decision::Refused(refusal(
                ContainerRefusalReason::OlderThanIn,
                "a newer copy is In",
            ));
        }
        if matches!(target, RotationTarget::Out)
            && copy.rotation == CopyRotation::In
            && !allow_empty
        {
            let live: Vec<ContainerId> = siblings()
                .filter(|(_, sibling)| !self.down.contains(&sibling.machine_id))
                .map(|(id, _)| *id)
                .collect();
            drop(copies);
            let routes = if self.lags(machine_id, false) {
                self.before(&copy.service)
            } else {
                self.routes(&copy.service, false)
            };
            let acked = live.iter().any(|sibling| {
                routes
                    .values()
                    .all(|route| route.as_ref().is_some_and(|route| route.contains(sibling)))
            });
            if !acked {
                return Decision::Refused(refusal(
                    ContainerRefusalReason::LastIn,
                    "no other In copy is acked by every live proxy",
                ));
            }
        }
        Decision::Write(to)
    }

    fn now(&self, machine_id: &MachineId, container_id: &ContainerId) -> ContainerNow {
        ContainerNow::Present(self.recorded(observation(machine_id, container_id, running())))
    }

    async fn rotate(
        &self,
        machine_id: &MachineId,
        container_id: ContainerId,
        target: RotationTarget,
        expect: Option<CopyRotation>,
        allow_empty: bool,
    ) -> Result<EnsuredContainer, RpcError> {
        let to = match self.decide(
            machine_id,
            container_id,
            &target,
            expect.as_ref(),
            allow_empty,
        ) {
            Decision::Met
                if matches!(target, RotationTarget::Held(_))
                    && self.unanswered_held.contains(&container_id) =>
            {
                return Err(RpcError::unanswered_target(
                    RpcErrorCode::Unavailable,
                    self.unanswered_reason.into(),
                ));
            }
            Decision::Met => {
                return Ok(EnsuredContainer::Ensured {
                    now: self.now(machine_id, &container_id),
                    health: None,
                });
            }
            Decision::Refused(refusal) => {
                return Ok(EnsuredContainer::Refused {
                    now: self.now(machine_id, &container_id),
                    refusal,
                });
            }
            Decision::Write(to) => to,
        };
        let routing = to == CopyRotation::In;
        if routing && self.unrouted_in.contains(&container_id) {
            return Err(no_server_named(machine_id));
        }
        if routing {
            let stopped = self
                .copies
                .lock()
                .unwrap()
                .get(&container_id)
                .is_some_and(|copy| copy.stopped);
            let verdict = if stopped {
                match self.next(Call::Restart(*machine_id, container_id)) {
                    Reply::Ok | Reply::Created(_) => None,
                    Reply::Settled(_, verdict) => Some(verdict),
                    Reply::Error(error) => return Err(error),
                    Reply::Pending => std::future::pending().await,
                    Reply::Listed(_)
                    | Reply::CreatedAfter(..)
                    | Reply::OkAfter(_)
                    | Reply::Observed(..)
                    | Reply::ObservedIngress(_) => {
                        panic!("scripted restart requires Ok, Settled or Error")
                    }
                }
            } else {
                self.unhealthy_in.get(&container_id).cloned()
            };
            self.record(container_id, |copy| copy.stopped = false);
            if let Some(verdict) = verdict.filter(|verdict| *verdict != HealthVerdict::Healthy) {
                return Ok(EnsuredContainer::Ensured {
                    now: self.now(machine_id, &container_id),
                    health: Some(verdict),
                });
            }
        }
        let mut left_in = None;
        self.record(container_id, |copy| {
            left_in = (copy.rotation == CopyRotation::In && to != CopyRotation::In)
                .then(|| copy.service.clone());
            copy.rotation = to.clone();
        });
        if let Some(reads) = left_in.and_then(|service| self.withheld.get(&service)) {
            self.lingering.lock().unwrap().insert(container_id, *reads);
        }
        let holding = matches!(to, CopyRotation::Held(_));
        self.applied(Move::To(container_id, to));
        if routing && self.answered_error_after_in.contains(&container_id) {
            return Err(error("Docker inspect failed after writing In"));
        }
        if routing && self.unanswered_in.contains(&container_id)
            || holding && self.unanswered_held.contains(&container_id)
        {
            return Err(RpcError::unanswered_target(
                RpcErrorCode::Unavailable,
                self.unanswered_reason.into(),
            ));
        }
        Ok(EnsuredContainer::Ensured {
            now: self.now(machine_id, &container_id),
            health: routing.then_some(HealthVerdict::Healthy),
        })
    }

    async fn converge(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
    ) -> Result<EnsuredContainer, RpcError> {
        let machine = *machine_id;
        let lifecycle = match &want {
            ContainerWant::Stopped {
                container: ContainerLocator::Id(id),
                ..
            } => Some((*id, RotationTarget::Stopped)),
            ContainerWant::Gone {
                container: ContainerLocator::Id(id),
                ..
            } => Some((*id, RotationTarget::Gone)),
            ContainerWant::Running { .. }
            | ContainerWant::Stopped { .. }
            | ContainerWant::Gone { .. }
            | ContainerWant::Out { .. }
            | ContainerWant::Held { .. } => None,
        };
        if let Some((id, target)) = &lifecycle {
            let copy = self.copies.lock().unwrap().get(id).cloned();
            match copy {
                Some(copy) if !copy.present => {
                    return Ok(EnsuredContainer::Refused {
                        now: ContainerNow::Absent {
                            of: ContainerLocator::Id(*id),
                        },
                        refusal: refusal(ContainerRefusalReason::NotFound, "no such copy"),
                    });
                }
                Some(copy) => {
                    if let ployz_core::RotationEdge::Refused(refused) = copy.rotation.edge(target) {
                        return Ok(EnsuredContainer::Refused {
                            now: self.now(machine_id, id),
                            refusal: edge_refusal(refused),
                        });
                    }
                }
                None => {}
            }
        }
        let mut settled = false;
        let mut created = None;
        let (call, of) = match want {
            ContainerWant::Running {
                container: RunTarget::New(new),
                ..
            } => {
                let call = Call::Run(machine, new.slot.kind);
                settled = new.answer == AnswerWhen::Settled;
                self.answers.lock().unwrap().push(new.answer);
                self.deploys.lock().unwrap().push(new.deployment_id.clone());
                self.slots.lock().unwrap().push(new.slot.clone());
                created = Some((
                    QualifiedService::new(
                        new.slot.namespace.clone(),
                        new.resolved_spec.name.clone(),
                    ),
                    new.slot.kind,
                    new.r#gen,
                    new.deployment_id.clone(),
                ));
                (call, ContainerLocator::Key(new.slot))
            }
            ContainerWant::Running {
                container: RunTarget::Existing(locator),
                ..
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
            want @ (ContainerWant::Out { .. } | ContainerWant::Held { .. }) => {
                panic!("a rotation ask goes through ensure_unsettled: {want:?}")
            }
        };
        if let Some((at, cancel)) = &self.cancel_on
            && *at == call
        {
            cancel.cancel();
        }
        let healthy = settled.then_some(HealthVerdict::Healthy);
        let reply = self.next(call.clone());
        let container_id = match &reply {
            Reply::Created(id) | Reply::Settled(id, _) | Reply::CreatedAfter(id, _) => Some(*id),
            Reply::Ok
            | Reply::OkAfter(_)
            | Reply::Listed(_)
            | Reply::Observed(..)
            | Reply::ObservedIngress(_)
            | Reply::Pending
            | Reply::Error(_) => None,
        };
        if let (Some((service, kind, r#gen, deploy)), Some(id)) = (created, container_id) {
            self.copies.lock().unwrap().insert(
                id,
                Recorded {
                    machine_id: machine,
                    service,
                    kind,
                    r#gen,
                    deploy,
                    rotation: CopyRotation::Pending,
                    in_before: false,
                    stopped: false,
                    present: true,
                },
            );
        }
        let ensured = |now, health| Ok(EnsuredContainer::Ensured { now, health });
        let present = |container_id: &ContainerId| self.now(machine_id, container_id);
        let answer = match reply {
            Reply::Created(container_id) => ensured(present(&container_id), healthy),
            Reply::Settled(container_id, verdict) => {
                assert!(settled, "only a Settled creation answers a verdict");
                ensured(present(&container_id), Some(verdict))
            }
            Reply::CreatedAfter(container_id, delay) => {
                delay().await;
                ensured(present(&container_id), healthy)
            }
            Reply::Ok => ensured(ContainerNow::Absent { of }, None),
            Reply::OkAfter(delay) => {
                delay().await;
                ensured(ContainerNow::Absent { of }, None)
            }
            Reply::Error(error) => Err(error),
            Reply::Pending => std::future::pending().await,
            Reply::Listed(_) | Reply::Observed(..) | Reply::ObservedIngress(_) => {
                panic!("scripted ensure requires Created, Ok or Error")
            }
        };
        if answer.is_ok() {
            match call {
                Call::Stop(_, id) | Call::StopNow(_, id) | Call::Kill(_, id) => {
                    self.record(id, |copy| copy.stopped = true);
                }
                Call::Restart(_, id) => self.record(id, |copy| copy.stopped = false),
                Call::Remove(_, id) | Call::Discard(_, id) => {
                    self.record(id, |copy| copy.present = false);
                    self.applied(Move::Gone(id));
                }
                Call::Prepare(_)
                | Call::List(_)
                | Call::Run(..)
                | Call::Inspect(..)
                | Call::DiscardSlot(..)
                | Call::RemoveVolume(_)
                | Call::UpdateLimits(..) => {}
            }
        }
        answer
    }
}

fn refusal(reason: ContainerRefusalReason, message: &str) -> ContainerRefusal {
    ContainerRefusal {
        reason,
        message: message.into(),
    }
}

fn edge_refusal(refused: ployz_core::RotationRefusal) -> ContainerRefusal {
    use ployz_core::RotationRefusal;
    match refused {
        RotationRefusal::HeldByOtherRun => refusal(
            ContainerRefusalReason::HeldByOtherRun,
            "another run holds the copy",
        ),
        RotationRefusal::InRotation => refusal(
            ContainerRefusalReason::InRotation,
            "take the copy out of routing first",
        ),
        RotationRefusal::OutIsFinal | RotationRefusal::NoEdge => refusal(
            ContainerRefusalReason::RotationMismatch,
            "no move leads there",
        ),
    }
}

impl MachineOperations for Scripted {
    async fn redial(&self) -> Option<Result<(), RpcError>> {
        let answer = self.redial.clone()?;
        *self.redials.lock().unwrap() += 1;
        Some(answer)
    }

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
            return Ok(observations
                .iter()
                .cloned()
                .map(|observed| self.recorded(observed))
                .collect());
        }
        let scripted = matches!(
            self.steps.lock().unwrap().front(),
            Some(Step(Call::List(listed), _)) if listed == service
        );
        if !scripted {
            return Ok(Vec::new());
        }
        match self.next(Call::List(service.clone())) {
            Reply::Listed(containers) => Ok(containers
                .into_iter()
                .map(|observed| self.recorded(observed))
                .collect()),
            Reply::Error(error) => Err(error),
            Reply::Ok
            | Reply::Created(_)
            | Reply::Settled(..)
            | Reply::CreatedAfter(..)
            | Reply::OkAfter(_)
            | Reply::Observed(..)
            | Reply::ObservedIngress(_)
            | Reply::Pending => {
                panic!("scripted list requires Listed or Error")
            }
        }
    }

    async fn listed_copies(&self, service: &QualifiedService) -> Result<Listed, RpcError> {
        let unanswered = self
            .undialed
            .iter()
            .map(|(machine_id, name)| Unanswered {
                name: ployz_core::MachineName::parse(*name).unwrap(),
                failure: ployz_core::MachineFailure {
                    machine_id: *machine_id,
                    error: RpcError::unanswered_target(
                        RpcErrorCode::Unavailable,
                        "tcp connect error".into(),
                    ),
                },
                message: "tcp connect error".into(),
            })
            .collect();
        Ok(Listed {
            copies: self.service_containers(service).await?,
            unanswered,
        })
    }

    async fn replicated_containers(&self) -> Result<Vec<ContainerObservation>, RpcError> {
        *self.store_reads.lock().unwrap() += 1;
        match self.store {
            Store::Empty => Ok(Vec::new()),
            Store::Copies => Ok(self
                .copies
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, copy)| copy.present)
                .map(|(id, copy)| copy.proxied(id))
                .collect()),
            Store::Fails => Err(unavailable("the store read failed")),
            Store::Silent => std::future::pending().await,
        }
    }

    async fn ensure_container(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
    ) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError> {
        self.converge(machine_id, want).await?.into_settled()
    }

    async fn ensure_unsettled(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        _timeout: Option<std::time::Duration>,
    ) -> Result<EnsuredContainer, RpcError> {
        match want {
            ContainerWant::Running {
                container: RunTarget::Existing(locator),
                rotation: RunRotation::In { run, expect },
            } => {
                self.in_runs
                    .lock()
                    .unwrap()
                    .push((id_of(&locator), run.clone()));
                self.rotate(
                    machine_id,
                    id_of(&locator),
                    RotationTarget::In { run },
                    expect,
                    false,
                )
                .await
            }
            ContainerWant::Held {
                container,
                run,
                expect,
            } => {
                self.rotate(
                    machine_id,
                    id_of(&container),
                    RotationTarget::Held(run),
                    expect,
                    false,
                )
                .await
            }
            ContainerWant::Out {
                container,
                allow_empty,
                expect,
            } => {
                self.rotate(
                    machine_id,
                    id_of(&container),
                    RotationTarget::Out,
                    expect,
                    allow_empty,
                )
                .await
            }
            want @ (ContainerWant::Running { .. }
            | ContainerWant::Stopped { .. }
            | ContainerWant::Gone { .. }) => self.converge(machine_id, want).await,
        }
    }

    async fn read_routes(
        &self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<RoutesRead, RpcError> {
        self.read_at(Some(machine_id), service).await
    }

    async fn read_entry_routes(&self, service: &QualifiedService) -> Result<RoutesRead, RpcError> {
        {
            let mut blipped = self.blipped_entry_reads.lock().unwrap();
            if *blipped > 0 {
                *blipped -= 1;
                return Err(RpcError::unanswered_target(
                    RpcErrorCode::Unavailable,
                    "target Machine RPC timed out".into(),
                ));
            }
        }
        self.read_at(None, service).await
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerDetails, RpcError> {
        match self.next(Call::Inspect(*machine_id, *container_id)) {
            Reply::Observed(runtime, healthcheck, last_check) => {
                let mut container = observation(machine_id, container_id, runtime);
                if let Some(spec) = self.observed_specs.get(container_id) {
                    container
                        .try_update(|parts| parts.resolved_spec = spec.clone())
                        .unwrap();
                }
                container
                    .try_update(|parts| parts.effective_healthcheck = healthcheck)
                    .unwrap();
                Ok(ContainerDetails {
                    container: self.recorded(container),
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
                    container: self.recorded(container),
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
            | Reply::CreatedAfter(..)
            | Reply::OkAfter(_) => {
                panic!("scripted inspect requires Observed or Error")
            }
        }
    }

    async fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError> {
        unit(self.next(Call::RemoveVolume(id.clone())))
    }

    async fn update_container_limits(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerLimitsUpdated, RpcError> {
        unit(self.next(Call::UpdateLimits(*machine_id, *container_id))).map(|()| {
            ContainerLimitsUpdated {
                container_id: *container_id,
                limits: spec.container.resources.limits(),
            }
        })
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
    ) -> Result<ImagePulled, RpcError> {
        self.pulled.lock().unwrap().insert(*machine_id);
        self.pull_error.clone().map_or_else(
            || {
                Ok(self
                    .pull_replies
                    .get(machine_id)
                    .cloned()
                    .unwrap_or_default())
            },
            Err,
        )
    }
}

fn unit(reply: Reply) -> Result<(), RpcError> {
    match reply {
        Reply::Ok => Ok(()),
        Reply::Error(error) => Err(error),
        Reply::Listed(_)
        | Reply::Created(_)
        | Reply::Settled(..)
        | Reply::CreatedAfter(..)
        | Reply::OkAfter(_)
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
#[path = "exec_tests/gen_floor.rs"]
mod gen_floor;
#[path = "exec_tests/health.rs"]
mod health;
#[path = "exec_tests/hooks.rs"]
mod hooks;
#[path = "exec_tests/image_ports.rs"]
mod image_ports;
#[path = "exec_tests/limits.rs"]
mod limits;
#[path = "exec_tests/moving.rs"]
mod moving;
#[path = "exec_tests/replacement.rs"]
mod replacement;
#[path = "exec_tests/restart.rs"]
mod restart;
#[path = "exec_tests/silent.rs"]
mod silent;
#[path = "exec_tests/user_commands.rs"]
mod user_commands;

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
        rotation: None,
        r#gen: None,
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
        stopped: false,
        run: None,
    })
    .unwrap()
}

fn routed(
    machine_id: &MachineId,
    container_id: &ContainerId,
    runtime: ContainerRuntimeObservation,
) -> ContainerObservation {
    let mut observation = observation(machine_id, container_id, runtime);
    observation
        .try_update(|parts| parts.rotation = Some(CopyRotation::In))
        .unwrap();
    observation
}

fn serving(
    machine_id: &MachineId,
    container_id: &ContainerId,
    deploy: Option<&str>,
) -> ContainerObservation {
    let mut observation = routed(machine_id, container_id, running());
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

fn machine_record(id: MachineId) -> ployz_core::Machine {
    let hex = id.to_string().chars().next().unwrap();
    ployz_core::Machine {
        id,
        name: ployz_core::MachineName::parse(format!("machine-{hex}")).unwrap(),
        subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
            .parse()
            .unwrap(),
        public_key: ployz_core::WireGuardPublicKey([hex as u8; 32]),
        public_ip: None,
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
        build_concurrency: None,
    }
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

/// What the entry Machine's routing answers for a Server it no longer lists.
fn no_server_named(machine_id: &MachineId) -> RpcError {
    RpcError {
        code: RpcErrorCode::NotFound,
        message: format!("No Server named {:?}", machine_id.to_string()),
        details: serde_json::json!({
            "inspect": ["ployz server ls"],
            "reason": "no_such_server",
            "targets": [machine_id.to_string()],
        }),
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
                oom_killed: false,
                departing: None,
            },
        ),
    )
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

/// Server lc-3, the one a Leave is removing in these tests.
fn leaving() -> ployz_core::MachineName {
    ployz_core::MachineName::parse("lc-3").unwrap()
}

/// The mark a Leave of Server lc-3 leaves on an answer.
fn departure() -> ployz_core::Departure {
    ployz_core::Departure { server: leaving() }
}

/// A Failed verdict, the copy exited 0 as a Leave's stop leaves it, marked with the
/// Leave of Server lc-3.
fn exited_leaving() -> HealthVerdict {
    HealthVerdict::Failed {
        observation: exited(0),
        last_check: None,
        oom_killed: false,
        departing: Some(departure()),
    }
}

/// A client that can't dial its entry again.
async fn no_redial() -> Option<Result<(), RpcError>> {
    None
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

/// The client heard nothing back from the call's target Machine.
fn failed_unanswered(call: Call, message: &str) -> Step {
    Step(
        call,
        Reply::Error(RpcError::unanswered_target(
            RpcErrorCode::Unavailable,
            message.into(),
        )),
    )
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

#[tokio::test(start_paused = true)]
async fn a_lost_answer_outlasts_a_resend_that_never_reached_the_server() {
    let machine = machine('1');
    // `call_retried` keeps a lost answer only when it was worth retrying.
    let lost = || {
        RpcError {
            code: RpcErrorCode::Unavailable,
            message: "connection lost".into(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        }
        .reply_lost_from(&machine)
    };
    let unreached = || {
        crate::cluster::answered_by(
            &machine,
            crate::connect::ConnectError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "connection refused",
            )),
        )
    };
    let mut replies = vec![unreached(), lost()];
    let error = wait_out_restart(
        &CancellationToken::new(),
        || {
            let reply = replies.pop().unwrap_or_else(unreached);
            async move { Err::<(), _>(reply) }
        },
        no_redial,
    )
    .await
    .unwrap_err();
    assert!(error.reply_lost(), "{error:?}");
}

#[test]
fn an_earlier_lost_answer_never_hides_a_silent_target() {
    let lost = crate::connect::ConnectError::from(tonic::Status::from_error(Box::new(
        std::io::Error::new(std::io::ErrorKind::NotConnected, "connection lost"),
    )));
    let silent = crate::connect::ConnectError::Rpc(crate::connect::TransportError::budget_expired(
        "target Machine RPC timed out",
    ));
    assert!(crate::connect::keep_lost_call(Some(lost), silent).target_unanswered());
}

#[tokio::test(start_paused = true)]
async fn a_lost_answer_never_hides_a_closed_session_or_a_refused_key() {
    let machine = machine('1');
    let lost = RpcError {
        code: RpcErrorCode::Unavailable,
        message: "connection lost".into(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
    .reply_lost_from(&machine);
    let closed = RpcError {
        code: RpcErrorCode::Unavailable,
        message: "transport session closed".into(),
        details: serde_json::json!({ "reason": "session_closed" }),
        cause: Vec::new(),
    }
    .unreached();
    let refused_key = crate::cluster::answered_by(&machine, {
        use std::os::unix::process::ExitStatusExt;
        crate::connect::ConnectError::SshProbe {
            target: "host".to_owned(),
            status: std::process::ExitStatus::from_raw(255 << 8),
            failure: crate::connect::SshFailure::Auth,
            detail: "Permission denied (publickey)".into(),
        }
    });

    let started = tokio::time::Instant::now();
    let mut replies = vec![closed, lost.clone()];
    let error = wait_out_restart(
        &CancellationToken::new(),
        || {
            let reply = replies.pop().expect("no resend after a closed session");
            async move { Err::<(), _>(reply) }
        },
        no_redial,
    )
    .await
    .unwrap_err();
    assert!(error.reply_lost(), "{error:?}");
    assert!(started.elapsed() < RESTART_WAIT, "{:?}", started.elapsed());

    let mut replies = vec![refused_key, lost];
    let error = wait_out_restart(
        &CancellationToken::new(),
        || {
            let reply = replies.pop().expect("no resend after a refused key");
            async move { Err::<(), _>(reply) }
        },
        no_redial,
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("Permission denied"), "{error:?}");
}
