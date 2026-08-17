use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use bollard::query_parameters::EventsOptionsBuilder;
use futures_util::StreamExt;
use ployz_core::{ContainerId, ContainerObservation, LocalMachinePhase};
use tokio_util::sync::CancellationToken;

use super::{ContainerRuntime, Error, LABEL_MANAGED};
use crate::corrosion::{LocalContainerSnapshot, ReplicatedStore};
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
        let Some(sink) = &self.sink else {
            shutdown.cancelled().await;
            return Ok(());
        };
        while !shutdown.is_cancelled() {
            if let Err(error) = self.watch_observations(sink, &shutdown).await {
                eprintln!("local Docker observation failed, retrying: {error}");
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
        let filters = HashMap::from([
            ("type", vec!["container"]),
            ("scope", vec!["local"]),
            ("label", vec![LABEL_MANAGED]),
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
        ]);
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
        self.sync_observations(sink).await?;

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
                _ = rescans.tick() => self.sync_observations(sink).await?,
                () = async {
                    match scan_at {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                } => {
                    scan_at = None;
                    self.sync_observations(sink).await?;
                }
                () = shutdown.cancelled() => return Ok(()),
            }
        }
    }

    async fn sync_observations(&self, sink: &ObservationSink) -> Result<(), Error> {
        // TODO(UT-120): preserve stale rows when Docker cannot provide a complete inventory.
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
}

fn redacted_container(observation: &ContainerObservation) -> ContainerObservation {
    let mut observation = observation.clone();
    observation.resolved_spec.container.environment.clear();
    if let Some(hook) = &mut observation.resolved_spec.pre_deploy {
        hook.environment.clear();
    }
    observation
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ployz_core::{ContainerId, ContainerObservation};
    use serde_json::json;

    use super::{local_container_changes, redacted_container};
    use crate::corrosion::LocalContainerSnapshot;

    #[test]
    fn publication_redacts_service_and_hook_environment_values() {
        let observation: ContainerObservation = serde_json::from_value(json!({
            "container_id": "a".repeat(64),
            "display_name": "api-test",
            "machine_id": "b".repeat(32),
            "service_id": "c".repeat(32),
            "service_name": "api",
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
    fn snapshot_diff_upserts_changes_and_deletes_only_absent_ids() {
        let observation: ContainerObservation = serde_json::from_value(json!({
            "container_id": "a".repeat(64),
            "display_name": "api-test",
            "machine_id": "b".repeat(32),
            "service_id": "c".repeat(32),
            "service_name": "api",
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
        stale.container_id = ContainerId::parse("b".repeat(64)).unwrap();
        let mut old_changed = observation.clone();
        old_changed.container_id = ContainerId::parse("c".repeat(64)).unwrap();
        let mut changed = old_changed.clone();
        changed.display_name = "renamed".into();
        let mut new = observation.clone();
        new.container_id = ContainerId::parse("d".repeat(64)).unwrap();

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
}
