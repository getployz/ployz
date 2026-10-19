//! The owner's half of serving rotation. A copy's rotation moves only here, along
//! [`CopyRotation::edge`]. Each want is checked before any effect, and again under
//! the exclusive admission turn right before the write, against the rotation this
//! Machine recorded. Replicated rows from other Machines are read before the turn
//! and only narrow what it does, but they lag: an Out written on another Machine is
//! invisible here until it replicates, so `LastIn` can count a sibling that just left.

use std::collections::{BTreeMap, BTreeSet};

use futures_util::{TryFutureExt, future::try_join_all};

use ployz_core::{
    ContainerId, ContainerKind, ContainerLocator, ContainerNow, ContainerObservation,
    ContainerRefusal, ContainerRefusalReason, ContainerRuntimeObservation, ContainerWant,
    CopyRotation, DeploymentLogId, EnsuredContainer, Gen, HealthVerdict, MachineId,
    QualifiedService, RotationEdge, RotationRefusal, RotationTarget, RunRotation, RunTarget,
};

use super::{Error, LocalMachine, not_found_refusal, redacted, refuse};
use crate::docker::ContainerRuntime;
use crate::route_acks::{Ackers, Membership, routed};

#[derive(Clone, Debug)]
pub(super) enum RotationWant {
    In { run: Option<DeploymentLogId> },
    Held(DeploymentLogId),
    Out { allow_empty: bool },
}

impl RotationWant {
    fn target(&self) -> RotationTarget {
        match self {
            Self::In { run } => RotationTarget::In { run: run.clone() },
            Self::Held(run) => RotationTarget::Held(run.clone()),
            Self::Out { .. } => RotationTarget::Out,
        }
    }

    fn met_by(&self, rotation: &CopyRotation) -> bool {
        match (self, rotation) {
            (Self::In { .. }, CopyRotation::In) | (Self::Out { .. }, CopyRotation::Out) => true,
            (Self::Held(run), CopyRotation::Held(held)) => run == held,
            _ => false,
        }
    }
}

/// A copy not created yet has no id.
#[derive(Clone, Debug)]
struct CopyState {
    id: Option<ContainerId>,
    service: QualifiedService,
    r#gen: Gen,
    rotation: CopyRotation,
}

impl CopyState {
    fn of(observed: &ContainerObservation) -> Self {
        Self {
            id: Some(observed.container_id),
            service: observed.identity(),
            r#gen: observed.copy_gen(),
            rotation: observed.rotation().clone(),
        }
    }
}

struct Routed {
    routes: BTreeMap<MachineId, BTreeSet<ContainerId>>,
    membership: Membership,
    named: Vec<ContainerObservation>,
}

/// Read before the turn. Only this copy's rotation and this Machine's copies are
/// read again under it.
enum GuardReads {
    None,
    /// Every copy's row, for the OlderThanIn guard.
    NewerIn(Vec<ContainerObservation>),
    LastIn(Routed),
}

enum Written {
    Done,
    /// The copy no longer runs, so In was not written.
    NotRunning(ContainerRuntimeObservation),
    Refused(ContainerRefusal),
}

enum Checked {
    Met,
    Allowed(CopyRotation),
    Refused(ContainerRefusal),
}

pub(super) fn describe(rotation: &CopyRotation) -> String {
    match rotation {
        CopyRotation::Pending => "pending".to_owned(),
        CopyRotation::In => "in".to_owned(),
        CopyRotation::Held(run) => format!("held by {run}"),
        CopyRotation::Out => "out".to_owned(),
    }
}

impl LocalMachine {
    /// Out or Held. The guards that read other copies run once, inside `write`.
    pub(super) async fn rotate(
        &self,
        locator: ContainerLocator,
        want: RotationWant,
        expect: Option<CopyRotation>,
    ) -> Result<EnsuredContainer, Error> {
        let now = self.container_now(&locator).await?;
        let Some(observed) = now.present() else {
            return Ok(refused(now, not_found_refusal(&locator)));
        };
        let copy = CopyState::of(observed);
        match decide(&copy.rotation, &want, expect.as_ref()) {
            Checked::Met => Ok(EnsuredContainer::Ensured {
                now: redacted(now),
                health: None,
            }),
            Checked::Refused(refusal) => Ok(refused(now, refusal)),
            Checked::Allowed(_) => self.write(copy, want, expect, None).await,
        }
    }

