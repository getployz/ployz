//! Placing this Machine's Ingress doors: the holder that owns the Ingress Proxy's
//! network namespace and host ports, the Caddy that joins it, and, when the Tunnel
//! Door is open, the Tunnel Origin that owns a network namespace on its own bridge
//! and the Tunnel Connector that joins it.
//!
//! The route reads what Docker has, plans the whole change, then carries it out one
//! stage at a time. Every Container it replaces is stopped first and removed only once
//! the new doors run, so a replacement that fails leaves the old ones to start again.
//! The holder goes before the Caddy, which is created in its network, and the Tunnel
//! Origin before the connector, last. A Caddy from before the holder published the ports itself, so it is
//! stopped before the holder starts.

use std::time::Duration;

use ployz_core::{
    Ciphertext, ContainerId, ContainerKind, ContainerRuntimeObservation, DoorChange, DoorOutcome,
    Gen, IngressDoor, IngressDoors, IngressDoorsFailure, IngressDoorsSet, IngressPorts,
    IngressStage, Namespace, ResolvedServiceSpec, RpcError, RpcErrorCode, ServiceId,
    SetIngressDoorsRequest, global_creation_key, tunnel_sealed_token,
};
use zeroize::Zeroizing;

use super::{Error, LocalMachine};
use crate::docker::{ContainerRuntime, Door, DoorEntry, same_creation};
use crate::docker_image::ImagePull;
use crate::images::{DoorSpecs, TunnelSpecs};

/// An opened tunnel token, written for the connector before it is placed.
pub(super) type TunnelToken = Zeroizing<Vec<u8>>;

/// Which Machine role a reserved placement needs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Gate {
    /// The Ingress doors need a Machine that accepts ingress.
    Ingress,
    /// The Private Access router needs no role.
    Router,
}

/// How long a started door may take to be observed running. The holder is a pause
/// process and Caddy binds its listeners at once, so this is a bound, not a budget.
const DOOR_START_WAIT: Duration = Duration::from_secs(15);
const DOOR_POLL: Duration = Duration::from_millis(100);

/// What the route does with a door other doors join: the holder, or the Tunnel
/// Origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum HolderStep {
    /// A canonical one runs.
    Keep(ContainerId),
    /// A canonical one exists at rest.
    Start(ContainerId),
    Create(Replacing),
}

/// What the route does with a door in another's network: the Caddy in the holder's,
/// or the Tunnel Connector in the Tunnel Origin's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum JoinedStep {
    /// A canonical one runs in its owner's network.
    Keep(ContainerId),
    /// A canonical one exists at rest.
    Start(ContainerId),
    /// A canonical one runs in a network its owner no longer has.
    Rejoin(ContainerId),
    Create(Replacing),
}

/// What the route does with the Tunnel Door's pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TunnelPlan {
    pub(super) origin: HolderStep,
    pub(super) connector: JoinedStep,
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
    /// Tunnel Origins likewise.
    pub(super) retire_origins: Vec<Retiree>,
    /// Joined Caddys that are not the canonical one.
    pub(super) retire_caddys: Vec<Retiree>,
    /// Caddys from before the holder, which publish the ports themselves.
    pub(super) legacy: Vec<Retiree>,
    pub(super) retire_holders: Vec<Retiree>,
    pub(super) holder: HolderStep,
    pub(super) proxy: JoinedStep,
    /// `None` while the Tunnel Door is closed.
    pub(super) tunnel: Option<TunnelPlan>,
}

impl DoorPlan {
    /// Every Container the plan replaces, in the order they stop: each connector before
    /// any Tunnel Origin, and each Caddy before any holder, so none is left in a
    /// network that goes.
    fn retirees(&self) -> impl Iterator<Item = &Retiree> {
        self.retire_tunnels
            .iter()
            .chain(&self.retire_origins)
            .chain(&self.retire_caddys)
            .chain(&self.legacy)
            .chain(&self.retire_holders)
    }

