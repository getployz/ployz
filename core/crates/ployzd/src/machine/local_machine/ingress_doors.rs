//! Placing this Machine's Ingress doors: the holder that owns the Ingress Proxy's
//! network namespace and host ports, the Caddy that joins it, and the Tunnel
//! Connector that joins it when the Tunnel Door is open.
//!
//! The route reads what Docker has, plans the whole change, then carries it out one
//! stage at a time. Every Container it replaces is stopped first and removed only once
//! the new doors run, so a replacement that fails leaves the old ones to start again.
//! The holder goes before the Caddy and the connector, since both are created in its
//! network. A Caddy from before the holder published the ports itself, so it is stopped
//! before the holder starts.

use std::time::Duration;

use ployz_core::{
    Ciphertext, ContainerId, ContainerKind, ContainerRuntimeObservation, DoorChange, DoorOutcome,
    Gen, IngressDoor, IngressDoorsFailure, IngressDoorsSet, IngressPorts, IngressStage, Namespace,
    ResolvedServiceSpec, RpcError, RpcErrorCode, ServiceId, SetIngressDoorsRequest,
    global_creation_key, tunnel_sealed_token,
};
use zeroize::Zeroizing;

use super::{Error, LocalMachine};
use crate::docker::{ContainerRuntime, Door, DoorEntry, same_creation};
use crate::docker_image::ImagePull;
use crate::images::DoorSpecs;

/// An opened tunnel token, written for the connector before it is placed.
pub(super) type TunnelToken = Zeroizing<Vec<u8>>;

/// How long a started door may take to be observed running. The holder is a pause
/// process and Caddy binds its listeners at once, so this is a bound, not a budget.
const DOOR_START_WAIT: Duration = Duration::from_secs(15);
const DOOR_POLL: Duration = Duration::from_millis(100);

/// What the route does with the holder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum HolderStep {
    /// A canonical holder runs.
    Keep(ContainerId),
    /// A canonical holder exists at rest.
    Start(ContainerId),
    Create(Replacing),
}

/// What the route does with a door in the holder's network: the Caddy, or the
/// Tunnel Connector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum JoinedStep {
    /// A canonical one runs in the holder's network.
    Keep(ContainerId),
    /// A canonical one exists at rest.
    Start(ContainerId),
    /// A canonical one runs in a network the holder no longer has.
    Rejoin(ContainerId),
    Create(Replacing),
}

/// The newest Container of the same door a creation replaces. The old one keeps its
/// name until the new one runs, so the new one is named for what it replaces.
pub(super) type Replacing = Option<ContainerId>;

/// A door Container the route replaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Retiree {
    pub(super) id: ContainerId,
    /// It ran when planned, so a replacement that fails starts it again.
    pub(super) running: bool,
}

/// The whole change, decided before any effect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DoorPlan {
    /// Connectors that are not the canonical one, or every one when the Tunnel Door
    /// closes.
    pub(super) retire_tunnels: Vec<Retiree>,
    /// Joined Caddys that are not the canonical one.
    pub(super) retire_caddys: Vec<Retiree>,
    /// Caddys from before the holder, which publish the ports themselves.
    pub(super) legacy: Vec<Retiree>,
    pub(super) retire_holders: Vec<Retiree>,
    pub(super) holder: HolderStep,
    pub(super) proxy: JoinedStep,
    /// `None` while the Tunnel Door is closed.
    pub(super) tunnel: Option<JoinedStep>,
}

impl DoorPlan {
    /// Every Container the plan replaces, in the order they stop: each connector and
    /// Caddy before any holder, so none is left in a network that goes.
    fn retirees(&self) -> impl Iterator<Item = &Retiree> {
        self.retire_tunnels
            .iter()
            .chain(&self.retire_caddys)
            .chain(&self.legacy)
            .chain(&self.retire_holders)
    }

    /// Whether the stage that failed was creating a door.
    fn creates_at(&self, stage: IngressStage) -> bool {
        match stage {
            IngressStage::Holder => true,
            IngressStage::Proxy => matches!(self.proxy, JoinedStep::Create(_)),
            IngressStage::Tunnel => matches!(self.tunnel, Some(JoinedStep::Create(_))),
            IngressStage::Unrecognized => false,
        }
    }
}

fn usable(entry: &DoorEntry) -> bool {
    !matches!(
        entry.facts.runtime,
        ContainerRuntimeObservation::Removing | ContainerRuntimeObservation::Dead
    )
}

fn newest<'entry>(entries: &[&'entry DoorEntry]) -> Option<&'entry DoorEntry> {
    entries.iter().copied().max_by_key(|entry| {
        (
            entry.facts.running(),
            entry.facts.created_at,
            entry.facts.id,
        )
    })
}

fn retirees(entries: &[&DoorEntry], kept: Option<ContainerId>) -> Vec<Retiree> {
    entries
        .iter()
        .filter(|entry| Some(entry.facts.id) != kept)
        .map(|entry| Retiree {
            id: entry.facts.id,
            running: !entry.facts.at_rest(),
        })
        .collect()
}

/// The newest of `entries` not `kept`. A Container named for replacing it would be
/// newer still, so no Container holds the name a creation that replaces it takes.
fn replacing(entries: &[&DoorEntry], kept: Option<ContainerId>) -> Replacing {
    entries
        .iter()
        .filter(|entry| Some(entry.facts.id) != kept)
        .map(|entry| (entry.facts.created_at, entry.facts.id))
        .max()
        .map(|(_, id)| id)
}

