use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use bollard::query_parameters::EventsOptionsBuilder;
use futures_util::StreamExt;
use ployz_core::{
    ContainerId, ContainerObservation, DockerVolume, DockerVolumeName, LocalMachinePhase,
    VolumeInventory,
};
use tokio_util::sync::CancellationToken;

use super::{ContainerRuntime, Error, LABEL_MANAGED, LABEL_PROJECT_NAME};
use crate::corrosion::{LocalContainerSnapshot, LocalVolumeSnapshot, ReplicatedStore};
use crate::machine::LocalMachineStore;

const RESCAN_INTERVAL: Duration = Duration::from_secs(30);
const EVENT_DEBOUNCE: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub(super) struct ObservationSink {
    replicated: ReplicatedStore,
    local: Arc<Mutex<LocalMachineStore>>,
    rescan_interval: Duration,
}

impl ContainerRuntime {
    #[must_use]
    pub fn replicating(
        mut self,
        replicated: ReplicatedStore,
        local: Arc<Mutex<LocalMachineStore>>,
    ) -> Self {
        self.sink = Some(ObservationSink {
            replicated,
            local,
            rescan_interval: RESCAN_INTERVAL,
        });
        self
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_rescan_interval(mut self, interval: Duration) -> Self {
        if let Some(sink) = &mut self.sink {
            sink.rescan_interval = interval;
        }
        self
    }

    pub async fn publish_observations(&self, shutdown: CancellationToken) -> Result<(), Error> {
        let Some(sink) = self.sink.clone() else {
            shutdown.cancelled().await;
            return Ok(());
        };
        tokio::try_join!(
            self.retry_watch(
                &sink,
                shutdown.clone(),
                "local Docker observation failed, retrying",
                Self::watch_observations,
            ),
            self.retry_watch(
                &sink,
                shutdown,
                "local Docker Volume observation failed, retrying",
                Self::watch_volume_observations,
            ),
        )?;
        Ok(())
    }

    async fn retry_watch<F>(
        &self,
        sink: &ObservationSink,
        shutdown: CancellationToken,
        retry: &str,
        watch: F,
    ) -> Result<(), Error>
    where
        F: AsyncFn(&Self, &ObservationSink, &CancellationToken) -> Result<(), Error>,
    {
        while !shutdown.is_cancelled() {
            if let Err(error) = watch(self, sink, &shutdown).await {
                eprintln!("{retry}: {error}");
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(1)) => {}
                    () = shutdown.cancelled() => {}
                }
            }
        }
        Ok(())
    }

    async fn watch_observations(
        &self,
        sink: &ObservationSink,
        shutdown: &CancellationToken,
    ) -> Result<(), Error> {
        self.watch_docker_events(
            sink,
            shutdown,
            HashMap::from([
                ("type", vec!["container"]),
                ("scope", vec!["local"]),
                ("label", vec![LABEL_MANAGED, LABEL_PROJECT_NAME]),
                (
                    "event",
                    vec![
                        "create",
                        "start",
                        "stop",
                        "pause",
                        "unpause",
                        "kill",
                        "die",
                        "oom",
                        "destroy",
                        "health_status",
                    ],
                ),
            ]),
            Self::sync_observations,
        )
        .await
    }

    async fn watch_volume_observations(
        &self,
        sink: &ObservationSink,
        shutdown: &CancellationToken,
    ) -> Result<(), Error> {
        self.watch_docker_events(
            sink,
            shutdown,
            HashMap::from([
                ("type", vec!["volume"]),
                ("event", vec!["create", "destroy"]),
            ]),
            Self::sync_volume_observations,
        )
        .await
    }

    async fn watch_docker_events<F>(
        &self,
        sink: &ObservationSink,
        shutdown: &CancellationToken,
        filters: HashMap<&str, Vec<&str>>,
        sync: F,
    ) -> Result<(), Error>
    where
        F: AsyncFn(&Self, &ObservationSink) -> Result<(), Error>,
    {
        let since = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|error| Error::Clock(error.to_string()))?
            .as_secs()
            .to_string();
        let options = EventsOptionsBuilder::default()
            .since(&since)
            .filters(&filters)
            .build();
        // Bollard opens this lazy stream when first polled. The cursor replays any event
        // between capturing `since` and completing the initial snapshot.
        let mut events = Box::pin(self.docker.client.events(Some(options)));
        sync(self, sink).await?;

        let mut rescans = tokio::time::interval(sink.rescan_interval);
        rescans.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        rescans.tick().await;
        let mut scan_at = None;
        loop {
            tokio::select! {
                event = events.next() => match event {
                    Some(Ok(_)) => {
                        scan_at = Some(tokio::time::Instant::now() + EVENT_DEBOUNCE);
                    }
                    Some(Err(error)) => return Err(error.into()),
                    None => return Err(Error::EventStreamClosed),
                },
                _ = rescans.tick() => sync(self, sink).await?,
                () = async {
                    match scan_at {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                } => {
                    scan_at = None;
                    sync(self, sink).await?;
                }
                () = shutdown.cancelled() => return Ok(()),
            }
        }
    }

    async fn sync_observations(&self, sink: &ObservationSink) -> Result<(), Error> {
        // TODO: preserve stale rows when Docker cannot provide a complete inventory.
        let machine_id = sink
            .local
            .lock()
            .map_err(|_| Error::LocalStorePoisoned)?
            .record()
            .id();
        let inventory = self.docker.managed_container_ids().await?;
        let mut live = LocalContainerSnapshot::from_inventory(inventory);
        let container_ids = live.ids().cloned().collect::<Vec<_>>();
        for container_id in &container_ids {
            match self.inspect_managed(container_id, &machine_id).await {
                Ok(observation) => {
                    live.observed(redacted_container(&observation));
                }
                Err(error) => {
                    eprintln!("failed to inspect managed container {container_id}: {error}")
                }
            }
        }
        let publication = sink.replicated.machine_publication().await;
        let local = sink
            .local
            .lock()
            .map_err(|_| Error::LocalStorePoisoned)?
            .record()
            .clone();
        if local.phase() != LocalMachinePhase::Participating || local.id() != machine_id {
            return Ok(());
        }
        let existing = publication.local_containers(&machine_id).await?;
        let changes = local_container_changes(&existing, &live);
        publication
            .apply_container_rows(&machine_id, &changes.deletions, &changes.upserts)
            .await
            .map_err(Error::from)
    }

    async fn sync_volume_observations(&self, sink: &ObservationSink) -> Result<(), Error> {
        let machine_id = sink
            .local
            .lock()
            .map_err(|_| Error::LocalStorePoisoned)?
            .record()
            .id();
        let VolumeInventory { volumes, failures } = self.list_volumes(&machine_id).await?;
        let mut live = LocalVolumeSnapshot::from_inventory(
            failures.into_iter().map(|failure| failure.id.name),
        );
        for volume in volumes {
            live.observed(volume);
        }
        let publication = sink.replicated.machine_publication().await;
        let local = sink
            .local
            .lock()
            .map_err(|_| Error::LocalStorePoisoned)?
            .record()
            .clone();
        if local.phase() != LocalMachinePhase::Participating || local.id() != machine_id {
            return Ok(());
        }
        let existing = publication.local_volumes(&machine_id).await?;
        let changes = local_volume_changes(&existing, &live);
        publication
            .apply_volume_rows(
                &machine_id,
                &changes.deletions,
                &changes.upserts,
                &changes.incomplete,
            )
            .await
            .map_err(Error::from)
    }
}