    /// Whether the stage that failed was creating a door.
    fn creates_at(&self, stage: IngressStage) -> bool {
        match stage {
            IngressStage::Holder => true,
            IngressStage::Proxy => matches!(self.proxy, JoinedStep::Create(_)),
            IngressStage::Tunnel => self.tunnel.as_ref().is_some_and(|tunnel| {
                matches!(tunnel.origin, HolderStep::Create(_))
                    || matches!(tunnel.connector, JoinedStep::Create(_))
            }),
            IngressStage::Unrecognized => false,
        }
    }
}

/// A connector an earlier daemon placed in the holder's network namespace, where it
/// reached everything Caddy reaches. It is stopped for good: never kept, and never
/// started again by an undo or at daemon start.
fn shares_holders_netns(entry: &DoorEntry) -> bool {
    entry.facts.door == Door::Tunnel
        && entry.observation.resolved_spec.container.network
            == Some(ployz_core::ServiceNetwork::IngressHolder)
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
            running: !entry.facts.at_rest() && !shares_holders_netns(entry),
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
    let of = |door: Door| {
        inventory
            .iter()
            .filter(|entry| entry.facts.door == door)
            .collect::<Vec<_>>()
    };
    let holders = of(Door::Holder);
    let (holder_step, holder) = plan_owner(&holders, &specs.holder);
    let holder_id = holder.map(|entry| entry.facts.id);
    let in_holder = Joined {
        holder,
        holder_step: &holder_step,
    };

    let caddys = of(Door::Proxy);
    let (joined, legacy): (Vec<_>, Vec<_>) = caddys
        .iter()
        .copied()
        .partition(|entry| entry.observation.resolved_spec.container.network.is_some());
    let (proxy_step, retire_caddys) = in_holder.plan(&joined, &specs.proxy, fresh_caddy);

    let origins = of(Door::TunnelOrigin);
    let tunnels = of(Door::Tunnel);
    let (tunnel, retire_origins, retire_tunnels) = match &specs.tunnel {
        Some(pair) => {
            let (origin_step, origin) = plan_owner(&origins, &pair.origin);
            let (connector, retire_tunnels) = Joined {
                holder: origin,
                holder_step: &origin_step,
            }
            .plan(&tunnels, &pair.connector, false);
            (
                Some(TunnelPlan {
                    origin: origin_step,
                    connector,
                }),
                retirees(&origins, origin.map(|entry| entry.facts.id)),
                retire_tunnels,
            )
        }
        None => (None, retirees(&origins, None), retirees(&tunnels, None)),
    };
    DoorPlan {
        retire_tunnels,
        retire_origins,
        retire_caddys,
        legacy: retirees(&legacy, None),
        retire_holders: retirees(&holders, holder_id),
        holder: holder_step,
        proxy: proxy_step,
        tunnel,
    }
}

/// The door a joined door is planned against: the holder, or the Tunnel Origin.
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
        let kept = canonical(entries, spec, |entry| {
            holder_id.is_some() && entry.facts.joined_to == holder_id
        })
        .filter(|_| !fresh);
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

/// The newest of `entries` created from `spec` that `placed` accepts.
fn canonical<'entry>(
    entries: &[&'entry DoorEntry],
    spec: &ResolvedServiceSpec,
    placed: impl Fn(&DoorEntry) -> bool,
) -> Option<&'entry DoorEntry> {
    newest(
        &entries
            .iter()
            .copied()
            .filter(|entry| {
                usable(entry)
                    && placed(entry)
                    && same_creation(
                        &entry.observation,
                        &Namespace::system(),
                        ContainerKind::ServiceContainer,
                        spec,
                    )
            })
            .collect::<Vec<_>>(),
    )
}