/// Decide the change from the inventory. `fresh_caddy` replaces the canonical Caddy
/// even when it runs, as a legacy Deploy that resends the Caddy expects.
pub(super) fn plan_doors(
    inventory: &[DoorEntry],
    specs: &DoorSpecs,
    fresh_caddy: bool,
) -> DoorPlan {
    let system = Namespace::system();
    let kind = ContainerKind::ServiceContainer;
    let holders = inventory
        .iter()
        .filter(|entry| entry.facts.door == Door::Holder)
        .collect::<Vec<_>>();
    let canonical_holders = holders
        .iter()
        .copied()
        .filter(|entry| {
            usable(entry) && same_creation(&entry.observation, &system, kind, &specs.holder)
        })
        .collect::<Vec<_>>();
    let holder = newest(&canonical_holders);
    let holder_id = holder.map(|entry| entry.facts.id);
    let holder_step = match holder {
        Some(entry)
            if matches!(
                entry.facts.runtime,
                ContainerRuntimeObservation::Running { .. }
                    | ContainerRuntimeObservation::Restarting
            ) =>
        {
            HolderStep::Keep(entry.facts.id)
        }
        Some(entry) => HolderStep::Start(entry.facts.id),
        None => HolderStep::Create(replacing(&holders, None)),
    };
    let in_holder = Joined {
        holder,
        holder_step: &holder_step,
    };

    let caddys = inventory
        .iter()
        .filter(|entry| entry.facts.door == Door::Proxy)
        .collect::<Vec<_>>();
    let (joined, legacy): (Vec<_>, Vec<_>) = caddys
        .iter()
        .copied()
        .partition(|entry| entry.observation.resolved_spec.container.network.is_some());
    let (proxy_step, retire_caddys) = in_holder.plan(&joined, &specs.proxy, fresh_caddy);

    let tunnels = inventory
        .iter()
        .filter(|entry| entry.facts.door == Door::Tunnel)
        .collect::<Vec<_>>();
    let (tunnel_step, retire_tunnels) = match &specs.tunnel {
        Some(spec) => {
            let (step, retire) = in_holder.plan(&tunnels, spec, false);
            (Some(step), retire)
        }
        None => (None, retirees(&tunnels, None)),
    };
    DoorPlan {
        retire_tunnels,
        retire_caddys,
        legacy: retirees(&legacy, None),
        retire_holders: retirees(&holders, holder_id),
        holder: holder_step,
        proxy: proxy_step,
        tunnel: tunnel_step,
    }
}

/// The holder a joined door is planned against.
struct Joined<'plan> {
    holder: Option<&'plan DoorEntry>,
    holder_step: &'plan HolderStep,
}

impl Joined<'_> {
    /// The step for the door `spec` describes among `entries`, and the entries it
    /// retires. `fresh` replaces the canonical one even when it runs.
    fn plan(
        &self,
        entries: &[&DoorEntry],
        spec: &ResolvedServiceSpec,
        fresh: bool,
    ) -> (JoinedStep, Vec<Retiree>) {
        let holder_id = self.holder.map(|entry| entry.facts.id);
        let canonical = entries
            .iter()
            .copied()
            .filter(|entry| {
                usable(entry)
                    && holder_id.is_some()
                    && entry.facts.joined_to == holder_id
                    && same_creation(
                        &entry.observation,
                        &Namespace::system(),
                        ContainerKind::ServiceContainer,
                        spec,
                    )
            })
            .collect::<Vec<_>>();
        let kept = newest(&canonical).filter(|_| !fresh);
        let kept_id = kept.map(|entry| entry.facts.id);
        let step = match kept {
            Some(entry) if entry.facts.running() => {
                let stale = match (
                    self.holder_step,
                    self.holder.and_then(|holder| holder.facts.netns),
                ) {
                    (HolderStep::Start(_), _) => true,
                    (_, Some(holder_ns)) => entry.facts.netns.is_some_and(|ns| ns != holder_ns),
                    (_, None) => false,
                };
                if stale {
                    JoinedStep::Rejoin(entry.facts.id)
                } else {
                    JoinedStep::Keep(entry.facts.id)
                }
            }
            Some(entry) if entry.facts.runtime == ContainerRuntimeObservation::Restarting => {
                JoinedStep::Keep(entry.facts.id)
            }
            Some(entry) => JoinedStep::Start(entry.facts.id),
            None => JoinedStep::Create(replacing(entries, None)),
        };
        (step, retirees(entries, kept_id))
    }
}

/// The doors a legacy Deploy keeps: it asks for a Caddy and knows no other Door, so
/// the holder's ports and the connector stay as this Machine has them.
fn doors_as_found(inventory: &[DoorEntry], proxy: ServiceId) -> DoorSpecs {
    let newest_of = |door: Door| {
        newest(
            &inventory
                .iter()
                .filter(|entry| entry.facts.door == door && usable(entry))
                .collect::<Vec<_>>(),
        )
    };
    let ports = match newest_of(Door::Holder) {
        Some(holder) if holder.observation.resolved_spec.ports.is_empty() => IngressPorts::Closed,
        _ => IngressPorts::Public,
    };
    let token = newest_of(Door::Tunnel)
        .and_then(|tunnel| tunnel_sealed_token(&tunnel.observation.resolved_spec.container.labels));
    DoorSpecs::new(proxy, ports, token.as_ref())
}