    /// Writes In only once the copy is healthy. An unhealthy copy keeps its rotation,
    /// and the answer carries the verdict.
    pub(super) async fn route_in(
        &self,
        container: RunTarget,
        run: Option<DeploymentLogId>,
        expect: Option<CopyRotation>,
    ) -> Result<EnsuredContainer, Error> {
        let want = RotationWant::In { run: run.clone() };
        let locator = match &container {
            RunTarget::New(new) => ContainerLocator::Key(new.slot.clone()),
            RunTarget::Existing(locator) => locator.clone(),
        };
        let now = self.container_now(&locator).await?;
        let copy = match (&now, &container) {
            (ContainerNow::Present(observed), _) => CopyState::of(observed),
            (ContainerNow::Absent { .. }, RunTarget::New(new)) => CopyState {
                id: None,
                service: QualifiedService::new(
                    new.slot.namespace.clone(),
                    new.resolved_spec.name.clone(),
                ),
                r#gen: new.r#gen,
                rotation: CopyRotation::Pending,
            },
            (ContainerNow::Absent { .. }, RunTarget::Existing(_)) => {
                return Ok(refused(now, not_found_refusal(&locator)));
            }
        };
        let checked = match decide(&copy.rotation, &want, expect.as_ref()) {
            Checked::Allowed(_) => {
                let reads = self.guard_reads(&copy, &want).await?;
                self.check(&copy, &want, expect.as_ref(), reads).await?
            }
            decided @ (Checked::Met | Checked::Refused(_)) => decided,
        };
        let met = match checked {
            Checked::Refused(refusal) => return Ok(refused(now, refusal)),
            Checked::Met => true,
            Checked::Allowed(_) => false,
        };
        // A copy already In still runs: Met only skips the rotation write.
        let started = self
            .ensure_docker(ContainerWant::Running {
                container,
                rotation: RunRotation::In {
                    run,
                    expect: expect.clone(),
                },
            })
            .await?;
        if met {
            return Ok(started);
        }
        let EnsuredContainer::Ensured {
            now: ContainerNow::Present(observed),
            health,
        } = &started
        else {
            return Ok(started);
        };
        let id = ContainerLocator::Id(observed.container_id);
        let health = match health {
            Some(settled) => settled.clone(),
            None => {
                let ContainerNow::Present(observed) = self.container_now(&id).await? else {
                    return Ok(refused(
                        ContainerNow::Absent { of: id.clone() },
                        not_found_refusal(&id),
                    ));
                };
                self.settle_health(&observed.container_id, &observed.resolved_spec)
                    .await?
            }
        };
        if health != HealthVerdict::Healthy {
            return Ok(EnsuredContainer::Ensured {
                now: redacted(self.container_now(&id).await?),
                health: Some(health),
            });
        }
        self.write(CopyState::of(observed), want, expect, Some(health))
            .await
    }