/// The step for a door others join among `entries`, and the canonical one it keeps.
fn plan_owner<'entry>(
    entries: &[&'entry DoorEntry],
    spec: &ResolvedServiceSpec,
) -> (HolderStep, Option<&'entry DoorEntry>) {
    let kept = canonical(entries, spec, |_| true);
    let step = match kept {
        Some(entry)
            if entry.facts.running()
                || entry.facts.runtime == ContainerRuntimeObservation::Restarting =>
        {
            HolderStep::Keep(entry.facts.id)
        }
        Some(entry) => HolderStep::Start(entry.facts.id),
        None => HolderStep::Create(replacing(entries, None)),
    };
    (step, kept)
}

/// The doors a legacy Deploy keeps: it asks for a Caddy and knows no other Door, so
/// the holder's ports and the connector stay as this Machine has them.
fn doors_as_found(inventory: &[DoorEntry], proxy: ServiceId, tunnel_closed: bool) -> DoorSpecs {
    let newest_of = |door: Door| {
        newest(
            &inventory
                .iter()
                .filter(|entry| {
                    entry.facts.door == door
                        && usable(entry)
                        && !(tunnel_closed && door == Door::Tunnel)
                })
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

impl DoorsHeld {
    /// Whether this Machine holds a connector for a Tunnel Door the Cluster doesn't
    /// declare. The connector is the one door a restart has to bring up, since Docker
    /// starts the holder and Caddy by its own restart policy. The Public Door is not
    /// narrowed here: placing it only starts a holder that is at rest, and leaving it
    /// out would keep the connector of a declared Tunnel Door down.
    fn tunnel_left_closed(&self, declared: &IngressDoors) -> bool {
        self.specs.tunnel.is_some() && !declared.has(IngressDoor::Tunnel)
    }
}

/// The doors the inventory shows, or `None` when this Machine held no holder with a
/// Caddy joined to it, or its newest connector carries no readable token. Planning
/// without the connector would retire it. A connector left from a closed Tunnel Door
/// is not held: the Machine was told to close it, so planning retires it.
fn doors_held(inventory: &[DoorEntry], tunnel_closed: bool) -> Option<DoorsHeld> {
    let newest_of = |door: Door| {
        newest(
            &inventory
                .iter()
                .filter(|entry| {
                    entry.facts.door == door
                        && usable(entry)
                        && !(tunnel_closed && door == Door::Tunnel)
                })
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
    // A connector from before the Tunnel Origin has none; the pinned one stands in.
    let tunnel = tunnel
        .zip(sealed.as_ref())
        .map(|(connector, sealed)| TunnelSpecs {
            origin: newest_of(Door::TunnelOrigin).map_or_else(
                || TunnelSpecs::new(proxy.observation.resolved_spec.service_id, sealed).origin,
                spec,
            ),
            connector: spec(connector),
        });
    Some(DoorsHeld {
        specs: DoorSpecs {
            holder: spec(holder),
            proxy: spec(proxy),
            tunnel,
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
pub(super) struct Effects {
    /// Retirees that ran until this call stopped them, in stop order.
    pub(super) stopped: Vec<ContainerId>,
    /// Doors at rest that this call started, recorded before the start request.
    pub(super) started: Vec<ContainerId>,
    /// Doors this call created, in creation order.
    pub(super) created: Vec<ContainerId>,
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
            let containers = self.docker()?;
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
    /// and a Machine with no doors places none. A connector left from a closed Tunnel
    /// Door, which Docker refused to remove, is retired again instead of started.
    ///
    /// The Cluster's declared Doors can only narrow this, and only the connector: a
    /// Tunnel Door the declaration lacks stays at rest with no token written, as
    /// `ployz ingress use` left it, and nothing is created, replaced or stopped here.
    /// Every other door is placed, so a Machine part way through a switch that still
    /// holds both Doors gets its declared connector back. A connector an earlier daemon
    /// placed in the holder's network is stopped first, whatever else this does. With no declaration, or one
    /// this Machine can't read, the doors it held are placed as above.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TunnelToken`] when the connector's token no longer opens, and
    /// the errors of the Docker reads and the token write. A Docker stage that fails
    /// is answered in the result's `failure`.
    pub(crate) async fn restore_doors(&self) -> Result<Option<IngressDoorsSet>, Error> {
        let containers = self.docker()?.clone();
        self.door_effect()?;
        self.finish_installed(move |local, _installation| async move {
            let _doors = containers.door_lifecycle().await;
            local.door_effect()?;
            let inventory = containers.door_inventory(&local.record().id()).await?;
            for entry in inventory
                .iter()
                .filter(|entry| shares_holders_netns(entry) && !entry.facts.at_rest())
            {
                match containers.stop(&entry.facts.id, None, None).await {
                    Ok(()) | Err(crate::docker::Error::ContainerNotFound(_)) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            let Some(held) = doors_held(&inventory, containers.tunnel_door_closed()) else {
                return Ok(None);
            };
            if let Some(declared) = local.declared_doors().await
                && held.tunnel_left_closed(&declared)
            {
                tracing::warn!(
                    declared = ?declared.names(),
                    "this Machine held the Tunnel Door open but the Cluster doesn't declare it; the connector stays at rest until the next `ployz ingress use`"
                );
                return Ok(None);
            }
            let pinned = doors_as_found(
                &inventory,
                held.specs.proxy.service_id,
                containers.tunnel_door_closed(),
            );
            if pinned != held.specs {
                tracing::warn!(
                    "a door here was created from a spec this daemon doesn't build; it is replaced with the pinned one, not started"
                );
            }
            for spec in [&pinned.holder, &pinned.proxy]
                .into_iter()
                .chain(pinned.tunnel.iter().flat_map(TunnelSpecs::specs))
            {
                containers
                    .prepare_pinned_image(&spec.container.image)
                    .await?;
            }
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
                .converge_doors_admitted(&containers, &inventory, &pinned, Legacy::No)
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
        let containers = self.docker()?.clone();
        self.door_effect()?;
        let specs = specs.clone();
        self.finish_installed(move |local, _installation| async move {
            let _doors = containers.door_lifecycle().await;
            local.door_effect()?;
            let machine_id = local.record().id();
            let inventory = containers.door_inventory(&machine_id).await?;
            let specs = match legacy {
                Legacy::Caddy => doors_as_found(
                    &inventory,
                    specs.proxy.service_id,
                    containers.tunnel_door_closed(),
                ),
                Legacy::No => specs,
            };
            for spec in [&specs.holder, &specs.proxy]
                .into_iter()
                .chain(specs.tunnel.iter().flat_map(TunnelSpecs::specs))
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

    /// Carry out the plan in four stages: stop, holder, proxy, tunnel. The tunnel stage
    /// places the Tunnel Origin, then the connector in its network.
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
        let reopening = specs.tunnel.is_some() && containers.tunnel_door_closed();
        if reopening {
            containers
                .clear_tunnel_door_closed()
                .map_err(Error::TunnelDoorMarker)?;
        }
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
            let undone = undo && !matches!(error, Error::Cleanup(_));
            let connector_may_run = stage == IngressStage::Tunnel && !undone;
            if reopening
                && !connector_may_run
                && let Err(marker) = containers.mark_tunnel_door_closed()
            {
                tracing::warn!(
                    error = %ployz_core::error_chain::inline(&marker),
                    "could not record that the Tunnel Door is still closed"
                );
            }
            set.failure = Some(stage_failure(stage, &error));
            return Ok(set);
        }
        let marked = if specs.tunnel.is_none() && !plan.retire_tunnels.is_empty() {
            containers.mark_tunnel_door_closed()
        } else {
            Ok(())
        };
        let mut connector_stays = false;
        for retiree in plan.retirees() {
            match containers.remove(&retiree.id, true, true).await {
                Ok(()) => set.retired.push(retiree.id),
                Err(crate::docker::Error::ContainerNotFound(_)) => {}
                // Stopped and held, it serves nothing, and the next call retires it.
                Err(error) => {
                    connector_stays |= plan.retire_tunnels.contains(retiree);
                    tracing::warn!(
                        container = %retiree.id,
                        error = %ployz_core::error_chain::inline(&error),
                        "could not remove a replaced Ingress door"
                    );
                }
            }
        }
        if specs.tunnel.is_none() {
            if connector_stays {
                marked.map_err(Error::TunnelDoorMarker)?;
            } else {
                containers
                    .clear_tunnel_door_closed()
                    .map_err(Error::TunnelDoorMarker)?;
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
            self.place_owner(containers, &specs.holder, &plan.holder, effects)
                .await
                .map_err(holder_stage)?,
        );
        set.proxy = Some(
            self.place_joined(containers, &specs.proxy, &plan.proxy, effects)
                .await
                .map_err(|error| (IngressStage::Proxy, error))?,
        );
        if let (Some(pair), Some(step)) = (&specs.tunnel, &plan.tunnel) {
            let tunnel_stage = |error| (IngressStage::Tunnel, error);
            self.place_owner(containers, &pair.origin, &step.origin, effects)
                .await
                .map_err(tunnel_stage)?;
            set.tunnel = Some(
                self.place_joined(containers, &pair.connector, &step.connector, effects)
                    .await
                    .map_err(tunnel_stage)?,
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

    async fn place_owner(
        &self,
        containers: &ContainerRuntime,
        spec: &ResolvedServiceSpec,
        step: &HolderStep,
        effects: &mut Effects,
    ) -> Result<DoorOutcome, Error> {
        let (id, change) = match step {
            HolderStep::Keep(id) => (*id, DoorChange::Unchanged),
            HolderStep::Start(id) => {
                effects.started.push(*id);
                self.start_door(containers, id, Gate::Ingress).await?;
                (*id, DoorChange::Started)
            }
            HolderStep::Create(replacing) => {
                let id = self
                    .create_door(spec, *replacing, effects, Gate::Ingress)
                    .await?;
                self.start_door(containers, &id, Gate::Ingress).await?;
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
                self.start_door(containers, id, Gate::Ingress).await?;
                (*id, DoorChange::Started)
            }
            JoinedStep::Rejoin(id) => {
                self.door_effect()?;
                containers.stop(id, None, None).await?;
                self.start_door(containers, id, Gate::Ingress).await?;
                (*id, DoorChange::Rejoined)
            }
            JoinedStep::Create(replacing) => {
                let id = self
                    .create_door(spec, *replacing, effects, Gate::Ingress)
                    .await?;
                self.start_door(containers, &id, Gate::Ingress).await?;
                (id, DoorChange::Created)
            }
        };
        let now = containers.inspect_managed(&id, &self.record().id()).await?;
        Ok(DoorOutcome { now, change })
    }

    /// The Machine may place doors now: it participates, accepts ingress and is not
    /// leaving. Checked again before every effect, since a stage may be slow.
    fn door_effect(&self) -> Result<(), Error> {
        self.reserved_effect(Gate::Ingress)
    }

    /// The Machine may place a reserved Container now. `gate` names the role it needs.
    pub(super) fn reserved_effect(&self, gate: Gate) -> Result<(), Error> {
        let record = self.record();
        if !matches!(
            record.phase(),
            ployz_core::LocalMachinePhase::Joining | ployz_core::LocalMachinePhase::Participating
        ) {
            return Err(Error::NotParticipating);
        }
        let machine = record.machine().ok_or(Error::NotParticipating)?;
        if gate == Gate::Ingress && !machine.accepts_ingress {
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

    pub(super) async fn start_door(
        &self,
        containers: &ContainerRuntime,
        id: &ContainerId,
        gate: Gate,
    ) -> Result<(), Error> {
        self.reserved_effect(gate)?;
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
            if runtime.at_rest() || tokio::time::Instant::now() >= deadline {
                return Err(Error::DoorNotRunning {
                    container_id: *id,
                    runtime,
                });
            }
            tokio::time::sleep(DOOR_POLL).await;
        }
    }

    /// Create a door and record it in `effects` as soon as Docker names it.
    pub(super) async fn create_door(
        &self,
        spec: &ResolvedServiceSpec,
        replacing: Replacing,
        effects: &mut Effects,
        gate: Gate,
    ) -> Result<ContainerId, Error> {
        self.reserved_effect(gate)?;
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
                key,
                None,
                Gen::FIRST,
                ImagePull::Prepared,
            )
            .await?;
        effects.created.push(created.container_id);
        Ok(created.container_id)
    }

    /// Undo `effects`: remove the doors the call created and stop the ones it started,
    /// each joined door before the one whose network it is in. Only then start again
    /// the ones it stopped while they ran, owners before the doors that join them. A failure of any
    /// step is folded into `error`, and one that leaves a door of this call running or
    /// present keeps the previous doors stopped.
    pub(super) async fn undo(
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

    fn pair() -> TunnelSpecs {
        tunnel_specs().tunnel.unwrap()
    }

    fn origin(id: char, runtime: serde_json::Value) -> DoorEntry {
        entry(id, Door::TunnelOrigin, &pair().origin, 3, runtime)
    }

    fn tunnel(id: char, origin: char, runtime: serde_json::Value) -> DoorEntry {
        let mut entry = entry(id, Door::Tunnel, &pair().connector, 4, runtime);
        entry.facts.joined_to = Some(self::id(origin));
        entry
    }

    /// A connector an earlier daemon built: joined to the holder's network namespace,
    /// where it reached everything Caddy reaches.
    fn shared_netns_tunnel(id: char, holder: char, runtime: serde_json::Value) -> DoorEntry {
        let mut spec = pair().connector;
        spec.container.network = Some(ployz_core::ServiceNetwork::IngressHolder);
        let mut entry = entry(id, Door::Tunnel, &spec, 3, runtime);
        entry.facts.joined_to = Some(self::id(holder));
        entry
    }

    fn tunnel_plan(origin: HolderStep, connector: JoinedStep) -> Option<TunnelPlan> {
        Some(TunnelPlan { origin, connector })
    }

    #[test]
    fn opening_the_tunnel_door_replaces_the_public_holder_and_creates_the_pair() {
        let plan = plan_doors(
            &[holder('a', running()), caddy('b', 'a', running())],
            &tunnel_specs(),
            false,
        );
        assert_eq!(plan.holder, HolderStep::Create(Some(id('a'))));
        assert_eq!(plan.retire_holders, vec![retiree('a', true)]);
        assert_eq!(plan.proxy, JoinedStep::Create(Some(id('b'))));
        assert_eq!(
            plan.tunnel,
            tunnel_plan(HolderStep::Create(None), JoinedStep::Create(None))
        );
        assert!(plan.retire_tunnels.is_empty());
        assert!(plan.creates_at(IngressStage::Tunnel));
    }

    #[test]
    fn closing_the_tunnel_door_retires_each_joined_door_before_its_owner() {
        let plan = plan_doors(
            &[
                in_netns(closed_holder('a', running()), 10),
                in_netns(caddy('b', 'a', running()), 10),
                in_netns(origin('e', running()), 20),
                in_netns(tunnel('c', 'e', running()), 20),
            ],
            &specs(),
            false,
        );
        assert_eq!(plan.holder, HolderStep::Create(Some(id('a'))));
        assert_eq!(plan.tunnel, None);
        assert_eq!(plan.retire_tunnels, vec![retiree('c', true)]);
        assert_eq!(plan.retire_origins, vec![retiree('e', true)]);
        assert_eq!(
            plan.retirees()
                .map(|retiree| retiree.id)
                .collect::<Vec<_>>(),
            vec![id('c'), id('e'), id('b'), id('a')],
            "the connector stops before its origin, and the Caddy before its holder"
        );
    }

    #[test]
    fn a_running_tunnel_set_is_left_alone_and_a_new_token_replaces_the_connector() {
        let inventory = [
            in_netns(closed_holder('a', running()), 10),
            in_netns(caddy('b', 'a', running()), 10),
            in_netns(origin('e', running()), 20),
            in_netns(tunnel('c', 'e', running()), 20),
        ];
        let plan = plan_doors(&inventory, &tunnel_specs(), false);
        assert_eq!(plan.holder, HolderStep::Keep(id('a')));
        assert_eq!(plan.proxy, JoinedStep::Keep(id('b')));
        assert_eq!(
            plan.tunnel,
            tunnel_plan(HolderStep::Keep(id('e')), JoinedStep::Keep(id('c')))
        );
        assert_eq!(plan.retirees().count(), 0);

        let other_token = ployz_core::Ciphertext::from_bytes(vec![8; 48]);
        let renewed = DoorSpecs::new(
            ServiceId::parse("c".repeat(32)).unwrap(),
            IngressPorts::Closed,
            Some(&other_token),
        );
        let plan = plan_doors(&inventory, &renewed, false);
        assert_eq!(plan.holder, HolderStep::Keep(id('a')));
        assert_eq!(
            plan.tunnel,
            tunnel_plan(HolderStep::Keep(id('e')), JoinedStep::Create(Some(id('c'))))
        );
        assert_eq!(plan.retire_tunnels, vec![retiree('c', true)]);
        assert!(plan.retire_origins.is_empty());
    }

    #[test]
    fn the_tunnel_pair_is_kept_on_its_own_bridge_when_the_holder_is_replaced() {
        let plan = plan_doors(
            &[
                in_netns(holder('a', running()), 10),
                in_netns(caddy('b', 'a', running()), 10),
                in_netns(origin('e', running()), 20),
                in_netns(tunnel('c', 'e', running()), 20),
            ],
            &tunnel_specs(),
            false,
        );
        assert_eq!(plan.holder, HolderStep::Create(Some(id('a'))));
        assert_eq!(plan.proxy, JoinedStep::Create(Some(id('b'))));
        assert_eq!(
            plan.tunnel,
            tunnel_plan(HolderStep::Keep(id('e')), JoinedStep::Keep(id('c')))
        );
        assert!(plan.retire_tunnels.is_empty());
    }

    #[test]
    fn a_connector_left_by_a_restarted_origin_rejoins_it() {
        let plan = plan_doors(
            &[
                in_netns(closed_holder('a', running()), 10),
                in_netns(caddy('b', 'a', running()), 10),
                in_netns(origin('e', running()), 21),
                in_netns(tunnel('c', 'e', running()), 20),
            ],
            &tunnel_specs(),
            false,
        );
        assert_eq!(
            plan.tunnel,
            tunnel_plan(HolderStep::Keep(id('e')), JoinedStep::Rejoin(id('c')))
        );
        let plan = plan_doors(
            &[
                in_netns(closed_holder('a', running()), 10),
                in_netns(caddy('b', 'a', running()), 10),
                origin('e', created()),
                tunnel('c', 'e', created()),
            ],
            &tunnel_specs(),
            false,
        );
        assert_eq!(
            plan.tunnel,
            tunnel_plan(HolderStep::Start(id('e')), JoinedStep::Start(id('c'))),
            "after a reboot the origin starts, then the connector in its new network"
        );
    }

    #[test]
    fn a_connector_in_the_holders_network_namespace_is_replaced() {
        let plan = plan_doors(
            &[
                in_netns(closed_holder('a', running()), 10),
                in_netns(caddy('b', 'a', running()), 10),
                in_netns(shared_netns_tunnel('c', 'a', running()), 10),
            ],
            &tunnel_specs(),
            false,
        );
        assert_eq!(plan.holder, HolderStep::Keep(id('a')));
        assert_eq!(plan.proxy, JoinedStep::Keep(id('b')));
        assert_eq!(
            plan.tunnel,
            tunnel_plan(HolderStep::Create(None), JoinedStep::Create(Some(id('c'))))
        );
        assert_eq!(
            plan.retire_tunnels,
            vec![retiree('c', false)],
            "it ran, but an undo never starts it again"
        );
    }

    #[test]
    fn the_doors_found_are_the_newest_holders_ports_and_the_connectors_token() {
        let proxy = ServiceId::parse("c".repeat(32)).unwrap();
        assert_eq!(doors_as_found(&[], proxy, false), DoorSpecs::public(proxy));
        assert_eq!(
            doors_as_found(
                &[holder('a', running()), caddy('b', 'a', running())],
                proxy,
                false
            ),
            DoorSpecs::public(proxy)
        );
        assert_eq!(
            doors_as_found(
                &[
                    closed_holder('a', running()),
                    caddy('b', 'a', running()),
                    origin('e', running()),
                    tunnel('c', 'e', running())
                ],
                proxy,
                false
            ),
            tunnel_specs()
        );
        assert_eq!(
            doors_as_found(
                &[closed_holder('a', running()), legacy_caddy('d', running())],
                proxy,
                false
            ),
            DoorSpecs::new(proxy, IngressPorts::Closed, None),
            "a closed holder with no connector stays closed until a command opens a Door"
        );
        assert_eq!(
            doors_as_found(
                &[
                    closed_holder('a', running()),
                    caddy('b', 'a', running()),
                    tunnel('c', 'e', created())
                ],
                proxy,
                true
            ),
            DoorSpecs::new(proxy, IngressPorts::Closed, None),
            "a connector left from a closed Tunnel Door is not found"
        );
        assert!(
            doors_held(
                &[
                    closed_holder('a', running()),
                    caddy('b', 'a', running()),
                    tunnel('c', 'e', created())
                ],
                true
            )
            .is_some_and(|held| held.specs.tunnel.is_none() && held.sealed.is_none()),
            "and the doors held leave it out"
        );
    }

    #[test]
    fn an_absent_pair_is_created() {
        assert_eq!(
            plan_doors(&[], &specs(), false),
            DoorPlan {
                retire_tunnels: vec![],
                retire_origins: vec![],
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
    #[test]
    fn restore_leaves_only_an_undeclared_connector_at_rest() {
        let proxy = ServiceId::parse("c".repeat(32)).unwrap();
        let held = |ports: IngressPorts, tunnel: bool| {
            let mut specs = DoorSpecs::new(proxy, ports, None);
            if tunnel {
                specs.tunnel = Some(TunnelSpecs::new(proxy, &token()));
            }
            DoorsHeld {
                specs,
                sealed: None,
            }
        };
        let public = held(IngressPorts::Public, false);
        let tunnel = held(IngressPorts::Closed, true);
        let both = held(IngressPorts::Public, true);
        let closed = held(IngressPorts::Closed, false);
        let sets = [
            IngressDoors::closed(),
            IngressDoors::public(),
            IngressDoors::tunnel(),
            IngressDoors::both(),
        ];
        let expected = [
            // closed, public, tunnel, both
            (&closed, [false, false, false, false]),
            (&public, [false, false, false, false]),
            (&tunnel, [true, true, false, false]),
            (&both, [true, true, false, false]),
        ];
        for (held, answers) in expected {
            for (declared, answer) in sets.iter().zip(answers) {
                assert_eq!(held.tunnel_left_closed(declared), answer, "{declared:?}");
            }
        }
    }
}
