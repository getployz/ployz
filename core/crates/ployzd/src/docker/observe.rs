use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
    time::{Duration, SystemTime},
};

use bollard::query_parameters::EventsOptionsBuilder;
use futures_util::StreamExt;
use ployz_core::{
    CADDY_ADMIN_ENV, ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation,
    CopyRole, DockerVolume, DockerVolumeName, DockerVolumeStorageObservation, LocalMachinePhase,
    MIRROR_ROOT, VolumeInventory,
};
use tokio_util::sync::CancellationToken;

use super::{ContainerRuntime, Error, LABEL_MANAGED, LABEL_NAMESPACE};
use crate::corrosion::{LocalContainerSnapshot, LocalVolumeSnapshot, ReplicatedStore};
use crate::machine::RecordOwner;

const RESCAN_INTERVAL: Duration = Duration::from_secs(30);
const EVENT_DEBOUNCE: Duration = Duration::from_millis(100);

#[derive(Default)]
pub(super) struct StrandedIngress {
    /// Stopped on request, so never started unasked.
    pub(super) held: BTreeSet<ContainerId>,
    /// The last one whose failure was logged. Docker names a fresh endpoint in
    /// every failure, so comparing the messages would log every scan.
    pub(super) reported: Option<ContainerId>,
    /// Taken through each start, so a stop that races it lands after it and holds,
    /// while other containers never wait on the Docker calls.
    pub(super) gates: BTreeMap<ContainerId, Arc<tokio::sync::Mutex<()>>>,
}

#[derive(Clone)]
pub(super) struct ObservationSink {
    replicated: ReplicatedStore,
    local: RecordOwner,
    plugin: crate::storage::Plugin,
    rescan_interval: Duration,
}