/// The doors this Machine last held, each as the spec it was created from, and the
/// sealed token the connector carries. Placing them again pulls and replaces nothing.
struct DoorsHeld {
    specs: DoorSpecs,
    sealed: Option<Ciphertext>,
}

/// The doors the inventory shows, or `None` when this Machine held no holder with a
/// Caddy joined to it, or its newest connector carries no readable token. Planning
/// without the connector would retire it.
fn doors_held(inventory: &[DoorEntry]) -> Option<DoorsHeld> {
    let newest_of = |door: Door| {
        newest(
            &inventory
                .iter()
                .filter(|entry| entry.facts.door == door && usable(entry))
                .collect::<Vec<_>>(),
        )
    };
    let spec = |entry: &DoorEntry| entry.observation.resolved_spec.clone();
    let holder = newest_of(Door::Holder)?;
    let proxy = newest(
        &inventory
            .iter()
            .filter(|entry| {
                entry.facts.door == Door::Proxy
                    && usable(entry)
                    && entry.observation.resolved_spec.container.network.is_some()
            })
            .collect::<Vec<_>>(),
    )?;
    let tunnel = newest_of(Door::Tunnel);
    let sealed = match tunnel {
        Some(tunnel) => Some(tunnel_sealed_token(
            &tunnel.observation.resolved_spec.container.labels,
        )?),
        None => None,
    };
    Some(DoorsHeld {
        specs: DoorSpecs {
            holder: spec(holder),
            proxy: spec(proxy),
            tunnel: tunnel.map(spec),
        },
        sealed,
    })
}

fn stage_failure(stage: IngressStage, error: &Error) -> IngressDoorsFailure {
    let error = if let Error::Docker(docker) = error {
        RpcError::from(docker)
    } else if matches!(error, Error::NotParticipating | Error::DockerUnavailable) {
        RpcError::caused(RpcErrorCode::Unavailable, error)
    } else {
        RpcError::caused(RpcErrorCode::Internal, error)
    };
    IngressDoorsFailure { stage, error }
}

/// Whether a converge answers a legacy Deploy, which resends the Caddy and knows no
/// Door but the Public one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Legacy {
    No,
    /// Replace the canonical Caddy even when it runs, and keep every other door as
    /// this Machine has it.
    Caddy,
}

/// Every Docker effect a converge made, recorded as Docker answers each request, so
/// a failure undoes exactly what the call did and nothing it found.
#[derive(Default)]
struct Effects {
    /// Retirees that ran until this call stopped them, in stop order.
    stopped: Vec<ContainerId>,
    /// Doors at rest that this call started, recorded before the start request.
    started: Vec<ContainerId>,
    /// Doors this call created, in creation order.
    created: Vec<ContainerId>,
}

impl LocalMachine {
    /// Bring the Ingress doors to the daemon's pinned wiring for the requested Doors.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedDoors`] for a Door this release doesn't know,
    /// [`Error::TunnelTokenMissing`] and [`Error::TunnelToken`] for a Tunnel Door
    /// without a token this Cluster opens, and the admission errors of a creation.
    /// A Docker stage that fails is answered in the result's `failure` instead, with
    /// the stages before it completed.
    pub(crate) async fn set_ingress_doors(
        &self,
        request: SetIngressDoorsRequest,
    ) -> Result<IngressDoorsSet, Error> {
        if !request.doors.is_known() {
            return Err(Error::UnsupportedDoors);
        }
        let ports = if request.doors.has(IngressDoor::Public) {
            IngressPorts::Public
        } else {
            IngressPorts::Closed
        };
        let (sealed, token) = if request.doors.has(IngressDoor::Tunnel) {
            let sealed = request
                .sealed_token
                .as_ref()
                .ok_or(Error::TunnelTokenMissing)?;
            let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
            let token = containers
                .open_tunnel_token(sealed)
                .map_err(Error::TunnelToken)?;
            (Some(sealed), Some(token))
        } else {
            (None, None)
        };
        let specs = DoorSpecs::new(request.proxy_service, ports, sealed);
        self.converge_doors(&specs, Legacy::No, token).await
    }

    /// Place the doors this Machine last held again, after a reboot or a restart.
    ///
    /// A reboot clears the opened token, and Docker starts the connector before the
    /// daemon is up to write it again, so its start fails and Docker never retries.
    /// This opens the connector's sealed token, writes it, then starts whichever door
    /// is at rest. A door that runs stays as it is, so a daemon restart changes nothing,
    /// and a Machine with no doors places none.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TunnelToken`] when the connector's token no longer opens, and
    /// the errors of the Docker reads and the token write. A Docker stage that fails
    /// is answered in the result's `failure`.
    pub(crate) async fn restore_doors(&self) -> Result<Option<IngressDoorsSet>, Error> {
        let containers = self.containers.clone().ok_or(Error::DockerUnavailable)?;
        self.door_effect()?;
        let local = self.clone();
        self.finish_installed(move |_installation| async move {
            let _doors = containers.door_lifecycle().await;
            local.door_effect()?;
            let inventory = containers.door_inventory(&local.record().id()).await?;
            let Some(held) = doors_held(&inventory) else {
                return Ok(None);
            };
            if let Some(sealed) = &held.sealed {
                let token = containers
                    .open_tunnel_token(sealed)
                    .map_err(Error::TunnelToken)?;
                containers
                    .write_tunnel_token(&token)
                    .map_err(Error::TunnelTokenFile)?;
            }
            let _turn = local.owner.admission_lock().write_owned().await;
            local
                .converge_doors_admitted(&containers, &inventory, &held.specs, Legacy::No)
                .await
                .map(Some)
        })
        .await
    }

