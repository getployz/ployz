//! The choice every reserved placement makes before it touches Docker: which of the
//! Containers it finds is the canonical one, what to do with it, and which others
//! retire. The Ingress doors and the Private Access router place differently but choose
//! the same way, so they share this and keep their own effects.

use ployz_core::{ContainerId, ContainerObservation, ContainerRuntimeObservation};

/// What a placement does with the reserved Container it wants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in super::super) enum Step {
    /// A canonical one runs, or is restarting.
    Keep(ContainerId),
    /// A canonical one exists at rest.
    Start(ContainerId),
    Create(Replacing),
}

/// The newest Container of the same identity a creation replaces. The old one keeps its
/// name until the new one runs, so the new one is named for what it replaces.
pub(in super::super) type Replacing = Option<ContainerId>;

/// A reserved Container the route replaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in super::super) struct Retiree {
    pub(in super::super) id: ContainerId,
    /// It ran when planned, so a replacement that fails starts it again.
    pub(in super::super) running: bool,
}

/// A reserved Container as Docker has it.
pub(in super::super) trait Placed {
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
pub(in super::super) fn usable(placed: &impl Placed) -> bool {
    !matches!(
        placed.runtime(),
        ContainerRuntimeObservation::Removing | ContainerRuntimeObservation::Dead
    )
}

fn running(placed: &impl Placed) -> bool {
    matches!(
        placed.runtime(),
        ContainerRuntimeObservation::Running { .. }
    )
}

/// A running Container before one at rest, then the latest created.
pub(in super::super) fn newest<'placed, P: Placed + 'placed>(
    entries: impl IntoIterator<Item = &'placed P>,
) -> Option<&'placed P> {
    entries
        .into_iter()
        .max_by_key(|placed| (running(*placed), placed.created_at(), placed.id()))
}

/// The newest of `entries` not `kept`. A Container named for replacing it would be
/// newer still, so no Container holds the name a creation that replaces it takes.
pub(in super::super) fn replacing<'placed, P: Placed + 'placed>(
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

pub(in super::super) fn retirees<'placed, P: Placed + 'placed>(
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
pub(in super::super) fn step<'placed, P: Placed + 'placed>(
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