impl ContainerRuntime {
    #[must_use]
    pub(crate) fn replicating(
        mut self,
        replicated: ReplicatedStore,
        local: RecordOwner,
        plugin: crate::storage::Plugin,
    ) -> Self {
        self.sink = Some(ObservationSink {
            replicated,
            local,
            plugin,
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
                eprintln!(
                    "{retry}: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
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
                ("label", vec![LABEL_MANAGED, LABEL_NAMESPACE]),
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
            true,
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
            false,
        )
        .await
    }

    async fn watch_docker_events<F>(
        &self,
        sink: &ObservationSink,
        shutdown: &CancellationToken,
        filters: HashMap<&str, Vec<&str>>,
        sync: F,
        on_stopping_marks: bool,
    ) -> Result<(), Error>
    where
        F: AsyncFn(&Self, &ObservationSink) -> Result<(), Error>,
    {
        let since = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|error| Error::Clock(ployz_core::error_chain::inline(&error)))?
            .as_secs()
            .to_string();
        let options = EventsOptionsBuilder::default()
            .since(&since)
            .filters(&filters)
            .build();
        // Bollard opens this lazy stream when first polled. The cursor replays any event
        // between capturing `since` and completing the initial snapshot.
        let mut events = Box::pin(self.docker.client.events(Some(options)));
        let mut stopping = self.stopping.subscribe();
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
                // A stopping mark changes no Docker state, so no event announces it.
                Ok(()) = stopping.changed(), if on_stopping_marks => sync(self, sink).await?,
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
        let machine_id = sink.local.record().id();
        let creates = self.docker.creates();
        let inventory = self.docker.managed_container_ids().await?;
        let mut live = LocalContainerSnapshot::from_inventory(inventory);
        let container_ids = live.ids().cloned().collect::<Vec<_>>();
        let mut observed = Vec::with_capacity(container_ids.len());
        for container_id in &container_ids {
            match self.inspect_managed(container_id, &machine_id).await {
                Ok(observation) => {
                    live.observed(redacted_container(&observation));
                    observed.push(observation);
                }
                Err(error) => {
                    eprintln!(
                        "failed to inspect managed container {container_id}: {error}",
                        error = ployz_core::error_chain::inline(&error),
                    )
                }
            }
        }
        let publication = sink.replicated.machine_publication().await;
        let local = sink.local.record();
        if local.phase() != LocalMachinePhase::Participating || local.id() != machine_id {
            return Ok(());
        }
        let existing = publication.local_containers(&machine_id).await?;
        let changes = local_container_changes(&existing, &live);
        publication
            .apply_container_rows(&machine_id, &changes.deletions, &changes.upserts)
            .await?;
        // Started after the guard drops, so a slow Docker start never stalls publication.
        // A reset that takes the guard meanwhile removes the container anyway. The record
        // is read again here, so a policy change that committed during publication holds.
        drop(publication);
        let local = sink.local.record();
        if local.phase() == LocalMachinePhase::Participating
            && local
                .machine()
                .is_some_and(|machine| machine.accepts_ingress)
            && let Some(container_id) =
                stranded_ingress(&observed, observed.len() == container_ids.len())
        {
            // Off the event loop, so a slow Docker start never delays later scans.
            let runtime = self.clone();
            tokio::spawn(async move { runtime.start_stranded(&container_id, creates).await });
        }
        Ok(())
    }

    async fn sync_volume_observations(&self, sink: &ObservationSink) -> Result<(), Error> {
        let machine_id = sink.local.record().id();
        let VolumeInventory {
            mut volumes,
            failures,
        } = self.list_volumes(&machine_id).await?;
        match sink
            .plugin
            .call::<ployz_core::StorageCapacity>("Storage.Inspect", &())
            .await
        {
            Ok(capacity) => observe_copies(&machine_id, &mut volumes, &capacity.copies),
            Err(error) => {
                eprintln!("Volume copies unobserved, Docker Volumes stay writers: {error}")
            }
        }
        let mut live = LocalVolumeSnapshot::from_inventory(
            failures.into_iter().map(|failure| failure.id.name),
        );
        for volume in volumes {
            live.observed(volume);
        }
        let publication = sink.replicated.machine_publication().await;
        let local = sink.local.record();
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

/// Every Docker Volume the plugin holds carries its role, and every slot, which Docker
/// never lists, joins the observation as a read-only copy under the mirror root.
fn observe_copies(
    machine_id: &ployz_core::MachineId,
    volumes: &mut Vec<DockerVolume>,
    copies: &std::collections::BTreeMap<DockerVolumeName, ployz_core::ProvisionedCopy>,
) {
    for volume in volumes.iter_mut() {
        if let DockerVolumeStorageObservation::Provisioned { role, .. } = &mut volume.storage {
            *role = copies.get(&volume.id.name).map(|copy| copy.role);
        }
    }
    for (name, copy) in copies {
        if copy.role != CopyRole::Slot || volumes.iter().any(|volume| volume.id.name == *name) {
            continue;
        }
        let (Ok(mountpoint), Some(bound_bytes)) = (
            ployz_core::MachinePath::parse(format!("/var/lib/{MIRROR_ROOT}/{name}/fs")),
            std::num::NonZeroU64::new(copy.maximum_bytes.get()),
        ) else {
            continue;
        };
        volumes.push(DockerVolume {
            id: ployz_core::DockerVolumeId {
                machine_id: *machine_id,
                name: name.clone(),
            },
            options: Default::default(),
            labels: Default::default(),
            storage: DockerVolumeStorageObservation::Provisioned {
                mountpoint,
                bound_bytes,
                used_bytes: copy.used_bytes,
                role: Some(CopyRole::Slot),
            },
        });
    }
}

fn redacted_container(observation: &ContainerObservation) -> ContainerObservation {
    let mut parts = observation.clone().into_parts();
    parts
        .resolved_spec
        .container
        .environment
        .retain(|key, _| key == CADDY_ADMIN_ENV);
    if let Some(hook) = &mut parts.resolved_spec.pre_deploy {
        hook.environment.clear();
    }
    parts.resolved_spec.mount_graph.redact_config_content();
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

/// The newest local Ingress Proxy, when no local one runs.
///
/// Docker's restart policy covers an ingress that ran and died, not one whose start
/// failed, such as when another process held port 80 at enrollment or at boot. The
/// candidate's inspect decides whether its last start failed. A scan that could not
/// inspect every container may have missed a running one, so it names nothing.
fn stranded_ingress(observations: &[ContainerObservation], complete: bool) -> Option<ContainerId> {
    if !complete {
        return None;
    }
    let ingresses = observations.iter().filter(|observation| {
        observation.kind == ContainerKind::ServiceContainer
            && crate::ingress::is_system_ingress(observation)
    });
    let at_rest = ingresses.clone().all(|observation| {
        matches!(
            observation.runtime,
            ContainerRuntimeObservation::Created
                | ContainerRuntimeObservation::Exited { .. }
                | ContainerRuntimeObservation::Dead
        )
    });
    let newest = ingresses
        .max_by_key(|observation| (observation.created_at_unix_nanos, observation.container_id))?;
    at_rest.then_some(newest.container_id)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ployz_core::{ContainerId, ContainerObservation, CopyRole, DockerVolume, DockerVolumeName};
    use serde_json::json;

    use super::{
        local_container_changes, local_volume_changes, observe_copies, redacted_container,
        stranded_ingress,
    };
    use crate::corrosion::{LocalContainerSnapshot, LocalVolumeSnapshot};

    fn container(
        id: char,
        namespace: &str,
        name: &str,
        created_at: i64,
        runtime: serde_json::Value,
    ) -> ContainerObservation {
        serde_json::from_value(json!({
            "container_id": id.to_string().repeat(64),
            "display_name": format!("{name}-{id}"),
            "created_at_unix_nanos": created_at,
            "machine_id": "b".repeat(32),
            "namespace": namespace,
            "kind": "service_container",
            "runtime": runtime,
            "resolved_spec": {
                "service_id": "c".repeat(32),
                "name": name,
                "mode": { "mode": "replicated", "replicas": 1 },
                "container": { "image": "caddy:2", "pull_policy": "missing" }
            }
        }))
        .unwrap()
    }

    fn ingress(id: char, created_at: i64, runtime: serde_json::Value) -> ContainerObservation {
        container(id, "ployz-system", "ingress", created_at, runtime)
    }

    fn id(id: char) -> ContainerId {
        ContainerId::parse(id.to_string().repeat(64)).unwrap()
    }

    #[test]
    fn ingress_that_never_started_is_started_again() {
        let created = json!({ "state": "created" });
        assert_eq!(
            stranded_ingress(&[ingress('a', 1, created)], true),
            Some(id('a'))
        );
    }

    #[test]
    fn newest_ingress_at_rest_is_the_candidate_when_none_runs() {
        assert_eq!(
            stranded_ingress(
                &[
                    ingress('a', 1, json!({ "state": "exited", "code": 0 })),
                    ingress('d', 2, json!({ "state": "created" })),
                ],
                true
            ),
            Some(id('d'))
        );
        assert_eq!(
            stranded_ingress(
                &[
                    ingress('a', 2, json!({ "state": "exited", "code": 255 })),
                    ingress('d', 1, json!({ "state": "created" })),
                ],
                true
            ),
            Some(id('a'))
        );
    }

    #[test]
    fn ingress_is_left_alone_while_one_runs_or_belongs_to_an_app() {
        let created = json!({ "state": "created" });
        let running = json!({ "state": "running", "health": "healthy" });
        assert_eq!(
            stranded_ingress(
                &[ingress('a', 1, running), ingress('d', 2, created.clone())],
                true
            ),
            None
        );
        assert_eq!(
            stranded_ingress(
                &[
                    ingress('a', 2, json!({ "state": "restarting" })),
                    ingress('d', 1, created.clone()),
                ],
                true
            ),
            None
        );
        assert_eq!(
            stranded_ingress(
                &[container('a', "app", "ingress", 1, created.clone())],
                true
            ),
            None
        );
        assert_eq!(stranded_ingress(&[], true), None);
    }

    #[test]
    fn a_scan_that_could_not_inspect_every_container_starts_no_ingress() {
        let created = json!({ "state": "created" });
        assert_eq!(stranded_ingress(&[ingress('a', 1, created)], false), None);
    }

    #[test]
    fn publication_redacts_service_and_hook_environment_values() {
        let observation: ContainerObservation = serde_json::from_value(json!({
            "container_id": "a".repeat(64),
            "display_name": "api-test",
            "machine_id": "b".repeat(32),
            "namespace": "app",
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
    fn publication_redacts_config_content() {
        let observation: ContainerObservation = serde_json::from_value(json!({
            "container_id": "a".repeat(64),
            "display_name": "api-test",
            "machine_id": "b".repeat(32),
            "namespace": "app",
            "kind": "service_container",
            "runtime": { "state": "created" },
            "resolved_spec": {
                "service_id": "c".repeat(32),
                "name": "api",
                "mode": { "mode": "replicated", "replicas": 1 },
                "container": {
                    "image": "alpine:3.23.3",
                    "pull_policy": "missing",
                    "config_mounts": [{ "config_name": "settings", "target": "/etc/settings.conf" }]
                },
                "configs": [{ "name": "settings", "content": b"token=config-secret".to_vec() }]
            }
        }))
        .unwrap();

        let redacted = redacted_container(&observation);

        assert_eq!(
            redacted.resolved_spec.configs(),
            [ployz_core::ConfigSpec {
                name: "settings".into(),
                content: Vec::new(),
            }]
        );
        assert_eq!(
            redacted.resolved_spec.config_mounts(),
            observation.resolved_spec.config_mounts()
        );
        assert_eq!(
            serde_json::to_value(&redacted)
                .unwrap()
                .pointer("/resolved_spec/configs"),
            Some(&json!([{ "name": "settings", "content": [] }]))
        );
        assert_eq!(
            observation.resolved_spec.configs(),
            [ployz_core::ConfigSpec {
                name: "settings".into(),
                content: b"token=config-secret".to_vec(),
            }]
        );
    }

    #[test]
    fn publication_keeps_caddy_admin_so_redacted_caddy_stays_valid() {
        let mut spec = ployz_core::caddy_service_spec("caddy:test".into(), Default::default())
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
                namespace: ployz_core::Namespace::system(),
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
        assert!(ployz_core::validate_ingress_service_spec(&redacted.resolved_spec).is_ok());
    }

    #[test]
    fn snapshot_diff_upserts_changes_and_deletes_only_absent_ids() {
        let observation: ContainerObservation = serde_json::from_value(json!({
            "container_id": "a".repeat(64),
            "display_name": "api-test",
            "machine_id": "b".repeat(32),
            "namespace": "app",
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
                role: None,
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

    #[test]
    fn copies_give_docker_volumes_their_role_and_add_each_unlisted_slot() {
        let machine_id = ployz_core::MachineId::parse("b".repeat(32)).unwrap();
        let name = |name: &str| DockerVolumeName::parse(name).unwrap();
        let provisioned = |volume: &str, role| DockerVolume {
            id: ployz_core::DockerVolumeId {
                machine_id,
                name: name(volume),
            },
            options: BTreeMap::new(),
            labels: BTreeMap::new(),
            storage: ployz_core::DockerVolumeStorageObservation::Provisioned {
                mountpoint: ployz_core::MachinePath::parse(format!(
                    "/var/lib/ployz-volumes/{volume}"
                ))
                .unwrap(),
                bound_bytes: std::num::NonZeroU64::new(1024).unwrap(),
                used_bytes: 512,
                role,
            },
        };
        let plain = DockerVolume {
            id: ployz_core::DockerVolumeId {
                machine_id,
                name: name("plain"),
            },
            options: BTreeMap::new(),
            labels: BTreeMap::new(),
            storage: ployz_core::DockerVolumeStorageObservation::Plain {
                driver: "local".into(),
            },
        };
        let copy = |role, used_bytes| ployz_core::ProvisionedCopy {
            role,
            maximum_bytes: ployz_core::ProvisionedVolumeMaximumBytes::new(
                std::num::NonZeroU64::new(1024).unwrap(),
            ),
            used_bytes,
        };
        let copies = BTreeMap::from([
            (name("data"), copy(CopyRole::Switching, 512)),
            (name("mirror"), copy(CopyRole::Slot, 300)),
        ]);
        let mut volumes = vec![provisioned("data", None), plain.clone()];

        observe_copies(&machine_id, &mut volumes, &copies);

        let mut slot = provisioned("mirror", Some(CopyRole::Slot));
        slot.storage = ployz_core::DockerVolumeStorageObservation::Provisioned {
            mountpoint: ployz_core::MachinePath::parse("/var/lib/ployz-mirror/mirror/fs").unwrap(),
            bound_bytes: std::num::NonZeroU64::new(1024).unwrap(),
            used_bytes: 300,
            role: Some(CopyRole::Slot),
        };
        assert_eq!(
            volumes,
            vec![provisioned("data", Some(CopyRole::Switching)), plain, slot]
        );

        observe_copies(&machine_id, &mut volumes, &copies);
        assert_eq!(volumes.len(), 3, "a listed slot is not added twice");
    }
}