    /// Place the doors `specs` describe, or the doors this Machine has with a fresh
    /// Caddy for a legacy Deploy. Every pin is pulled and the tunnel `token` written
    /// before any running Container is touched, under the door lifecycle lock and
    /// before the admission turn, so a slow pull holds up no other admitted work.
    pub(super) async fn converge_doors(
        &self,
        specs: &DoorSpecs,
        legacy: Legacy,
        token: Option<TunnelToken>,
    ) -> Result<IngressDoorsSet, Error> {
        let containers = self.containers.clone().ok_or(Error::DockerUnavailable)?;
        self.door_effect()?;
        let local = self.clone();
        let specs = specs.clone();
        self.finish_installed(move |_installation| async move {
            let _doors = containers.door_lifecycle().await;
            local.door_effect()?;
            let machine_id = local.record().id();
            let inventory = containers.door_inventory(&machine_id).await?;
            let specs = match legacy {
                Legacy::Caddy => doors_as_found(&inventory, specs.proxy.service_id),
                Legacy::No => specs,
            };
            for spec in [
                Some(&specs.holder),
                Some(&specs.proxy),
                specs.tunnel.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                containers
                    .prepare_pinned_image(&spec.container.image)
                    .await?;
            }
            if let Some(token) = &token {
                containers
                    .write_tunnel_token(token)
                    .map_err(Error::TunnelTokenFile)?;
            }
            let _turn = local.owner.admission_lock().write_owned().await;
            let set = local
                .converge_doors_admitted(&containers, &inventory, &specs, legacy)
                .await?;
            if specs.tunnel.is_none() && set.failure.is_none() {
                containers
                    .remove_tunnel_token()
                    .map_err(Error::TunnelTokenFile)?;
            }
            Ok(set)
        })
        .await
    }

    /// Carry out the plan in four stages: stop, holder, proxy, tunnel.
    ///
    /// The stop stage stops every door the plan replaces before anything starts,
    /// connectors and Caddys before holders, so a legacy Caddy has let go of 80 and
    /// 443 before the holder takes them. The proxy and tunnel stages stop only a door
    /// they rejoin, and start it again at once. No new Caddy starts while an old one
    /// runs, so two Caddys never run at once. Replaced doors are removed only once the
    /// new doors run. A removal that fails leaves that door stopped, and the next call
    /// retires it.
    ///
    /// Each effect is recorded in [`Effects`] as Docker answers it. A failure in the
    /// stop or holder stage, or in a later stage that creates a door to replace doors
    /// the plan retires, undoes the record: it removes what the call created, stops
    /// what it started, then starts again what it stopped while it ran. The previous
    /// doors start again only once nothing the call made still runs. Any other
    /// failure leaves the completed stages in place with nothing to come back to, and
    /// the next call starts or replaces the door that failed.
    async fn converge_doors_admitted(
        &self,
        containers: &ContainerRuntime,
        inventory: &[DoorEntry],
        specs: &DoorSpecs,
        legacy: Legacy,
    ) -> Result<IngressDoorsSet, Error> {
        let plan = plan_doors(inventory, specs, legacy == Legacy::Caddy);
        let mut set = IngressDoorsSet::default();
        let mut effects = Effects::default();
        if let Err((stage, error)) = self
            .place_doors(containers, specs, &plan, &mut set, &mut effects)
            .await
        {
            let undo = stage == IngressStage::Holder
                || (plan.creates_at(stage) && plan.retirees().next().is_some());
            let error = if undo {
                let unchanged = |outcome: DoorOutcome| {
                    (outcome.change == DoorChange::Unchanged).then_some(outcome)
                };
                set.holder = set.holder.and_then(unchanged);
                set.proxy = set.proxy.and_then(unchanged);
                self.undo(containers, &effects, error).await
            } else {
                error
            };
            set.failure = Some(stage_failure(stage, &error));
            return Ok(set);
        }
        for retiree in plan.retirees() {
            match containers.remove(&retiree.id, true, true).await {
                Ok(()) => set.retired.push(retiree.id),
                Err(crate::docker::Error::ContainerNotFound(_)) => {}
                // Stopped and held, it serves nothing, and the next call retires it.
                Err(error) => tracing::warn!(
                    container = %retiree.id,
                    error = %ployz_core::error_chain::inline(&error),
                    "could not remove a replaced Ingress door"
                ),
            }
        }
        Ok(set)
    }

