//! The choice every reserved placement makes before it touches Docker: which of the
//! Containers it finds is the canonical one, what to do with it, and which others
//! retire. The Ingress doors and the Private Access router share this choice and the
//! driver that carries a step out, and keep their own plans.

use std::time::Duration;

use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation, DoorChange,
    DoorOutcome, Gen, Namespace, ResolvedServiceSpec, global_creation_key,
};

use super::container::CreationFacts;
use super::{AdmittedSpec, Error, LocalMachine};
use crate::docker::ContainerRuntime;
use crate::docker_image::ImagePull;

/// What a placement does with the reserved Container it wants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Step {
    /// A canonical one runs, or is restarting.
    Keep(ContainerId),
    /// A canonical one exists at rest.
    Start(ContainerId),
    /// A canonical one runs in a network its owner no longer has. Only a door in
    /// another's network is planned this way.
    Rejoin(ContainerId),
    Create(Replacing),
}

/// The newest Container of the same identity a creation replaces. The old one keeps its
/// name until the new one runs, so the new one is named for what it replaces.
pub(super) type Replacing = Option<ContainerId>;

/// A reserved Container the route replaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Retiree {
    pub(super) id: ContainerId,
    /// It ran when planned, so a replacement that fails starts it again.
    pub(super) running: bool,
}

/// A reserved Container as Docker has it.
pub(super) trait Placed {
    fn id(&self) -> ContainerId;
    fn runtime(&self) -> &ContainerRuntimeObservation;
    fn created_at(&self) -> i64;
    /// Whether a replacement that fails starts it again.
    fn was_running(&self) -> bool;
}

impl Placed for ContainerObservation {
    fn id(&self) -> ContainerId {
        self.container_id
    }

    fn runtime(&self) -> &ContainerRuntimeObservation {
        &self.runtime
    }

    fn created_at(&self) -> i64 {
        self.created_at_unix_nanos
    }

    fn was_running(&self) -> bool {
        !self.runtime.at_rest()
    }
}

/// Whether the Container can still be kept: Docker is not taking it away.
pub(super) fn usable(placed: &impl Placed) -> bool {
    !matches!(
        placed.runtime(),
        ContainerRuntimeObservation::Removing | ContainerRuntimeObservation::Dead
    )
}

fn running(placed: &impl Placed) -> bool {
    placed.runtime().is_running()
}

/// A running Container before one at rest, then the latest created.
pub(super) fn newest<'placed, P: Placed + 'placed>(
    entries: impl IntoIterator<Item = &'placed P>,
) -> Option<&'placed P> {
    entries
        .into_iter()
        .max_by_key(|placed| (running(*placed), placed.created_at(), placed.id()))
}

/// The newest of `entries` not `kept`. A Container named for replacing it would be
/// newer still, so no Container holds the name a creation that replaces it takes.
pub(super) fn replacing<'placed, P: Placed + 'placed>(
    entries: impl IntoIterator<Item = &'placed P>,
    kept: Option<ContainerId>,
) -> Replacing {
    entries
        .into_iter()
        .filter(|placed| Some(placed.id()) != kept)
        .map(|placed| (placed.created_at(), placed.id()))
        .max()
        .map(|(_, id)| id)
}

pub(super) fn retirees<'placed, P: Placed + 'placed>(
    entries: impl IntoIterator<Item = &'placed P>,
    kept: Option<ContainerId>,
) -> Vec<Retiree> {
    entries
        .into_iter()
        .filter(|placed| Some(placed.id()) != kept)
        .map(|placed| Retiree {
            id: placed.id(),
            running: placed.was_running(),
        })
        .collect()
}