fn redacted_container(observation: &ContainerObservation) -> ContainerObservation {
    let mut parts = observation.clone().into_parts();
    let keep = ployz_core::ingress_proxy_profile_environment_keys();
    parts
        .resolved_spec
        .container
        .environment
        .retain(|key, _| keep.contains(key));
    if let Some(hook) = &mut parts.resolved_spec.pre_deploy {
        hook.environment.clear();
    }
    ContainerObservation::try_from(parts)
        .expect("environment redaction preserves Container identity")
}

struct LocalContainerChanges {
    deletions: Vec<ContainerId>,
    upserts: Vec<ContainerObservation>,
}

fn local_container_changes(
    existing: &LocalContainerSnapshot,
    current: &LocalContainerSnapshot,
) -> LocalContainerChanges {
    LocalContainerChanges {
        deletions: existing
            .inventory
            .iter()
            .filter(|id| !current.inventory.contains(id))
            .cloned()
            .collect(),
        upserts: current
            .observations
            .values()
            .filter(|observation| {
                existing.observations.get(&observation.container_id) != Some(observation)
            })
            .cloned()
            .collect(),
    }
}

struct LocalVolumeChanges {
    deletions: Vec<DockerVolumeName>,
    upserts: Vec<DockerVolume>,
    incomplete: Vec<DockerVolumeName>,
}

