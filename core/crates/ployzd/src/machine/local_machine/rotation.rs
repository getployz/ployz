//! The owner's half of serving rotation. A copy's rotation moves only here, along
//! [`CopyRotation::edge`]. Each want is checked before any effect, and again under
//! the exclusive admission turn right before the write, against the rotation this
//! Machine recorded. Replicated rows from other Machines only narrow what it does.

use std::collections::{BTreeMap, BTreeSet};

use futures_util::{TryFutureExt, future::try_join_all};

use ployz_core::{
    ContainerId, ContainerKind, ContainerLocator, ContainerNow, ContainerObservation,
    ContainerRefusal, ContainerRefusalReason, ContainerWant, CopyRotation, DeploymentLogId,
    EnsuredContainer, Gen, HealthVerdict, MachineId, QualifiedService, RotationEdge,
    RotationRefusal, RotationTarget, RunRotation, RunTarget,
};

use super::{Error, LocalMachine, not_found_refusal, redacted, refuse};
use crate::route_acks::{Ackers, Membership, routed};

/// A rotation a want asks a copy to move to.
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

/// What a rotation check reads about one copy. A copy not created yet has no id.
#[derive(Clone, Debug)]
struct Copy {
    id: Option<ContainerId>,
    service: QualifiedService,
    r#gen: Gen,
    rotation: CopyRotation,
}

impl Copy {
    fn of(observed: &ContainerObservation) -> Self {
        Self {
            id: Some(observed.container_id),
            service: observed.identity(),
            r#gen: observed.copy_gen(),
            rotation: observed.rotation().clone(),
        }
    }
}

/// What the LastIn guard reads from replicated rows: each Machine's routed copies
/// of the Service, the Machines that must ack, and the rows of the routed copies.
struct Routed {
    routes: BTreeMap<MachineId, BTreeSet<ContainerId>>,
    membership: Membership,
    named: Vec<ContainerObservation>,
}

enum Checked {
    /// The copy already has the asked rotation: nothing to write.
    Met,
    Allowed(CopyRotation),
    Refused(ContainerRefusal),
}

/// A rotation as refusal messages name it.
pub(super) fn describe(rotation: &CopyRotation) -> String {
    match rotation {
        CopyRotation::Pending => "pending".to_owned(),
        CopyRotation::In => "in".to_owned(),
        CopyRotation::Held(run) => format!("held by {run}"),
        CopyRotation::Out => "out".to_owned(),
    }
}