/// The step for the canonical Container `kept` among `entries`: keep one that runs or
/// is restarting, start one at rest, or create one named for the newest it replaces.
pub(super) fn step<'placed, P: Placed + 'placed>(
    kept: Option<&P>,
    entries: impl IntoIterator<Item = &'placed P>,
) -> Step {
    match kept {
        Some(placed)
            if running(placed) || *placed.runtime() == ContainerRuntimeObservation::Restarting =>
        {
            Step::Keep(placed.id())
        }
        Some(placed) => Step::Start(placed.id()),
        None => Step::Create(replacing(entries, None)),
    }
}

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
    /// The Machine may place a reserved Container now. `gate` names the role it needs.
    pub(super) fn reserved_effect(&self, gate: Gate) -> Result<(), Error> {
        let record = self.record();
        let machine = record.cluster_machine().ok_or(Error::NotParticipating)?;
        if gate == Gate::Ingress && !machine.accepts_ingress {
            return Err(crate::docker::Error::WorkNotAccepted.into());
        }
        if self
            .containers
            .as_ref()
            .is_some_and(|containers| containers.lifecycle().departing())
        {
            return Err(Error::Departing {
                server: machine.name.clone(),
                source: Box::new(crate::docker::Error::WorkNotAccepted.into()),
            });
        }
        Ok(())
    }

    /// Stop a Container the placement replaces, recording it in `effects` when it ran.
    pub(super) async fn stop_retiree(
        &self,
        containers: &ContainerRuntime,
        retiree: &Retiree,
        gate: Gate,
        effects: &mut Effects,
    ) -> Result<(), Error> {
        self.reserved_effect(gate)?;
        match containers.lifecycle().stop(&retiree.id, None, None).await {
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

    /// Carry out `step` for the Container `spec` describes, and read it back.
    pub(super) async fn place(
        &self,
        containers: &ContainerRuntime,
        spec: &ResolvedServiceSpec,
        step: &Step,
        gate: Gate,
        effects: &mut Effects,
    ) -> Result<DoorOutcome, Error> {
        let (id, change) = match step {
            Step::Keep(id) => (*id, DoorChange::Unchanged),
            Step::Start(id) => {
                effects.started.push(*id);
                self.start_door(containers, id, gate).await?;
                (*id, DoorChange::Started)
            }
            Step::Rejoin(id) => {
                self.reserved_effect(gate)?;
                containers.lifecycle().stop(id, None, None).await?;
                self.start_door(containers, id, gate).await?;
                (*id, DoorChange::Rejoined)
            }
            Step::Create(replacing) => {
                let id = self.create_door(spec, *replacing, effects, gate).await?;
                self.start_door(containers, &id, gate).await?;
                (id, DoorChange::Created)
            }
        };
        let now = containers
            .observation()
            .inspect_managed(&id, &self.record().id())
            .await?;
        Ok(DoorOutcome { now, change })
    }

    pub(super) async fn start_door(
        &self,
        containers: &ContainerRuntime,
        id: &ContainerId,
        gate: Gate,
    ) -> Result<(), Error> {
        self.reserved_effect(gate)?;
        containers.lifecycle().start(id).await?;
        let machine_id = self.record().id();
        let deadline = tokio::time::Instant::now() + DOOR_START_WAIT;
        loop {
            let runtime = containers
                .observation()
                .inspect_managed(id, &machine_id)
                .await?
                .runtime
                .clone();
            if runtime.is_running() {
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
            .create_container_admitted(CreationFacts {
                kind: ContainerKind::ServiceContainer,
                namespace: &Namespace::system(),
                spec,
                creation_key: key,
                deployment_id: None,
                r#gen: Gen::FIRST,
                image: ImagePull::Prepared,
                admitted: AdmittedSpec::own(&Namespace::system(), spec)?,
            })
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
            if let Err(removal) = containers.lifecycle().remove(id, true, true).await {
                cleared = false;
                error = Error::Cleanup(format!(
                    "{}; removing the new Ingress door {id} failed too: {}",
                    ployz_core::error_chain::inline(&error),
                    ployz_core::error_chain::inline(&removal),
                ));
            }
        }
        for id in effects.started.iter().rev() {
            if let Err(stop) = containers.lifecycle().stop(id, None, None).await {
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
            if let Err(restart) = containers.lifecycle().start(id).await {
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
    use ployz_core::HealthObservation;

    struct Fake {
        id: char,
        created_at: i64,
        runtime: ContainerRuntimeObservation,
    }

    impl Placed for Fake {
        fn id(&self) -> ContainerId {
            ContainerId::parse(self.id.to_string().repeat(64)).unwrap()
        }

        fn runtime(&self) -> &ContainerRuntimeObservation {
            &self.runtime
        }

        fn created_at(&self) -> i64 {
            self.created_at
        }

        fn was_running(&self) -> bool {
            !self.runtime.at_rest()
        }
    }

    fn fake(id: char, created_at: i64, runtime: ContainerRuntimeObservation) -> Fake {
        Fake {
            id,
            created_at,
            runtime,
        }
    }

    fn id(id: char) -> ContainerId {
        fake(id, 0, ContainerRuntimeObservation::Removing).id()
    }

    fn up() -> ContainerRuntimeObservation {
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        }
    }

    // Catches the doors and the router choosing differently: each runtime state of the
    // canonical Container maps to one step, and a creation names what it replaces.
    #[test]
    fn a_running_or_restarting_one_is_kept_one_at_rest_is_started_and_none_is_created() {
        let running = fake('a', 1, up());
        let restarting = fake('b', 1, ContainerRuntimeObservation::Restarting);
        let stopped = fake('c', 1, ContainerRuntimeObservation::Created);
        let all = [&running, &restarting, &stopped];
        assert_eq!(step(Some(&running), all), Step::Keep(id('a')));
        assert_eq!(step(Some(&restarting), all), Step::Keep(id('b')));
        assert_eq!(step(Some(&stopped), all), Step::Start(id('c')));
        assert_eq!(step::<Fake>(None, []), Step::Create(None));
        let older = fake('d', 5, ContainerRuntimeObservation::Removing);
        let newer = fake('e', 9, ContainerRuntimeObservation::Dead);
        assert_eq!(
            step(None, [&older, &newer]),
            Step::Create(Some(id('e'))),
            "a creation is named for the newest, whatever state it is in"
        );
    }

    #[test]
    fn the_canonical_one_is_the_newest_running_and_the_rest_retire() {
        let old_running = fake('a', 1, up());
        let new_stopped = fake('b', 9, ContainerRuntimeObservation::Created);
        let new_running = fake('c', 5, up());
        let dying = fake('d', 20, ContainerRuntimeObservation::Removing);
        let all = [&old_running, &new_stopped, &new_running, &dying];
        assert!(!usable(&dying));
        assert!(usable(&new_stopped));
        let kept = newest(all.iter().copied().filter(|placed| usable(*placed)));
        assert_eq!(kept.map(Placed::id), Some(id('c')));
        assert_eq!(
            retirees(all, Some(id('c'))),
            vec![
                Retiree {
                    id: id('a'),
                    running: true
                },
                Retiree {
                    id: id('b'),
                    running: false
                },
                Retiree {
                    id: id('d'),
                    running: true
                },
            ]
        );
        assert_eq!(retirees(all, None).len(), 4);
        assert_eq!(replacing(all, Some(id('d'))), Some(id('b')));
    }
}
