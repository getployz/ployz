//! What may run once the Deployments in flight end, which a push diffs Saved against.

use super::*;
use crate::review::{Head, InFlight};

/// What may run once every Deployment in flight ends, however each ends: Applied
/// State, and what each in flight puts in place or removes. A node of Saved is
/// settled only when every way they can end leaves it as Saved has it, and Saved
/// drops each node any of them may leave running.
pub(crate) struct Landing<'a> {
    applied: &'a SavedEnvironmentIntent,
    in_flight: &'a [InFlight],
}

impl<'a> Landing<'a> {
    /// Applied State and every Deployment in flight of `head`.
    pub(crate) fn of(head: &'a Head) -> Self {
        Self {
            applied: &head.applied,
            in_flight: &head.in_flight,
        }
    }

    /// Applied State alone, for a Deployment that doesn't look past it.
    pub(crate) fn applied(applied: &'a SavedEnvironmentIntent) -> Self {
        Self {
            applied,
            in_flight: &[],
        }
    }

    /// Every version of a Service that may run.
    pub(crate) fn services(&self) -> impl Iterator<Item = &'a SavedServiceIntent> {
        self.applied
            .services
            .iter()
            .chain(self.in_flight.iter().flat_map(|target| &target.services))
    }

    /// Every version of a Volume that may be in place.
    pub(crate) fn volumes(&self) -> impl Iterator<Item = &'a SavedVolumeIntent> {
        self.applied
            .volumes
            .iter()
            .chain(self.in_flight.iter().flat_map(|target| &target.volumes))
    }

    /// Every version of a Config that may be in place.
    pub(crate) fn configs(&self) -> impl Iterator<Item = &'a SavedConfigIntent> {
        self.applied
            .configs
            .iter()
            .chain(self.in_flight.iter().flat_map(|target| &target.configs))
    }

    pub(crate) fn settled_service(&self, service: &SavedServiceIntent) -> bool {
        self.settled(
            service,
            &self.applied.services,
            |target| &target.services,
            |service| &service.id,
        )
    }

    pub(crate) fn settled_volume(&self, volume: &SavedVolumeIntent) -> bool {
        self.settled(
            volume,
            &self.applied.volumes,
            |target| &target.volumes,
            |volume| &volume.resource_id,
        )
    }

    pub(crate) fn settled_config(&self, config: &SavedConfigIntent) -> bool {
        self.settled(
            config,
            &self.applied.configs,
            |target| &target.configs,
            |config| &config.resource_id,
        )
    }

    /// Whether `node` runs as it is however the Deployments in flight end: Applied
    /// State holds it so, and none in flight removes it or puts another in place.
    fn settled<T: PartialEq>(
        &self,
        node: &T,
        applied: &[T],
        put: impl Fn(&InFlight) -> &[T],
        id: impl Fn(&T) -> &str,
    ) -> bool {
        let node_id = id(node);
        applied.contains(node)
            && self.in_flight.iter().all(|target| {
                !target.removes.contains(node_id)
                    && put(target)
                        .iter()
                        .filter(|other| id(other) == node_id)
                        .all(|other| other == node)
            })
    }

    /// What `saved` drops: each Service, Volume and Config that may run and `saved`
    /// lacks, once each. Secrets ship inside the Services, so they go with them.
    pub(crate) fn dropped(&self, saved: &SavedEnvironmentIntent) -> SavedEnvironmentIntent {
        fn gone<'a, T: Clone + 'a>(
            held: impl Iterator<Item = &'a T>,
            kept: &[T],
            id: impl Fn(&T) -> &str,
        ) -> Vec<T> {
            let mut gone: Vec<T> = Vec::new();
            for item in held {
                let same = |other: &T| id(other) == id(item);
                if !kept.iter().any(same) && !gone.iter().any(same) {
                    gone.push(item.clone());
                }
            }
            gone
        }
        SavedEnvironmentIntent {
            version: self.applied.version,
            environment_slug: self.applied.environment_slug.clone(),
            services: gone(self.services(), &saved.services, |service| &service.id),
            volumes: gone(self.volumes(), &saved.volumes, |volume| &volume.resource_id),
            configs: gone(self.configs(), &saved.configs, |config| &config.resource_id),
        }
    }
}