    /// The stop, holder, proxy and tunnel stages, filling `set` as each completes. A
    /// failure names its stage, and the stop stage reports as the holder's.
    async fn place_doors(
        &self,
        containers: &ContainerRuntime,
        specs: &DoorSpecs,
        plan: &DoorPlan,
        set: &mut IngressDoorsSet,
        effects: &mut Effects,
    ) -> Result<(), (IngressStage, Error)> {
        let holder_stage = |error| (IngressStage::Holder, error);
        for retiree in plan.retirees() {
            self.stop_retiree(containers, retiree, effects)
                .await
                .map_err(holder_stage)?;
        }
        set.holder = Some(
            self.place_holder(containers, specs, plan, effects)
                .await
                .map_err(holder_stage)?,
        );
        set.proxy = Some(
            self.place_joined(containers, &specs.proxy, &plan.proxy, effects)
                .await
                .map_err(|error| (IngressStage::Proxy, error))?,
        );
        if let (Some(spec), Some(step)) = (&specs.tunnel, &plan.tunnel) {
            set.tunnel = Some(
                self.place_joined(containers, spec, step, effects)
                    .await
                    .map_err(|error| (IngressStage::Tunnel, error))?,
            );
        }
        Ok(())
    }

    async fn stop_retiree(
        &self,
        containers: &ContainerRuntime,
        retiree: &Retiree,
        effects: &mut Effects,
    ) -> Result<(), Error> {
        self.door_effect()?;
        match containers.stop(&retiree.id, None, None).await {
            Ok(()) => {
                if retiree.running {
                    effects.stopped.push(retiree.id);
                }
                Ok(())
            }
            Err(crate::docker::Error::ContainerNotFound(_)) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn place_holder(
        &self,
        containers: &ContainerRuntime,
        specs: &DoorSpecs,
        plan: &DoorPlan,
        effects: &mut Effects,
    ) -> Result<DoorOutcome, Error> {
        let (id, change) = match &plan.holder {
            HolderStep::Keep(id) => (*id, DoorChange::Unchanged),
            HolderStep::Start(id) => {
                effects.started.push(*id);
                self.start_door(containers, id).await?;
                (*id, DoorChange::Started)
            }
            HolderStep::Create(replacing) => {
                let id = self.create_door(&specs.holder, *replacing, effects).await?;
                self.start_door(containers, &id).await?;
                (id, DoorChange::Created)
            }
        };
        let now = containers.inspect_managed(&id, &self.record().id()).await?;
        Ok(DoorOutcome { now, change })
    }

    async fn place_joined(
        &self,
        containers: &ContainerRuntime,
        spec: &ResolvedServiceSpec,
        step: &JoinedStep,
        effects: &mut Effects,
    ) -> Result<DoorOutcome, Error> {
        let (id, change) = match step {
            JoinedStep::Keep(id) => (*id, DoorChange::Unchanged),
            JoinedStep::Start(id) => {
                effects.started.push(*id);
                self.start_door(containers, id).await?;
                (*id, DoorChange::Started)
            }
            JoinedStep::Rejoin(id) => {
                self.door_effect()?;
                containers.stop(id, None, None).await?;
                self.start_door(containers, id).await?;
                (*id, DoorChange::Rejoined)
            }
            JoinedStep::Create(replacing) => {
                let id = self.create_door(spec, *replacing, effects).await?;
                self.start_door(containers, &id).await?;
                (id, DoorChange::Created)
            }
        };
        let now = containers.inspect_managed(&id, &self.record().id()).await?;
        Ok(DoorOutcome { now, change })
    }

    /// The Machine may place doors now: it participates, accepts ingress and is not
    /// leaving. Checked again before every effect, since a stage may be slow.
    fn door_effect(&self) -> Result<(), Error> {
        let record = self.record();
        if !matches!(
            record.phase(),
            ployz_core::LocalMachinePhase::Joining | ployz_core::LocalMachinePhase::Participating
        ) {
            return Err(Error::NotParticipating);
        }
        let machine = record.machine().ok_or(Error::NotParticipating)?;
        if !machine.accepts_ingress {
            return Err(crate::docker::Error::WorkNotAccepted.into());
        }
        if self
            .containers
            .as_ref()
            .is_some_and(ContainerRuntime::departing)
        {
            return Err(Error::Departing {
                server: machine.name.clone(),
                source: Box::new(crate::docker::Error::WorkNotAccepted.into()),
            });
        }
        Ok(())
    }

    async fn start_door(
        &self,
        containers: &ContainerRuntime,
        id: &ContainerId,
    ) -> Result<(), Error> {
        self.door_effect()?;
        containers.start(id).await?;
        let machine_id = self.record().id();
        let deadline = tokio::time::Instant::now() + DOOR_START_WAIT;
        loop {
            let runtime = containers
                .inspect_managed(id, &machine_id)
                .await?
                .runtime
                .clone();
            if matches!(runtime, ContainerRuntimeObservation::Running { .. }) {
                return Ok(());
            }
            if crate::ingress::at_rest(&runtime) || tokio::time::Instant::now() >= deadline {
                return Err(Error::DoorNotRunning {
                    container_id: *id,
                    runtime,
                });
            }
            tokio::time::sleep(DOOR_POLL).await;
        }
    }

    /// Create a door and record it in `effects` as soon as Docker names it.
    async fn create_door(
        &self,
        spec: &ResolvedServiceSpec,
        replacing: Replacing,
        effects: &mut Effects,
    ) -> Result<ContainerId, Error> {
        self.door_effect()?;
        let key = global_creation_key(spec);
        let key = match replacing {
            Some(old) => format!("{key}:replace:{old}"),
            None => key,
        };
        let created = self
            .create_container_admitted(
                ContainerKind::ServiceContainer,
                &Namespace::system(),
                spec,
                Some(key),
                None,
                Gen::FIRST,
                ImagePull::Prepared,
            )
            .await?;
        effects.created.push(created.container_id);
        Ok(created.container_id)
    }

    /// Undo `effects`: remove the doors the call created and stop the ones it started,
    /// each Caddy before the holder whose network it is in. Only then start again the
    /// ones it stopped while they ran, holders before their Caddys. A failure of any
    /// step is folded into `error`, and one that leaves a door of this call running or
    /// present keeps the previous doors stopped.
    async fn undo(
        &self,
        containers: &ContainerRuntime,
        effects: &Effects,
        mut error: Error,
    ) -> Error {
        let mut cleared = true;
        for id in effects.created.iter().rev() {
            if let Err(removal) = containers.remove(id, true, true).await {
                cleared = false;
                error = Error::Cleanup(format!(
                    "{}; removing the new Ingress door {id} failed too: {}",
                    ployz_core::error_chain::inline(&error),
                    ployz_core::error_chain::inline(&removal),
                ));
            }
        }
        for id in effects.started.iter().rev() {
            if let Err(stop) = containers.stop(id, None, None).await {
                cleared = false;
                error = Error::Cleanup(format!(
                    "{}; stopping the Ingress door {id} again failed too: {}",
                    ployz_core::error_chain::inline(&error),
                    ployz_core::error_chain::inline(&stop),
                ));
            }
        }
        if !cleared {
            return if effects.stopped.is_empty() {
                error
            } else {
                Error::Cleanup(format!(
                    "{}; the previous Ingress doors stay stopped",
                    ployz_core::error_chain::inline(&error),
                ))
            };
        }
        for id in effects.stopped.iter().rev() {
            if let Err(restart) = containers.start(id).await {
                error = Error::Cleanup(format!(
                    "{}; starting the previous Ingress door {id} again failed: {}",
                    ployz_core::error_chain::inline(&error),
                    ployz_core::error_chain::inline(&restart),
                ));
            }
        }
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::{DoorFacts, NetnsId};
    use ployz_core::{ContainerObservation, ResolvedServiceSpec, ServiceId};
    use serde_json::json;

    fn id(id: char) -> ContainerId {
        ContainerId::parse(id.to_string().repeat(64)).unwrap()
    }

    fn specs() -> DoorSpecs {
        DoorSpecs::public(ServiceId::parse("c".repeat(32)).unwrap())
    }

    fn entry(
        id_char: char,
        door: Door,
        spec: &ResolvedServiceSpec,
        created_at: i64,
        runtime: serde_json::Value,
    ) -> DoorEntry {
        let observation: ContainerObservation = serde_json::from_value(json!({
            "container_id": id_char.to_string().repeat(64),
            "display_name": format!("{}-{id_char}", spec.name),
            "created_at_unix_nanos": created_at,
            "machine_id": "b".repeat(32),
            "namespace": "ployz-system",
            "kind": "service_container",
            "runtime": runtime,
            "resolved_spec": serde_json::to_value(spec).unwrap(),
        }))
        .unwrap();
        DoorEntry {
            facts: DoorFacts {
                id: id(id_char),
                door,
                created_at,
                runtime: observation.runtime.clone(),
                failed_start: false,
                joined_to: None,
                netns: None,
            },
            observation,
        }
    }

    fn running() -> serde_json::Value {
        json!({ "state": "running", "health": "healthy" })
    }

    fn created() -> serde_json::Value {
        json!({ "state": "created" })
    }

    fn holder(id: char, runtime: serde_json::Value) -> DoorEntry {
        entry(id, Door::Holder, &specs().holder, 1, runtime)
    }

    fn caddy(id: char, holder: char, runtime: serde_json::Value) -> DoorEntry {
        let mut entry = entry(id, Door::Proxy, &specs().proxy, 2, runtime);
        entry.facts.joined_to = Some(self::id(holder));
        entry
    }

    fn in_netns(mut entry: DoorEntry, ino: u64) -> DoorEntry {
        entry.facts.netns = Some(NetnsId::new(ino));
        entry
    }

    fn legacy_caddy(id: char, runtime: serde_json::Value) -> DoorEntry {
        let legacy = crate::images::legacy_caddy_for_tests("caddy:2")
            .to_resolved(
                ServiceId::parse("c".repeat(32)).unwrap(),
                Default::default(),
            )
            .unwrap();
        entry(id, Door::Proxy, &legacy, 0, runtime)
    }

    fn retiree(id: char, running: bool) -> Retiree {
        Retiree {
            id: self::id(id),
            running,
        }
    }

    fn token() -> ployz_core::Ciphertext {
        ployz_core::Ciphertext::from_bytes(vec![7; 48])
    }

    fn tunnel_specs() -> DoorSpecs {
        DoorSpecs::new(
            ServiceId::parse("c".repeat(32)).unwrap(),
            IngressPorts::Closed,
            Some(&token()),
        )
    }

    fn closed_holder(id: char, runtime: serde_json::Value) -> DoorEntry {
        entry(id, Door::Holder, &tunnel_specs().holder, 1, runtime)
    }

    fn tunnel(id: char, holder: char, runtime: serde_json::Value) -> DoorEntry {
        let spec = tunnel_specs().tunnel.unwrap();
        let mut entry = entry(id, Door::Tunnel, &spec, 3, runtime);
        entry.facts.joined_to = Some(self::id(holder));
        entry
    }

    #[test]
    fn opening_the_tunnel_door_replaces_the_public_holder_and_creates_the_connector() {
        let plan = plan_doors(
            &[holder('a', running()), caddy('b', 'a', running())],
            &tunnel_specs(),
            false,
        );
        assert_eq!(plan.holder, HolderStep::Create(Some(id('a'))));
        assert_eq!(plan.retire_holders, vec![retiree('a', true)]);
        assert_eq!(plan.proxy, JoinedStep::Create(Some(id('b'))));
        assert_eq!(plan.tunnel, Some(JoinedStep::Create(None)));
        assert!(plan.retire_tunnels.is_empty());
        assert!(plan.creates_at(IngressStage::Tunnel));
    }

    #[test]
    fn closing_the_tunnel_door_retires_the_connector_before_the_holder() {
        let plan = plan_doors(
            &[
                in_netns(closed_holder('a', running()), 10),
                in_netns(caddy('b', 'a', running()), 10),
                in_netns(tunnel('c', 'a', running()), 10),
            ],
            &specs(),
            false,
        );
        assert_eq!(plan.holder, HolderStep::Create(Some(id('a'))));
        assert_eq!(plan.tunnel, None);
        assert_eq!(plan.retire_tunnels, vec![retiree('c', true)]);
        assert_eq!(
            plan.retirees()
                .map(|retiree| retiree.id)
                .collect::<Vec<_>>(),
            vec![id('c'), id('b'), id('a')],
            "the connector and the Caddy stop before the holder whose network they share"
        );
    }

    #[test]
    fn a_running_tunnel_set_is_left_alone_and_a_new_token_replaces_the_connector() {
        let inventory = [
            in_netns(closed_holder('a', running()), 10),
            in_netns(caddy('b', 'a', running()), 10),
            in_netns(tunnel('c', 'a', running()), 10),
        ];
        let plan = plan_doors(&inventory, &tunnel_specs(), false);
        assert_eq!(plan.holder, HolderStep::Keep(id('a')));
        assert_eq!(plan.proxy, JoinedStep::Keep(id('b')));
        assert_eq!(plan.tunnel, Some(JoinedStep::Keep(id('c'))));
        assert_eq!(plan.retirees().count(), 0);

        let other_token = ployz_core::Ciphertext::from_bytes(vec![8; 48]);
        let renewed = DoorSpecs::new(
            ServiceId::parse("c".repeat(32)).unwrap(),
            IngressPorts::Closed,
            Some(&other_token),
        );
        let plan = plan_doors(&inventory, &renewed, false);
        assert_eq!(plan.holder, HolderStep::Keep(id('a')));
        assert_eq!(plan.tunnel, Some(JoinedStep::Create(Some(id('c')))));
        assert_eq!(plan.retire_tunnels, vec![retiree('c', true)]);
    }

    #[test]
    fn the_doors_found_are_the_newest_holders_ports_and_the_connectors_token() {
        let proxy = ServiceId::parse("c".repeat(32)).unwrap();
        assert_eq!(doors_as_found(&[], proxy), DoorSpecs::public(proxy));
        assert_eq!(
            doors_as_found(&[holder('a', running()), caddy('b', 'a', running())], proxy),
            DoorSpecs::public(proxy)
        );
        assert_eq!(
            doors_as_found(
                &[
                    closed_holder('a', running()),
                    caddy('b', 'a', running()),
                    tunnel('c', 'a', running())
                ],
                proxy
            ),
            tunnel_specs()
        );
        assert_eq!(
            doors_as_found(
                &[closed_holder('a', running()), legacy_caddy('d', running())],
                proxy
            ),
            DoorSpecs::new(proxy, IngressPorts::Closed, None),
            "a closed holder with no connector stays closed until a command opens a Door"
        );
    }

    #[test]
    fn an_absent_pair_is_created() {
        assert_eq!(
            plan_doors(&[], &specs(), false),
            DoorPlan {
                retire_tunnels: vec![],
                retire_caddys: vec![],
                legacy: vec![],
                retire_holders: vec![],
                holder: HolderStep::Create(None),
                proxy: JoinedStep::Create(None),
                tunnel: None,
            }
        );
    }

    #[test]
    fn a_stopped_pair_is_started_and_the_caddy_rejoins() {
        let plan = plan_doors(
            &[holder('a', created()), caddy('b', 'a', created())],
            &specs(),
            false,
        );
        assert_eq!(plan.holder, HolderStep::Start(id('a')));
        assert_eq!(
            plan.proxy,
            JoinedStep::Start(id('b')),
            "a Caddy at rest joins the holder's new network when it starts"
        );
        let plan = plan_doors(
            &[holder('a', created()), caddy('b', 'a', running())],
            &specs(),
            false,
        );
        assert_eq!(
            plan.proxy,
            JoinedStep::Rejoin(id('b')),
            "a Caddy running in the stopped holder's old network is rejoined"
        );
    }

    #[test]
    fn a_canonical_pair_is_left_alone() {
        let plan = plan_doors(
            &[
                in_netns(holder('a', running()), 10),
                in_netns(caddy('b', 'a', running()), 10),
            ],
            &specs(),
            false,
        );
        assert_eq!(plan.holder, HolderStep::Keep(id('a')));
        assert_eq!(plan.proxy, JoinedStep::Keep(id('b')));
        assert_eq!(plan.retirees().count(), 0);
    }

    #[test]
    fn a_fresh_caddy_is_named_for_the_canonical_one_it_replaces() {
        let plan = plan_doors(
            &[holder('a', running()), caddy('b', 'a', created())],
            &specs(),
            true,
        );
        assert_eq!(plan.holder, HolderStep::Keep(id('a')));
        assert_eq!(plan.proxy, JoinedStep::Create(Some(id('b'))));
        assert_eq!(
            plan.retire_caddys,
            vec![retiree('b', false)],
            "a Caddy an older Deploy stopped is not started again if the new one fails"
        );
    }

    #[test]
    fn a_pin_change_replaces_the_holder_and_every_caddy() {
        let mut old = specs();
        old.holder.container.image = "registry.k8s.io/pause:3.9".into();
        let plan = plan_doors(
            &[
                entry('a', Door::Holder, &old.holder, 1, running()),
                caddy('b', 'a', running()),
            ],
            &specs(),
            false,
        );
        assert_eq!(plan.holder, HolderStep::Create(Some(id('a'))));
        assert_eq!(plan.retire_holders, vec![retiree('a', true)]);
        assert_eq!(plan.retire_caddys, vec![retiree('b', true)]);
        assert_eq!(plan.proxy, JoinedStep::Create(Some(id('b'))));
        assert_eq!(
            plan.retirees()
                .map(|retiree| retiree.id)
                .collect::<Vec<_>>(),
            vec![id('b'), id('a')],
            "the Caddy stops before the holder whose network it is in"
        );

        let mut old = specs();
        old.proxy.container.image = "docker.io/library/caddy:2.11.4".into();
        let plan = plan_doors(
            &[
                holder('a', running()),
                entry('b', Door::Proxy, &old.proxy, 2, running()),
            ],
            &specs(),
            false,
        );
        assert_eq!(plan.holder, HolderStep::Keep(id('a')));
        assert_eq!(plan.retire_caddys, vec![retiree('b', true)]);
        assert_eq!(plan.proxy, JoinedStep::Create(Some(id('b'))));
    }

    #[test]
    fn a_legacy_caddy_is_stopped_for_the_holder_and_retired() {
        let plan = plan_doors(&[legacy_caddy('a', running())], &specs(), true);
        assert_eq!(plan.legacy, vec![retiree('a', true)]);
        assert!(plan.retire_caddys.is_empty());
        assert_eq!(plan.holder, HolderStep::Create(None));
        assert_eq!(
            plan.proxy,
            JoinedStep::Create(None),
            "a legacy Caddy's spec names no joined Caddy's slot"
        );
    }

    #[test]
    fn surplus_caddys_are_retired_and_the_newest_running_canonical_one_kept() {
        let mut older = caddy('b', 'a', running());
        older.facts.created_at = 1;
        let mut stopped = caddy('d', 'a', created());
        stopped.facts.created_at = 3;
        let mut foreign = caddy('e', 'f', running());
        foreign.facts.created_at = 4;
        let plan = plan_doors(
            &[holder('a', running()), older, stopped, foreign],
            &specs(),
            false,
        );
        assert_eq!(plan.proxy, JoinedStep::Keep(id('b')));
        assert_eq!(
            plan.retire_caddys,
            vec![retiree('d', false), retiree('e', true)]
        );
    }

    #[test]
    fn a_replacement_is_named_for_the_newest_caddy_it_replaces() {
        let mut old = specs();
        old.proxy.container.image = "docker.io/library/caddy:2.11.4".into();
        let mut stale = entry('b', Door::Proxy, &old.proxy, 2, created());
        stale.facts.joined_to = Some(id('a'));
        let mut newer = caddy('d', 'f', created());
        newer.facts.created_at = 5;
        let plan = plan_doors(&[holder('a', running()), stale, newer], &specs(), false);
        assert_eq!(plan.proxy, JoinedStep::Create(Some(id('d'))));
    }

    #[test]
    fn a_caddy_in_the_holders_old_namespace_is_rejoined() {
        let plan = plan_doors(
            &[
                in_netns(holder('a', running()), 10),
                in_netns(caddy('b', 'a', running()), 11),
            ],
            &specs(),
            false,
        );
        assert_eq!(plan.proxy, JoinedStep::Rejoin(id('b')));
        let plan = plan_doors(
            &[
                in_netns(holder('a', running()), 10),
                caddy('b', 'a', running()),
            ],
            &specs(),
            false,
        );
        assert_eq!(
            plan.proxy,
            JoinedStep::Keep(id('b')),
            "an unreadable namespace reads as canonical"
        );
    }

    #[test]
    fn a_dead_holder_is_retired_not_started() {
        let plan = plan_doors(
            &[
                holder('a', json!({ "state": "dead" })),
                caddy('b', 'a', created()),
            ],
            &specs(),
            false,
        );
        assert_eq!(
            plan.holder,
            HolderStep::Create(Some(id('a'))),
            "the dead holder still holds the canonical name"
        );
        assert_eq!(plan.retire_holders, vec![retiree('a', false)]);
        assert_eq!(plan.retire_caddys, vec![retiree('b', false)]);
    }
}