impl LocalMachine {
    /// Out or Held: a write only, so the guards that read other copies run once, under
    /// the turn. The Container keeps running.
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
        let copy = Copy::of(observed);
        match decide(&copy, &want, expect.as_ref()) {
            Checked::Met => Ok(EnsuredContainer::Ensured {
                now: redacted(now),
                health: None,
            }),
            Checked::Refused(refusal) => Ok(refused(now, refusal)),
            Checked::Allowed(_) => self.write(copy, want, expect, None).await,
        }
    }

    /// In: start the copy as a Pending `Running` want does, settle its health, and
    /// write In only once it is healthy. A copy that does not settle healthy keeps
    /// its rotation, and the answer carries the verdict.
    pub(super) async fn route_in(
        &self,
        container: RunTarget,
        run: Option<DeploymentLogId>,
        expect: Option<CopyRotation>,
    ) -> Result<EnsuredContainer, Error> {
        let want = RotationWant::In { run };
        let locator = match &container {
            RunTarget::New(new) => ContainerLocator::Key(new.slot.clone()),
            RunTarget::Existing(locator) => locator.clone(),
        };
        let now = self.container_now(&locator).await?;
        let copy = match (&now, &container) {
            (ContainerNow::Present(observed), _) => Copy::of(observed),
            (ContainerNow::Absent { .. }, RunTarget::New(new)) => Copy {
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
        match self.check(&copy, &want, expect.as_ref(), None).await? {
            Checked::Met => {
                return Ok(EnsuredContainer::Ensured {
                    now: redacted(now),
                    health: None,
                });
            }
            Checked::Refused(refusal) => return Ok(refused(now, refusal)),
            Checked::Allowed(_) => {}
        }
        let started = self
            .ensure_docker(ContainerWant::Running {
                container,
                rotation: RunRotation::Pending,
            })
            .await?;
        let EnsuredContainer::Ensured {
            now: ContainerNow::Present(started),
            ..
        } = started
        else {
            return Ok(started);
        };
        let id = ContainerLocator::Id(started.container_id);
        let ContainerNow::Present(observed) = self.container_now(&id).await? else {
            return Ok(refused(
                ContainerNow::Absent { of: id.clone() },
                not_found_refusal(&id),
            ));
        };
        let health = self
            .settle_health(&observed.container_id, &observed.resolved_spec)
            .await?;
        if health != HealthVerdict::Healthy {
            return Ok(EnsuredContainer::Ensured {
                now: redacted(self.container_now(&id).await?),
                health: Some(health),
            });
        }
        self.write(Copy::of(&observed), want, expect, Some(health))
            .await
    }

    /// Check `want` again under the exclusive turn, against the rotation this Machine
    /// recorded, and write it. Holds the lock `remove` takes from the read to the write.
    async fn write(
        &self,
        copy: Copy,
        want: RotationWant,
        expect: Option<CopyRotation>,
        health: Option<HealthVerdict>,
    ) -> Result<EnsuredContainer, Error> {
        let id = copy
            .id
            .expect("only an existing copy reaches the rotation write");
        let locator = ContainerLocator::Id(id);
        // Replicated rows are not guarded by the turn, so the LastIn guard's reads of
        // them run alongside it; the local rotations it reads wait for the turn.
        let early = matches!(
            (&want, &copy.rotation),
            (RotationWant::Out { allow_empty: false }, CopyRotation::In)
        );
        let local = self.clone();
        let refusal = self
            .finish_mutation({
                let locator = locator.clone();
                async move {
                    let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
                    let (turn, routed) = tokio::join!(containers.rotation_turn(&id), async {
                        if early {
                            Some(local.routed(&copy).await)
                        } else {
                            None
                        }
                    });
                    let turn = match turn {
                        Ok(turn) => turn,
                        Err(crate::docker::Error::ContainerNotFound(_)) => {
                            return Ok(Some(not_found_refusal(&locator)));
                        }
                        Err(error) => return Err(error.into()),
                    };
                    let copy = Copy {
                        rotation: turn.rotation().clone(),
                        ..copy
                    };
                    match local.check(&copy, &want, expect.as_ref(), routed).await? {
                        Checked::Met => Ok(None),
                        Checked::Refused(refusal) => Ok(Some(refusal)),
                        Checked::Allowed(rotation) => {
                            turn.write(&rotation).await?;
                            Ok(None)
                        }
                    }
                }
            })
            .await?;
        let now = self.container_now(&locator).await?;
        Ok(match refusal {
            None => EnsuredContainer::Ensured {
                now: redacted(now),
                health,
            },
            Some(refusal) => refused(now, refusal),
        })
    }

    /// The want against `copy`: [`decide`], then the guards that read other copies.
    /// `routed` is the LastIn guard's replicated read when it was already taken.
    async fn check(
        &self,
        copy: &Copy,
        want: &RotationWant,
        expect: Option<&CopyRotation>,
        routed: Option<Result<Routed, Error>>,
    ) -> Result<Checked, Error> {
        let to = match decide(copy, want, expect) {
            Checked::Allowed(to) => to,
            decided @ (Checked::Met | Checked::Refused(_)) => return Ok(decided),
        };
        let guard = match (want, &copy.rotation) {
            (RotationWant::In { .. }, _) => self.newer_in(copy).await?,
            (RotationWant::Out { allow_empty: false }, CopyRotation::In) => {
                let routed = match routed {
                    Some(routed) => routed?,
                    None => self.routed(copy).await?,
                };
                self.last_in(copy, routed).await?
            }
            _ => None,
        };
        Ok(guard.map_or(Checked::Allowed(to), Checked::Refused))
    }

    /// Refuse In while a newer copy of the Service is already In.
    async fn newer_in(&self, copy: &Copy) -> Result<Option<ContainerRefusal>, Error> {
        let observed = self.replicated()?.containers().await?.observations;
        let newer = self
            .in_siblings(copy, observed)
            .await?
            .into_iter()
            .filter(|sibling| sibling.copy_gen() > copy.r#gen)
            .max_by_key(|sibling| sibling.copy_gen());
        Ok(newer.map(|sibling| {
            refuse(
                ContainerRefusalReason::OlderThanIn,
                format!(
                    "copy {} of {} is In at gen {}, newer than this copy's gen {}",
                    sibling.container_id,
                    copy.service,
                    sibling.copy_gen(),
                    copy.r#gen
                ),
            )
        }))
    }

    /// The LastIn guard's reads of replicated rows. Only copies some ack row names can
    /// pass, so only those rows are read.
    async fn routed(&self, copy: &Copy) -> Result<Routed, Error> {
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
    /// proxy. A Machine list that is only partly replicated never passes.
    async fn last_in(
        &self,
        copy: &Copy,
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

    /// The other In copies of `copy`'s Service among `observed`. A copy on this
    /// Machine reads this Machine's own record instead of its row, which may lag.
    async fn in_siblings(
        &self,
        copy: &Copy,
        observed: Vec<ContainerObservation>,
    ) -> Result<Vec<ContainerObservation>, Error> {
        let machine_id = self.record().id();
        let others = observed
            .into_iter()
            .filter(|observed| {
                observed.kind == ContainerKind::ServiceContainer
                    && Some(observed.container_id) != copy.id
                    && observed.identity() == copy.service
            })
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

/// The want against `copy` alone: already met, then `expect`, then the edge table.
fn decide(copy: &Copy, want: &RotationWant, expect: Option<&CopyRotation>) -> Checked {
    if want.met_by(&copy.rotation) {
        return Checked::Met;
    }
    if let Some(expect) = expect
        && *expect != copy.rotation
    {
        return Checked::Refused(refuse(
            ContainerRefusalReason::RotationMismatch,
            format!(
                "the copy is {}, not {} as expected",
                describe(&copy.rotation),
                describe(expect)
            ),
        ));
    }
    match copy.rotation.edge(&want.target()) {
        RotationEdge::Write(to) => Checked::Allowed(to),
        RotationEdge::Refused(refusal) => Checked::Refused(edge_refusal(refusal, copy, want)),
        RotationEdge::Keep | RotationEdge::Remove => {
            unreachable!("a met want returned early, and no rotation want removes")
        }
    }
}

fn edge_refusal(refusal: RotationRefusal, copy: &Copy, want: &RotationWant) -> ContainerRefusal {
    let from = describe(&copy.rotation);
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