fn local_volume_changes(
    existing: &LocalVolumeSnapshot,
    current: &LocalVolumeSnapshot,
) -> LocalVolumeChanges {
    LocalVolumeChanges {
        deletions: existing
            .iter()
            .filter(|(name, _)| current.get(name).is_none())
            .map(|(name, _)| name)
            .cloned()
            .collect(),
        upserts: current
            .iter()
            .filter_map(|(_, volume)| volume)
            .filter(|volume| existing.get(&volume.id.name).flatten() != Some(volume))
            .cloned()
            .collect(),
        incomplete: current
            .iter()
            .filter(|(name, volume)| volume.is_none() && existing.get(name) != Some(None))
            .map(|(name, _)| name)
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ployz_core::{ContainerId, ContainerObservation, DockerVolume, DockerVolumeName};
    use serde_json::json;

    use super::{local_container_changes, local_volume_changes, redacted_container};
    use crate::corrosion::{LocalContainerSnapshot, LocalVolumeSnapshot};

    #[test]
    fn publication_redacts_service_and_hook_environment_values() {
        let observation: ContainerObservation = serde_json::from_value(json!({
            "container_id": "a".repeat(64),
            "display_name": "api-test",
            "machine_id": "b".repeat(32),
            "project_name": "app",
            "kind": "service_container",
            "runtime": { "state": "created" },
            "resolved_spec": {
                "service_id": "c".repeat(32),
                "name": "api",
                "mode": { "mode": "replicated", "replicas": 1 },
                "container": {
                    "image": "alpine:3.23.3",
                    "environment": { "TOKEN": "service-secret" },
                    "pull_policy": "missing"
                },
                "pre_deploy": {
                    "command": ["true"],
                    "environment": { "TOKEN": "hook-secret" }
                }
            }
        }))
        .unwrap();

        let redacted = redacted_container(&observation);
        assert!(redacted.resolved_spec.container.environment.is_empty());
        assert!(
            redacted
                .resolved_spec
                .pre_deploy
                .as_ref()
                .unwrap()
                .environment
                .is_empty()
        );
        assert_eq!(
            observation
                .resolved_spec
                .container
                .environment
                .get("TOKEN")
                .map(String::as_str),
            Some("service-secret")
        );
    }

    #[test]
    fn publication_keeps_caddy_admin_so_redacted_caddy_still_identifies() {
        let mut spec = ployz_core::IngressProxyBackend::Caddy
            .requested_service_spec("caddy:test".into(), Vec::new(), None)
            .unwrap()
            .to_resolved(
                ployz_core::ServiceId::parse("c".repeat(32)).unwrap(),
                ployz_core::ResolvedUpdateConfig::default(),
            )
            .expect("volume graph is scoped");
        spec.container
            .environment
            .insert("TOKEN".into(), "service-secret".into());
        let observation =
            ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
                container_id: ContainerId::parse("a".repeat(64)).unwrap(),
                display_name: "ingress-test".into(),
                created_at_unix_nanos: 1,
                machine_id: ployz_core::MachineId::parse("b".repeat(32)).unwrap(),
                project_name: ployz_core::ProjectName::system(),
                kind: ployz_core::ContainerKind::ServiceContainer,
                runtime: ployz_core::ContainerRuntimeObservation::Created,
                effective_healthcheck: None,
                resolved_spec: spec,
                address: None,
                labels: Default::default(),
            })
            .unwrap();

        let redacted = redacted_container(&observation);

        assert_eq!(
            redacted
                .resolved_spec
                .container
                .environment
                .get("CADDY_ADMIN")
                .map(String::as_str),
            Some("unix//run/ingress/caddy/admin.sock")
        );
        assert!(
            !redacted
                .resolved_spec
                .container
                .environment
                .contains_key("TOKEN")
        );
        assert_eq!(
            ployz_core::ingress_proxy_backend(&redacted.resolved_spec).unwrap(),
            ployz_core::IngressProxyBackend::Caddy
        );
    }

    #[test]
    fn snapshot_diff_upserts_changes_and_deletes_only_absent_ids() {
        let observation: ContainerObservation = serde_json::from_value(json!({
            "container_id": "a".repeat(64),
            "display_name": "api-test",
            "machine_id": "b".repeat(32),
            "project_name": "app",
            "kind": "service_container",
            "runtime": { "state": "created" },
            "resolved_spec": {
                "service_id": "c".repeat(32),
                "name": "api",
                "mode": { "mode": "replicated", "replicas": 1 },
                "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
            }
        }))
        .unwrap();
        let stable = observation.clone();
        let mut stale = observation.clone();
        stale
            .try_update(|parts| parts.container_id = ContainerId::parse("b".repeat(64)).unwrap())
            .unwrap();
        let mut old_changed = observation.clone();
        old_changed
            .try_update(|parts| parts.container_id = ContainerId::parse("c".repeat(64)).unwrap())
            .unwrap();
        let mut changed = old_changed.clone();
        changed
            .try_update(|parts| parts.display_name = "renamed".into())
            .unwrap();
        let mut new = observation.clone();
        new.try_update(|parts| parts.container_id = ContainerId::parse("d".repeat(64)).unwrap())
            .unwrap();

        let existing = LocalContainerSnapshot {
            inventory: [stable.clone(), stale.clone(), old_changed.clone()]
                .into_iter()
                .map(|item| item.container_id)
                .collect(),
            observations: [stable.clone(), stale.clone(), old_changed]
                .into_iter()
                .map(|item| (item.container_id, item))
                .collect::<BTreeMap<_, _>>(),
        };
        let current = LocalContainerSnapshot {
            inventory: [stable.clone(), changed.clone(), new.clone()]
                .into_iter()
                .map(|item| item.container_id)
                .collect(),
            observations: [stable, changed.clone(), new.clone()]
                .into_iter()
                .map(|item| (item.container_id, item))
                .collect::<BTreeMap<_, _>>(),
        };
        let changes = local_container_changes(&existing, &current);

        assert_eq!(changes.deletions, vec![stale.container_id]);
        assert_eq!(changes.upserts, vec![changed, new]);
    }

    #[test]
    fn volume_snapshot_diff_upserts_changes_and_deletes_only_absent_names() {
        let machine_id = ployz_core::MachineId::parse("b".repeat(32)).unwrap();
        let volume = |name: &str, driver: &str| DockerVolume {
            id: ployz_core::DockerVolumeId {
                machine_id,
                name: DockerVolumeName::parse(name).unwrap(),
            },
            options: BTreeMap::new(),
            labels: BTreeMap::new(),
            storage: ployz_core::DockerVolumeStorageObservation::Plain {
                driver: driver.into(),
            },
        };
        let stable = volume("a-stable", "local");
        let stale = volume("b-stale", "local");
        let old_changed = volume("c-changed", "local");
        let changed = volume("c-changed", "custom");
        let new = volume("d-new", "local");

        let mut existing = LocalVolumeSnapshot::default();
        for volume in [stable.clone(), stale.clone(), old_changed] {
            existing.observed(volume);
        }
        let mut current = LocalVolumeSnapshot::default();
        for volume in [stable, changed.clone(), new.clone()] {
            current.observed(volume);
        }
        let changes = local_volume_changes(&existing, &current);

        assert_eq!(changes.deletions, vec![stale.id.name]);
        assert_eq!(changes.upserts, vec![changed, new]);
    }

    #[test]
    fn unavailable_volume_becomes_incomplete_instead_of_deleted_or_stale() {
        let machine_id = ployz_core::MachineId::parse("b".repeat(32)).unwrap();
        let name = DockerVolumeName::parse("data").unwrap();
        let observed = DockerVolume {
            id: ployz_core::DockerVolumeId {
                machine_id,
                name: name.clone(),
            },
            options: BTreeMap::new(),
            labels: BTreeMap::new(),
            storage: ployz_core::DockerVolumeStorageObservation::Provisioned {
                mountpoint: ployz_core::MachinePath::parse("/var/lib/ployz-volumes/data").unwrap(),
                bound_bytes: std::num::NonZeroU64::new(1024).unwrap(),
                used_bytes: 512,
            },
        };
        let mut existing = LocalVolumeSnapshot::default();
        existing.observed(observed);
        let current = LocalVolumeSnapshot::from_inventory([name.clone()]);

        let changes = local_volume_changes(&existing, &current);

        assert!(changes.deletions.is_empty());
        assert!(changes.upserts.is_empty());
        assert_eq!(changes.incomplete, vec![name]);
    }
}