    /// Starts an existing copy for an In want, as [`start_fenced`] does.
    pub(super) async fn start_routed(
        &self,
        id: ContainerId,
        run: Option<DeploymentLogId>,
        expect: Option<CopyRotation>,
    ) -> Result<Option<ContainerRefusal>, Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
            start_fenced(containers, &id, run, expect.as_ref()).await
        })
        .await
    }

    /// Re-checks `want` under the exclusive turn against this Machine's record,
    /// holding the lock `remove` takes from read to write. In is written only on a
    /// copy that still runs.
    async fn write(
        &self,
        copy: CopyState,
        want: RotationWant,
        expect: Option<CopyRotation>,
        health: Option<HealthVerdict>,
    ) -> Result<EnsuredContainer, Error> {
        let id = copy
            .id
            .expect("only an existing copy reaches the rotation write");
        let locator = ContainerLocator::Id(id);
        let reads = self.guard_reads(&copy, &want).await?;
        let local = self.clone();
        let written = self
            .finish_mutation({
                let locator = locator.clone();
                async move {
                    let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
                    let turn = match containers.rotation_turn(&id).await {
                        Ok(turn) => turn,
                        Err(crate::docker::Error::ContainerNotFound(_)) => {
                            return Ok(Written::Refused(not_found_refusal(&locator)));
                        }
                        Err(error) => return Err(error.into()),
                    };
                    let copy = CopyState {
                        rotation: turn.rotation().clone(),
                        ..copy
                    };
                    let to = match local.check(&copy, &want, expect.as_ref(), reads).await? {
                        Checked::Met => return Ok(Written::Done),
                        Checked::Refused(refusal) => return Ok(Written::Refused(refusal)),
                        Checked::Allowed(to) => to,
                    };
                    if let RotationWant::In { .. } = want {
                        let runtime = containers.inspect_runtime(&id).await?;
                        if !super::is_running(&runtime) {
                            return Ok(Written::NotRunning(runtime));
                        }
                    }
                    turn.write(&to).await?;
                    Ok(Written::Done)
                }
            })
            .await?;
        let now = self.container_now(&locator).await?;
        Ok(match written {
            Written::Done => EnsuredContainer::Ensured {
                now: redacted(now),
                health,
            },
            Written::NotRunning(observation) => EnsuredContainer::Ensured {
                now: redacted(now),
                health: Some(HealthVerdict::Failed {
                    observation,
                    last_check: None,
                }),
            },
            Written::Refused(refusal) => refused(now, refusal),
        })
    }

    /// Read outside the turn.
    async fn guard_reads(
        &self,
        copy: &CopyState,
        want: &RotationWant,
    ) -> Result<GuardReads, Error> {
        Ok(match want {
            RotationWant::In { .. } => {
                GuardReads::NewerIn(self.replicated()?.containers().await?.observations)
            }
            RotationWant::Out { allow_empty: false } => {
                GuardReads::LastIn(self.routed(copy).await?)
            }
            RotationWant::Out { allow_empty: true } | RotationWant::Held(_) => GuardReads::None,
        })
    }

    async fn check(
        &self,
        copy: &CopyState,
        want: &RotationWant,
        expect: Option<&CopyRotation>,
        reads: GuardReads,
    ) -> Result<Checked, Error> {
        let to = match decide(&copy.rotation, want, expect) {
            Checked::Allowed(to) => to,
            decided @ (Checked::Met | Checked::Refused(_)) => return Ok(decided),
        };
        let guard = match (reads, &copy.rotation) {
            (GuardReads::NewerIn(observed), _) => self.newer_in(copy, observed).await?,
            (GuardReads::LastIn(routed), CopyRotation::In) => self.last_in(copy, routed).await?,
            (GuardReads::LastIn(_), _) | (GuardReads::None, _) => None,
        };
        Ok(guard.map_or(Checked::Allowed(to), Checked::Refused))
    }

    /// Refuse In while a newer copy of the Service is already In. This Machine's copies
    /// are listed afresh from their labels and record, so one created since `observed`
    /// was read still counts and no health check runs under the turn.
    async fn newer_in(
        &self,
        copy: &CopyState,
        observed: Vec<ContainerObservation>,
    ) -> Result<Option<ContainerRefusal>, Error> {
        let machine_id = self.record().id();
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let local = containers
            .service_copies(&copy.service)
            .await?
            .into_iter()
            .filter(|local| local.rotation == CopyRotation::In)
            .map(|local| (local.container_id, local.r#gen));
        let newer = observed
            .into_iter()
            .filter(|observed| {
                observed.machine_id != machine_id
                    && is_sibling(copy, observed)
                    && *observed.rotation() == CopyRotation::In
            })
            .map(|observed| (observed.container_id, observed.copy_gen()))
            .chain(local)
            .filter(|(id, r#gen)| Some(*id) != copy.id && *r#gen > copy.r#gen)
            .max_by_key(|(_, r#gen)| *r#gen);
        Ok(newer.map(|(id, r#gen)| {
            refuse(
                ContainerRefusalReason::OlderThanIn,
                format!(
                    "copy {id} of {} is In at gen {}, newer than this copy's gen {}",
                    copy.service, r#gen, copy.r#gen
                ),
            )
        }))
    }

    /// Only copies some ack row names can pass, so only those rows are read.
    async fn routed(&self, copy: &CopyState) -> Result<Routed, Error> {
        let replicated = self.replicated()?;
        let (rows, membership) = tokio::try_join!(
            replicated
                .service_ingress_routes(&copy.service)
                .map_err(Error::from),
            self.route_membership()
        )?;
        let routes = routed(rows, &copy.service);
        let named = routes
            .values()
            .flatten()
            .filter(|routed| Some(**routed) != copy.id)
            .collect::<BTreeSet<_>>();
        let named = try_join_all(
            named
                .into_iter()
                .map(|candidate| replicated.container(candidate)),
        )
        .await?
        .into_iter()
        .flatten()
        .collect();
        Ok(Routed {
            routes,
            membership,
            named,
        })
    }

    /// Refuse In to Out unless another In copy of the Service is acked by every live
    /// proxy. A Machine list that is only partly replicated never passes, and a copy
    /// on a Down Machine serves no one.
    async fn last_in(
        &self,
        copy: &CopyState,
        Routed {
            routes,
            membership,
            named,
        }: Routed,
    ) -> Result<Option<ContainerRefusal>, Error> {
        let siblings = self
            .in_siblings(copy, named)
            .await?
            .into_iter()
            .filter(|sibling| !membership.down.contains(&sibling.machine_id))
            .map(|sibling| sibling.container_id)
            .collect::<Vec<_>>();
        if siblings.is_empty() {
            return Ok(Some(refuse(
                ContainerRefusalReason::LastIn,
                format!(
                    "no other copy of {} is In and routed; send allow_empty to empty the Service",
                    copy.service
                ),
            )));
        }
        let unacked = siblings
            .iter()
            .map(|sibling| {
                membership.ackers.unacked(|machine| {
                    routes
                        .get(machine)
                        .is_some_and(|routed| routed.contains(sibling))
                })
            })
            .min_by_key(Vec::len)
            .unwrap_or_default();
        let known = matches!(&membership.ackers, Ackers::Known(machines) if !machines.is_empty());
        if known && unacked.is_empty() {
            return Ok(None);
        }
        let waiting = if unacked.is_empty() {
            "no proxy Machine is known".to_owned()
        } else {
            format!("not acked by {}", unacked.join(", "))
        };
        Ok(Some(refuse(
            ContainerRefusalReason::LastIn,
            format!(
                "no other In copy of {} is acked by every live proxy: {waiting}",
                copy.service
            ),
        )))
    }

    /// A local copy reads this Machine's own record, since its replicated row may lag.
    async fn in_siblings(
        &self,
        copy: &CopyState,
        observed: Vec<ContainerObservation>,
    ) -> Result<Vec<ContainerObservation>, Error> {
        let machine_id = self.record().id();
        let others = observed
            .into_iter()
            .filter(|observed| is_sibling(copy, observed))
            .collect::<Vec<_>>();
        let local = others
            .iter()
            .filter(|observed| observed.machine_id == machine_id)
            .map(|observed| observed.container_id)
            .collect::<Vec<_>>();
        let recorded = if local.is_empty() {
            BTreeMap::new()
        } else {
            let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
            containers.rotations(local).await?
        };
        Ok(others
            .into_iter()
            .filter(|observed| {
                let rotation = if observed.machine_id == machine_id {
                    recorded.get(&observed.container_id)
                } else {
                    Some(observed.rotation())
                };
                rotation == Some(&CopyRotation::In)
            })
            .collect())
    }
}

fn is_sibling(copy: &CopyState, observed: &ContainerObservation) -> bool {
    observed.kind == ContainerKind::ServiceContainer
        && Some(observed.container_id) != copy.id
        && observed.identity() == copy.service
}

/// Starts `id` for an In want with `run` and `expect` only if the want still holds
/// against this Machine's record, read under the copy's rotation turn, so a refused
/// In starts nothing. The caller holds the admission turn.
pub(super) async fn start_fenced(
    containers: &ContainerRuntime,
    id: &ContainerId,
    run: Option<DeploymentLogId>,
    expect: Option<&CopyRotation>,
) -> Result<Option<ContainerRefusal>, Error> {
    let turn = containers.rotation_turn(id).await?;
    match decide(turn.rotation(), &RotationWant::In { run }, expect) {
        Checked::Refused(refusal) => return Ok(Some(refusal)),
        Checked::Met | Checked::Allowed(_) => {}
    }
    containers.start(id).await?;
    drop(turn);
    Ok(None)
}

fn decide(rotation: &CopyRotation, want: &RotationWant, expect: Option<&CopyRotation>) -> Checked {
    if want.met_by(rotation) {
        return Checked::Met;
    }
    if let Some(expect) = expect
        && expect != rotation
    {
        return Checked::Refused(refuse(
            ContainerRefusalReason::RotationMismatch,
            format!(
                "the copy is {}, not {} as expected",
                describe(rotation),
                describe(expect)
            ),
        ));
    }
    match rotation.edge(&want.target()) {
        RotationEdge::Write(to) => Checked::Allowed(to),
        RotationEdge::Refused(refusal) => Checked::Refused(edge_refusal(refusal, rotation, want)),
        RotationEdge::Keep | RotationEdge::Remove => {
            unreachable!("a met want returned early, and no rotation want removes")
        }
    }
}

fn edge_refusal(
    refusal: RotationRefusal,
    rotation: &CopyRotation,
    want: &RotationWant,
) -> ContainerRefusal {
    let from = describe(rotation);
    match refusal {
        RotationRefusal::HeldByOtherRun => refuse(
            ContainerRefusalReason::HeldByOtherRun,
            format!("the copy is {from}, and only that run may move it"),
        ),
        RotationRefusal::InRotation => refuse(
            ContainerRefusalReason::InRotation,
            format!("the copy is {from}; take it out of routing first"),
        ),
        RotationRefusal::OutIsFinal => refuse(
            ContainerRefusalReason::RotationMismatch,
            "the copy is out, and Out is final; deploy again",
        ),
        RotationRefusal::NoEdge => refuse(
            ContainerRefusalReason::RotationMismatch,
            format!("no move leads from {from} to {}", target_name(want)),
        ),
    }
}

fn target_name(want: &RotationWant) -> String {
    match want {
        RotationWant::In { .. } => "in".to_owned(),
        RotationWant::Held(run) => format!("held by {run}"),
        RotationWant::Out { .. } => "out".to_owned(),
    }
}

fn refused(now: ContainerNow, refusal: ContainerRefusal) -> EnsuredContainer {
    EnsuredContainer::Refused {
        now: redacted(now),
        refusal,
    }
}
