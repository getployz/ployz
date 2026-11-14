use ployz_core::labels;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
    time::{Duration, SystemTime},
};

use bollard::query_parameters::EventsOptionsBuilder;
use futures_util::StreamExt;
use ployz_core::{
    CADDY_ADMIN_ENV, ContainerId, ContainerKind, ContainerObservation, CopyPlace, DockerVolume,
    DockerVolumeName, DockerVolumeStorageObservation, LocalMachinePhase, ResolvedServiceSpec,
    VolumeInventory,
};
use tokio_util::sync::CancellationToken;

use super::{
    ContainerRuntime, Error,
    doors::{Door, DoorFacts, door_facts},
};
use crate::corrosion::{LocalContainerSnapshot, LocalVolumeSnapshot, ReplicatedStore};
use crate::machine::RecordOwner;

const RESCAN_INTERVAL: Duration = Duration::from_secs(30);
const EVENT_DEBOUNCE: Duration = Duration::from_millis(100);

#[derive(Default)]
pub(super) struct StrandedIngress {
    /// Stopped on request, so never started unasked. Held in daemon memory, so a
    /// daemon restart forgets it.
    pub(super) held: Holds,
    /// The last one whose failure was logged. Docker names a fresh endpoint in
    /// every failure, so comparing the messages would log every scan.
    pub(super) reported: Option<ContainerId>,
    /// Taken through each start, so a stop that races it lands after it and holds,
    /// while other containers never wait on the Docker calls.
    pub(super) gates: BTreeMap<ContainerId, Arc<tokio::sync::Mutex<()>>>,
}

/// Sync inside, so a [`Held`] dropped mid-await still lifts its hold.
#[derive(Clone, Default)]
pub(super) struct Holds(Arc<std::sync::Mutex<HoldTable>>);

#[derive(Default)]
struct HoldTable {
    stopped: BTreeSet<ContainerId>,
    /// Stops still deciding whether to proceed, one ticket each, so a hold that
    /// `forget` voided can never lift or keep a later stop's hold.
    deciding: BTreeMap<ContainerId, BTreeSet<u64>>,
    next_ticket: u64,
}

impl Holds {
    pub(super) fn hold(&self, container_id: ContainerId) -> Held {
        let mut table = self.table();
        let ticket = table.next_ticket;
        table.next_ticket += 1;
        table
            .deciding
            .entry(container_id)
            .or_default()
            .insert(ticket);
        Held {
            holds: self.clone(),
            container_id,
            ticket,
        }
    }

    pub(super) fn contains(&self, container_id: &ContainerId) -> bool {
        let table = self.table();
        table.stopped.contains(container_id) || table.deciding.contains_key(container_id)
    }

    pub(super) fn release(&self, container_id: &ContainerId) {
        self.table().stopped.remove(container_id);
    }

    pub(super) fn forget(&self, container_id: &ContainerId) {
        let mut table = self.table();
        table.stopped.remove(container_id);
        table.deciding.remove(container_id);
    }

    fn table(&self) -> std::sync::MutexGuard<'_, HoldTable> {
        self.0.lock().expect("stranded holds are never poisoned")
    }
}

/// One stop's hold while it decides whether to proceed.
#[must_use = "the hold lifts when it drops unless kept"]
pub(super) struct Held {
    holds: Holds,
    container_id: ContainerId,
    ticket: u64,
}

impl Held {
    pub(super) fn keep(self) {
        let mut table = self.holds.table();
        if table
            .deciding
            .get(&self.container_id)
            .is_some_and(|tickets| tickets.contains(&self.ticket))
        {
            table.stopped.insert(self.container_id);
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut table = self.holds.table();
        let Some(tickets) = table.deciding.get_mut(&self.container_id) else {
            return;
        };
        tickets.remove(&self.ticket);
        if tickets.is_empty() {
            table.deciding.remove(&self.container_id);
        }
    }
}

#[derive(Clone)]
pub(super) struct ObservationSink {
    replicated: ReplicatedStore,
    local: RecordOwner,
    plugin: crate::storage::Plugin,
    rescan_interval: Duration,
    pool_committable_bytes: tokio::sync::watch::Sender<Option<u64>>,
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
            pool_committable_bytes: tokio::sync::watch::Sender::new(None),
        });
        self
    }

    /// What this Machine's Pool could still commit, as of the last storage
    /// observation.
    pub(crate) fn pool_committable_bytes(&self) -> tokio::sync::watch::Receiver<Option<u64>> {
        self.sink.as_ref().map_or_else(
            || tokio::sync::watch::channel(None).1,
            |sink| sink.pool_committable_bytes.subscribe(),
        )
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_rescan_interval(mut self, interval: Duration) -> Self {
        if let Some(sink) = &mut self.sink {
            sink.rescan_interval = interval;
        }
        self
    }

    /// Turns `true` once this process has published its container observations
    /// while participating, and stays `true` across watch retries.
    pub(crate) fn first_synced(&self) -> tokio::sync::watch::Receiver<bool> {
        self.first_synced.subscribe()
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
                ("label", vec![labels::MANAGED, labels::NAMESPACE]),
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
                        // `docker update` keeps the id but changes the limits
                        // inspect_managed reports.
                        "update",
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
        on_local_marks: bool,
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
        let mut rotations = self.specs.subscribe_rotations();
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
                // Stopping marks and rotation writes change no Docker state, so no event announces them.
                Ok(()) = stopping.changed(), if on_local_marks => sync(self, sink).await?,
                Ok(()) = rotations.changed(), if on_local_marks => sync(self, sink).await?,
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
        let inventory = self.managed_inventory().await?;
        let mut live = LocalContainerSnapshot::from_inventory(inventory);
        let container_ids = live.ids().cloned().collect::<Vec<_>>();
        let mut doors = Vec::new();
        let mut complete = true;
        for container_id in &container_ids {
            match self.inspect_managed(container_id, &machine_id).await {
                Ok(observation) => {
                    live.observed(redacted_container(&observation));
                    match self.door_facts_of(&observation).await {
                        Ok(Some(facts)) => doors.push(facts),
                        Ok(None) => {}
                        Err(error) => {
                            complete = false;
                            eprintln!(
                                "failed to inspect Ingress door {container_id}: {error}",
                                error = ployz_core::error_chain::inline(&error),
                            );
                        }
                    }
                }
                Err(error) => {
                    complete = false;
                    eprintln!(
                        "failed to inspect managed container {container_id}: {error}",
                        error = ployz_core::error_chain::inline(&error),
                    );
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
        self.first_synced
            .send_if_modified(|synced| !std::mem::replace(synced, true));
        // The Ingress doors are the only Service Containers the daemon starts unasked,
        // and only while this Machine participates and accepts ingress. It starts an
        // existing holder or Caddy, or restarts a Caddy or connector left in its owner's
        // old network; it never creates, replaces, moves or removes one.
        // Started after the guard drops, so a slow Docker start never stalls publication.
        // A reset that takes the guard meanwhile removes the container anyway. The record
        // is read again here, so a policy change that committed during publication holds.
        drop(publication);
        let local = sink.local.record();
        if local.phase() == LocalMachinePhase::Participating
            && local
                .machine()
                .is_some_and(|machine| machine.accepts_ingress)
            && let Some(recovery) = ingress_recovery(&doors, complete)
        {
            // Off the event loop, so a slow Docker start never delays later scans.
            let runtime = self.clone();
            tokio::spawn(async move { runtime.start_stranded(recovery, creates).await });
        }
        Ok(())
    }

    /// The Docker facts of a reserved Ingress Container, or `None` for any other.
    async fn door_facts_of(
        &self,
        observation: &ContainerObservation,
    ) -> Result<Option<DoorFacts>, Error> {
        if observation.kind != ContainerKind::ServiceContainer {
            return Ok(None);
        }
        let Some(door) = Door::of(&observation.identity()) else {
            return Ok(None);
        };
        let inspected = self
            .docker
            .client
            .inspect_container(observation.container_id.as_str(), None)
            .await
            .map_err(|error| super::docker_error(&observation.container_id, error))?;
        Ok(Some(door_facts(observation, door, &inspected)))
    }

    async fn sync_volume_observations(&self, sink: &ObservationSink) -> Result<(), Error> {
        let machine_id = sink.local.record().id();
        let VolumeInventory {
            mut volumes,
            failures,
        } = match self.list_volumes(&machine_id).await {
            Ok(inventory) => inventory,
            Err(error) => {
                sink.pool_committable_bytes
                    .send_if_modified(|current| current.take().is_some());
                return Err(error);
            }
        };
        let committable = match sink
            .plugin
            .call::<ployz_core::StorageCapacity>(crate::volume_copy::STORAGE_INSPECT_ROUTE, &())
            .await
        {
            Ok(capacity) => {
                observe_copies(&machine_id, &mut volumes, &capacity.copies);
                // Whole GiB: each Machine row change rebuilds every peer's mesh routes.
                capacity.committable_hint_bytes().ok()
            }
            Err(error) => {
                eprintln!("Volume copies unobserved, Docker Volumes stay writers: {error}");
                None
            }
        };
        sink.pool_committable_bytes
            .send_if_modified(|current| std::mem::replace(current, committable) != committable);
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
        volume.storage.set_copy(copies.get(&volume.id.name));
    }
    for (name, copy) in copies {
        let place @ CopyPlace::Slot(_) = copy.place() else {
            continue;
        };
        if volumes.iter().any(|volume| volume.id.name == *name) {
            continue;
        }
        let (Ok(mountpoint), Some(bound_bytes)) = (
            ployz_core::MachinePath::parse(crate::dataset_layout::slot_fs_mountpoint(name)),
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
            storage: DockerVolumeStorageObservation::provisioned(
                mountpoint,
                bound_bytes,
                copy.used_bytes,
                Some(place),
            ),
        });
    }
}

pub(crate) fn redacted_container(observation: &ContainerObservation) -> ContainerObservation {
    let mut parts = observation.clone().into_parts();
    parts.resolved_spec = published_spec(parts.resolved_spec);
    ContainerObservation::try_from(parts)
        .expect("environment redaction preserves Container identity")
}

/// A spec as this Machine publishes it: no environment and no Config content.
pub(crate) fn published_spec(mut spec: ResolvedServiceSpec) -> ResolvedServiceSpec {
    spec.container
        .environment
        .retain(|key, _| key == CADDY_ADMIN_ENV);
    if let Some(hook) = &mut spec.pre_deploy {
        hook.environment.clear();
    }
    spec.mount_graph.redact_config_content();
    spec
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

/// One Ingress door recovery a scan decided on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum IngressRecovery {
    /// A holder at rest whose last start failed.
    StartHolder(ContainerId),
    /// A Caddy at rest whose last start failed: one joined to the running `holder`,
    /// or a Caddy from before the holder when none exists.
    StartCaddy {
        holder: Option<ContainerId>,
        caddy: ContainerId,
    },
    /// A running Caddy left in a network namespace the running holder no longer has,
    /// or a running connector left in one the running Tunnel Origin no longer has.
    Rejoin {
        owner: ContainerId,
        joined: ContainerId,
    },
}

impl IngressRecovery {
    /// The Container the recovery starts.
    pub(super) fn target(&self) -> &ContainerId {
        match self {
            Self::StartHolder(id) => id,
            Self::StartCaddy { caddy, .. } => caddy,
            Self::Rejoin { joined, .. } => joined,
        }
    }

    pub(super) fn verb(&self) -> &'static str {
        match self {
            Self::StartHolder(_) | Self::StartCaddy { .. } => "started",
            Self::Rejoin { .. } => "rejoined",
        }
    }
}

/// The one recovery this scan calls for, if any.
///
/// Docker's restart policy covers a door that ran and died, not one whose start
/// failed, such as when another process held port 80 at enrollment or at boot, and
/// it never moves a Caddy into the network of a holder that restarted. A scan that
/// could not inspect every container may have missed a running one, so it names
/// nothing. A Caddy is never selected while its holder is stopped, restarting or
/// unknown: it would join nothing. Unknown evidence does nothing.
fn ingress_recovery(doors: &[DoorFacts], complete: bool) -> Option<IngressRecovery> {
    if !complete {
        return None;
    }
    proxy_recovery(doors).or_else(|| tunnel_recovery(doors))
}

/// The holder's or its Caddy's recovery, if any.
fn proxy_recovery(doors: &[DoorFacts]) -> Option<IngressRecovery> {
    let newest = |facts: &[&DoorFacts]| {
        facts
            .iter()
            .copied()
            .max_by_key(|facts| (facts.created_at, facts.id))
            .cloned()
    };
    let holders = doors
        .iter()
        .filter(|facts| facts.door == Door::Holder)
        .collect::<Vec<_>>();
    let caddys = doors
        .iter()
        .filter(|facts| facts.door == Door::Proxy)
        .collect::<Vec<_>>();
    if holders.is_empty() {
        let legacy = caddys
            .iter()
            .copied()
            .filter(|facts| facts.joined_to.is_none())
            .collect::<Vec<_>>();
        if !caddys.iter().all(|facts| facts.at_rest()) {
            return None;
        }
        let caddy = newest(&legacy)?;
        return caddy.failed_start.then_some(IngressRecovery::StartCaddy {
            holder: None,
            caddy: caddy.id,
        });
    }
    if holders.iter().all(|facts| facts.at_rest()) {
        let holder = newest(&holders)?;
        return holder
            .failed_start
            .then_some(IngressRecovery::StartHolder(holder.id));
    }
    let mut running = holders.iter().copied().filter(|facts| facts.running());
    let (Some(holder), None) = (running.next(), running.next()) else {
        return None;
    };
    if holders
        .iter()
        .any(|facts| !facts.running() && !facts.at_rest())
    {
        return None;
    }
    let joined = caddys
        .iter()
        .copied()
        .filter(|facts| facts.joined_to == Some(holder.id))
        .collect::<Vec<_>>();
    if let Some(caddy) = joined.iter().find(|facts| facts.running()) {
        return match (holder.netns, caddy.netns) {
            (Some(holder_ns), Some(caddy_ns)) if holder_ns != caddy_ns => {
                Some(IngressRecovery::Rejoin {
                    owner: holder.id,
                    joined: caddy.id,
                })
            }
            _ => None,
        };
    }
    if !joined.iter().all(|facts| facts.at_rest()) {
        return None;
    }
    let caddy = newest(&joined)?;
    caddy.failed_start.then_some(IngressRecovery::StartCaddy {
        holder: Some(holder.id),
        caddy: caddy.id,
    })
}

/// A running connector to move into the running Tunnel Origin's network, if any.
///
/// Only that: a door at rest stays at rest, since the route starts the Tunnel Door
/// only once it has written the token and the Cluster still declares it. Docker
/// restarts a connector that exits into the origin's current network, so only a
/// connector that keeps running after its origin restarted is left behind.
fn tunnel_recovery(doors: &[DoorFacts]) -> Option<IngressRecovery> {
    let origins = doors
        .iter()
        .filter(|facts| facts.door == Door::TunnelOrigin)
        .collect::<Vec<_>>();
    let mut running = origins.iter().copied().filter(|facts| facts.running());
    let (Some(origin), None) = (running.next(), running.next()) else {
        return None;
    };
    if origins
        .iter()
        .any(|facts| !facts.running() && !facts.at_rest())
    {
        return None;
    }
    let origin_ns = origin.netns?;
    doors
        .iter()
        .find(|facts| {
            facts.door == Door::Tunnel
                && facts.running()
                && facts.joined_to == Some(origin.id)
                && facts.netns.is_some_and(|ns| ns != origin_ns)
        })
        .map(|connector| IngressRecovery::Rejoin {
            owner: origin.id,
            joined: connector.id,
        })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ployz_core::{ContainerId, ContainerObservation, CopyRole, DockerVolume, DockerVolumeName};
    use serde_json::json;

    use super::{
        IngressRecovery, ingress_recovery, local_container_changes, local_volume_changes,
        observe_copies, redacted_container,
    };
    use crate::corrosion::{LocalContainerSnapshot, LocalVolumeSnapshot};
    use crate::docker::{Door, DoorFacts, NetnsId};

    fn id(id: char) -> ContainerId {
        ContainerId::parse(id.to_string().repeat(64)).unwrap()
    }

    fn door(id_char: char, door: Door, created_at: i64, runtime: serde_json::Value) -> DoorFacts {
        DoorFacts {
            id: id(id_char),
            door,
            created_at,
            runtime: serde_json::from_value(runtime).unwrap(),
            failed_start: false,
            joined_to: None,
            netns: None,
        }
    }

    fn holder(id: char, created_at: i64, runtime: serde_json::Value) -> DoorFacts {
        door(id, Door::Holder, created_at, runtime)
    }

    fn caddy(id: char, created_at: i64, runtime: serde_json::Value) -> DoorFacts {
        door(id, Door::Proxy, created_at, runtime)
    }

    fn joined(mut facts: DoorFacts, holder: char) -> DoorFacts {
        facts.joined_to = Some(id(holder));
        facts
    }

    fn failed(mut facts: DoorFacts) -> DoorFacts {
        facts.failed_start = true;
        facts
    }

    fn in_netns(mut facts: DoorFacts, ino: u64) -> DoorFacts {
        facts.netns = Some(NetnsId::new(ino));
        facts
    }

    fn created() -> serde_json::Value {
        json!({ "state": "created" })
    }

    fn running() -> serde_json::Value {
        json!({ "state": "running", "health": "healthy" })
    }

    #[test]
    fn a_holder_whose_start_failed_is_started_again() {
        assert_eq!(
            ingress_recovery(&[failed(holder('a', 1, created()))], true),
            Some(IngressRecovery::StartHolder(id('a')))
        );
        assert_eq!(
            ingress_recovery(&[holder('a', 1, created())], true),
            None,
            "a holder Docker never tried to start is left alone"
        );
        assert_eq!(
            ingress_recovery(
                &[
                    failed(holder('a', 1, created())),
                    failed(holder('d', 2, json!({ "state": "exited", "code": 128 }))),
                ],
                true
            ),
            Some(IngressRecovery::StartHolder(id('d'))),
            "the newest holder at rest is the candidate"
        );
    }

    #[test]
    fn a_caddy_is_never_selected_while_its_holder_is_not_running() {
        let caddy = failed(joined(caddy('c', 2, created()), 'a'));
        assert_eq!(
            ingress_recovery(&[holder('a', 1, created()), caddy.clone()], true),
            None,
            "a stopped holder with no failed start starts nothing"
        );
        assert_eq!(
            ingress_recovery(
                &[
                    holder('a', 1, json!({ "state": "restarting" })),
                    caddy.clone()
                ],
                true
            ),
            None
        );
        assert_eq!(
            ingress_recovery(
                &[
                    holder('a', 1, json!({ "unrecognized": { "raw": "odd" } })),
                    caddy
                ],
                true
            ),
            None
        );
    }

    #[test]
    fn a_joined_caddy_whose_start_failed_is_started_once_the_holder_runs() {
        assert_eq!(
            ingress_recovery(
                &[
                    holder('a', 1, running()),
                    failed(joined(caddy('c', 2, created()), 'a'))
                ],
                true
            ),
            Some(IngressRecovery::StartCaddy {
                holder: Some(id('a')),
                caddy: id('c'),
            })
        );
        assert_eq!(
            ingress_recovery(
                &[
                    holder('a', 1, running()),
                    failed(joined(caddy('c', 2, created()), 'b'))
                ],
                true
            ),
            None,
            "a Caddy joined to another holder is not this holder's"
        );
        assert_eq!(
            ingress_recovery(
                &[
                    holder('a', 1, running()),
                    joined(caddy('c', 2, running()), 'a'),
                    failed(joined(caddy('d', 3, created()), 'a')),
                ],
                true
            ),
            None,
            "one Caddy runs, so none is started"
        );
    }

    fn tunnel_origin(id: char, created_at: i64, runtime: serde_json::Value) -> DoorFacts {
        door(id, Door::TunnelOrigin, created_at, runtime)
    }

    fn connector(id: char, created_at: i64, runtime: serde_json::Value) -> DoorFacts {
        door(id, Door::Tunnel, created_at, runtime)
    }

    #[test]
    fn a_running_connector_in_the_origins_old_namespace_is_rejoined() {
        let doors = |connector_ns| {
            [
                in_netns(holder('a', 1, running()), 1),
                in_netns(joined(caddy('b', 2, running()), 'a'), 1),
                in_netns(tunnel_origin('e', 3, running()), 10),
                in_netns(joined(connector('f', 4, running()), 'e'), connector_ns),
            ]
        };
        assert_eq!(
            ingress_recovery(&doors(11), true),
            Some(IngressRecovery::Rejoin {
                owner: id('e'),
                joined: id('f'),
            })
        );
        assert_eq!(ingress_recovery(&doors(10), true), None);
        assert_eq!(
            ingress_recovery(
                &[
                    in_netns(tunnel_origin('e', 3, running()), 10),
                    failed(joined(connector('f', 4, created()), 'e')),
                ],
                true
            ),
            None,
            "a connector at rest waits for the route, which writes its token first"
        );
    }

    #[test]
    fn a_running_caddy_in_the_holders_old_namespace_is_rejoined() {
        assert_eq!(
            ingress_recovery(
                &[
                    in_netns(holder('a', 1, running()), 10),
                    in_netns(joined(caddy('c', 2, running()), 'a'), 11),
                ],
                true
            ),
            Some(IngressRecovery::Rejoin {
                owner: id('a'),
                joined: id('c'),
            })
        );
        assert_eq!(
            ingress_recovery(
                &[
                    in_netns(holder('a', 1, running()), 10),
                    in_netns(joined(caddy('c', 2, running()), 'a'), 10),
                ],
                true
            ),
            None,
            "a Caddy in the holder's namespace is where it belongs"
        );
        assert_eq!(
            ingress_recovery(
                &[
                    in_netns(holder('a', 1, running()), 10),
                    joined(caddy('c', 2, running()), 'a'),
                ],
                true
            ),
            None,
            "an unknown namespace is no evidence"
        );
    }

    #[test]
    fn without_a_holder_the_legacy_caddy_rule_holds() {
        assert_eq!(
            ingress_recovery(&[failed(caddy('a', 1, created()))], true),
            Some(IngressRecovery::StartCaddy {
                holder: None,
                caddy: id('a'),
            })
        );
        assert_eq!(
            ingress_recovery(
                &[
                    failed(caddy('a', 1, json!({ "state": "exited", "code": 0 }))),
                    failed(caddy('d', 2, created())),
                ],
                true
            ),
            Some(IngressRecovery::StartCaddy {
                holder: None,
                caddy: id('d'),
            })
        );
        assert_eq!(
            ingress_recovery(
                &[caddy('a', 1, running()), failed(caddy('d', 2, created()))],
                true
            ),
            None
        );
        assert_eq!(
            ingress_recovery(&[failed(joined(caddy('a', 1, created()), 'e'))], true),
            None,
            "a Caddy joined to a holder that is gone has no network to start in"
        );
        assert_eq!(ingress_recovery(&[], true), None);
    }

    #[test]
    fn a_scan_that_could_not_inspect_every_container_recovers_nothing() {
        assert_eq!(
            ingress_recovery(&[failed(holder('a', 1, created()))], false),
            None
        );
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
                .and_then(ployz_core::EnvValue::text),
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
        let mut spec = crate::images::legacy_caddy_for_tests("caddy:test")
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
                rotation: None,
                r#gen: None,
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
                stopped: false,
                run: None,
            })
            .unwrap();

        let redacted = redacted_container(&observation);

        assert_eq!(
            redacted
                .resolved_spec
                .container
                .environment
                .get("CADDY_ADMIN")
                .and_then(ployz_core::EnvValue::text),
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
        let mut new = observation;
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
                slot_role: None,
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
    fn a_slots_recorded_role_reaches_its_observation_listed_or_not() {
        use ployz_core::{CopyPlace, DockerVolumeStorageObservation::Provisioned, SlotRole};
        let machine_id = ployz_core::MachineId::parse("b".repeat(32)).unwrap();
        let name = |name: &str| DockerVolumeName::parse(name).unwrap();
        let maximum_bytes = ployz_core::ProvisionedVolumeMaximumBytes::new(
            std::num::NonZeroU64::new(1024).unwrap(),
        );
        let listed = |volume: &str| DockerVolume {
            id: ployz_core::DockerVolumeId {
                machine_id,
                name: name(volume),
            },
            options: BTreeMap::new(),
            labels: BTreeMap::new(),
            storage: Provisioned {
                mountpoint: ployz_core::MachinePath::parse(format!(
                    "/var/lib/ployz-volumes/{volume}"
                ))
                .unwrap(),
                bound_bytes: std::num::NonZeroU64::new(1024).unwrap(),
                used_bytes: 0,
                role: None,
                slot_role: None,
            },
        };
        let copy =
            |place, used_bytes| ployz_core::ProvisionedCopy::new(place, maximum_bytes, used_bytes);
        let copies = BTreeMap::from([
            (name("kept"), copy(CopyPlace::Writer, 5)),
            (
                name("old"),
                copy(CopyPlace::Slot(Some(SlotRole::OldCopy)), 0),
            ),
            (
                name("mirrored"),
                copy(CopyPlace::Slot(Some(SlotRole::Mirror)), 3),
            ),
            (name("unmarked"), copy(CopyPlace::Slot(None), 4)),
        ]);
        let mut volumes = vec![listed("kept"), listed("old")];

        observe_copies(&machine_id, &mut volumes, &copies);

        let pair = |volume: &DockerVolume| match &volume.storage {
            Provisioned {
                role,
                slot_role,
                used_bytes,
                ..
            } => (*role, *slot_role, *used_bytes),
            other => panic!("not provisioned: {other:?}"),
        };
        let seen: Vec<_> = volumes
            .iter()
            .map(|volume| (volume.id.name.to_string(), pair(volume)))
            .collect();
        assert_eq!(
            seen,
            vec![
                ("kept".to_owned(), (Some(CopyRole::Writer), None, 0)),
                (
                    "old".to_owned(),
                    (Some(CopyRole::Slot), Some(SlotRole::OldCopy), 0)
                ),
                (
                    "mirrored".to_owned(),
                    (Some(CopyRole::Slot), Some(SlotRole::Mirror), 3)
                ),
                ("unmarked".to_owned(), (Some(CopyRole::Slot), None, 4)),
            ]
        );
    }

    #[test]
    fn a_stray_slot_role_on_a_writer_or_switching_copy_reaches_its_observation_as_received() {
        use ployz_core::DockerVolumeStorageObservation::Provisioned;
        let machine_id = ployz_core::MachineId::parse("b".repeat(32)).unwrap();
        let name = DockerVolumeName::parse("stray").unwrap();
        let mut volumes = vec![DockerVolume {
            id: ployz_core::DockerVolumeId {
                machine_id,
                name: name.clone(),
            },
            options: BTreeMap::new(),
            labels: BTreeMap::new(),
            storage: Provisioned {
                mountpoint: ployz_core::MachinePath::parse("/var/lib/ployz-volumes/stray").unwrap(),
                bound_bytes: std::num::NonZeroU64::new(1024).unwrap(),
                used_bytes: 0,
                role: None,
                slot_role: None,
            },
        }];
        for (role, slot_role) in [
            ("writer", "mirror"),
            ("writer", "old_copy"),
            ("switching", "mirror"),
            ("switching", "old_copy"),
        ] {
            let copy: ployz_core::ProvisionedCopy = serde_json::from_str(&format!(
                r#"{{"role":"{role}","maximum_bytes":1024,"used_bytes":0,"slot_role":"{slot_role}"}}"#
            ))
            .unwrap();
            observe_copies(
                &machine_id,
                &mut volumes,
                &BTreeMap::from([(name.clone(), copy)]),
            );
            let Provisioned {
                role: seen_role,
                slot_role: seen_slot_role,
                ..
            } = &volumes.first().unwrap().storage
            else {
                panic!("not provisioned");
            };
            assert_eq!(
                serde_json::to_string(&(seen_role, seen_slot_role)).unwrap(),
                format!(r#"["{role}","{slot_role}"]"#)
            );
        }
    }

    #[test]
    fn copies_give_docker_volumes_their_role_and_add_each_unlisted_slot() {
        use ployz_core::CopyPlace;
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
                slot_role: None,
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
        let copy = |place, used_bytes| {
            ployz_core::ProvisionedCopy::new(
                place,
                ployz_core::ProvisionedVolumeMaximumBytes::new(
                    std::num::NonZeroU64::new(1024).unwrap(),
                ),
                used_bytes,
            )
        };
        let copies = BTreeMap::from([
            (name("data"), copy(CopyPlace::Switching, 512)),
            (name("mirror"), copy(CopyPlace::Slot(None), 300)),
        ]);
        let mut volumes = vec![provisioned("data", None), plain.clone()];

        observe_copies(&machine_id, &mut volumes, &copies);

        let mut slot = provisioned("mirror", Some(CopyRole::Slot));
        slot.storage = ployz_core::DockerVolumeStorageObservation::Provisioned {
            mountpoint: ployz_core::MachinePath::parse("/var/lib/ployz-mirror/mirror/fs").unwrap(),
            bound_bytes: std::num::NonZeroU64::new(1024).unwrap(),
            used_bytes: 300,
            role: Some(CopyRole::Slot),
            slot_role: None,
        };
        assert_eq!(
            volumes,
            vec![provisioned("data", Some(CopyRole::Switching)), plain, slot]
        );

        observe_copies(&machine_id, &mut volumes, &copies);
        assert_eq!(volumes.len(), 3, "a listed slot is not added twice");
    }

    struct Replicating {
        runtime: super::ContainerRuntime,
        replicated: crate::corrosion::ReplicatedStore,
        corrosion: tokio::task::JoinHandle<()>,
        container_id: ContainerId,
        data_dir: std::path::PathBuf,
    }

    #[tokio::test]
    async fn storage_inspect_sets_what_the_pool_could_still_commit() {
        use std::time::Duration;

        use ployz_core::{
            AdvertisedEndpoint, MachineId, MachineName, StorageBacking, StorageCapacity,
        };
        use tokio_util::sync::CancellationToken;

        use crate::corrosion::store_tests::{corrosion_db, identity_store};
        use crate::docker::test_support::runtime_with_running_container;
        use crate::machine::{LocalMachineStore, RecordOwner};
        use crate::storage::test_support::FakePlugin;

        let data_dir =
            std::env::temp_dir().join(format!("ployzd-committable-{}", MachineId::random()));
        let mut store = LocalMachineStore::open(&data_dir).unwrap();
        store
            .enroll(ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: MachineName::parse("local").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: vec![AdvertisedEndpoint(
                        "192.0.2.1:51820".parse().unwrap(),
                    )],
                    grant: None,
                },
                wireguard_mtu: None,
                pinned: Default::default(),
            })
            .unwrap();
        let owner = RecordOwner::spawn(store).unwrap();
        let fake = FakePlugin::default();
        let gib = ployz_core::STORAGE_GIB;
        fake.reply(
            "Storage.Inspect",
            serde_json::json!({"Ok": StorageCapacity {
                backing: StorageBacking::Fixed {
                    pool_size_bytes: 10 * gib,
                },
                unmanaged_used_bytes: 1000,
                slot_bound_bytes: 0,
                volumes: BTreeMap::new(),
                copies: BTreeMap::new(),
            }}),
        );
        let (plugin, plugin_server) = fake.serve(&data_dir);
        let (replicated, corrosion) = identity_store(corrosion_db()).await;
        let (runtime, docker, _id) = runtime_with_running_container().await;
        let runtime = runtime
            .replicating(replicated, owner, plugin)
            .with_rescan_interval(Duration::from_millis(50));
        let committable = runtime.pool_committable_bytes();
        let shutdown = CancellationToken::new();
        let publisher = tokio::spawn({
            let (runtime, shutdown) = (runtime.clone(), shutdown.clone());
            async move { runtime.publish_observations(shutdown).await }
        });
        let observed = |expected: Option<u64>| {
            let mut committable = committable.clone();
            async move {
                tokio::time::timeout(
                    Duration::from_secs(5),
                    committable.wait_for(|value| *value == expected),
                )
                .await
                .unwrap_or_else(|_| panic!("{expected:?} was never observed"))
                .unwrap();
            }
        };

        observed(Some(8 * gib)).await;
        docker
            .reject_list
            .store(true, std::sync::atomic::Ordering::Relaxed);
        observed(None).await;
        docker
            .reject_list
            .store(false, std::sync::atomic::Ordering::Relaxed);
        observed(Some(8 * gib)).await;
        fake.replies.lock().unwrap().remove("Storage.Inspect");
        observed(None).await;

        shutdown.cancel();
        publisher.await.unwrap().unwrap();
        plugin_server.abort();
        corrosion.abort();
        let _ = std::fs::remove_dir_all(data_dir);
    }

    async fn wall_clock(duration: std::time::Duration) {
        // real-wait: the fake Docker and Corrosion answer over loopback sockets, and paused time would jump past their replies.
        tokio::time::sleep(duration).await;
    }

    async fn replicating(rescan: std::time::Duration) -> Replicating {
        use ployz_core::{AdvertisedEndpoint, MachineId, MachineName};

        use crate::corrosion::store_tests::{corrosion_db, identity_store};
        use crate::docker::test_support::runtime_with_running_container;
        use crate::machine::{LocalMachineStore, RecordOwner};

        let data_dir =
            std::env::temp_dir().join(format!("ployzd-rotation-{}", MachineId::random()));
        let mut store = LocalMachineStore::open(&data_dir).unwrap();
        store
            .enroll(ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: MachineName::parse("local").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: vec![AdvertisedEndpoint(
                        "192.0.2.1:51820".parse().unwrap(),
                    )],
                    grant: None,
                },
                wireguard_mtu: None,
                pinned: Default::default(),
            })
            .unwrap();
        let owner = RecordOwner::spawn(store).unwrap();
        let (replicated, corrosion) = identity_store(corrosion_db()).await;
        let (runtime, _fake, container_id) = runtime_with_running_container().await;
        let runtime = runtime
            .replicating(replicated.clone(), owner, crate::storage::Plugin::default())
            .with_rescan_interval(rescan);
        Replicating {
            runtime,
            replicated,
            corrosion,
            container_id,
            data_dir,
        }
    }

    #[tokio::test]
    async fn rotation_write_wakes_sync() {
        use std::time::Duration;

        use ployz_core::CopyRotation;
        use tokio_util::sync::CancellationToken;

        let Replicating {
            runtime,
            replicated,
            corrosion,
            container_id: id,
            data_dir,
        } = replicating(Duration::from_secs(3600)).await;
        let shutdown = CancellationToken::new();
        let publisher = tokio::spawn({
            let (runtime, shutdown) = (runtime.clone(), shutdown.clone());
            async move { runtime.publish_observations(shutdown).await }
        });
        let published = || async {
            replicated
                .container(&id)
                .await
                .unwrap()
                .map(|observation| observation.rotation().clone())
        };
        let wait_for = |rotation: CopyRotation| async move {
            tokio::time::timeout(Duration::from_secs(5), async {
                while published().await.as_ref() != Some(&rotation) {
                    wall_clock(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{rotation:?} was never published"));
        };

        wait_for(CopyRotation::Pending).await;
        runtime
            .rotation_turn(&id)
            .await
            .unwrap()
            .write(&CopyRotation::In)
            .await
            .unwrap();
        wait_for(CopyRotation::In).await;

        shutdown.cancel();
        publisher.await.unwrap().unwrap();
        corrosion.abort();
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn first_sync_signals_once() {
        use std::time::Duration;

        use tokio_util::sync::CancellationToken;

        let Replicating {
            runtime,
            replicated,
            corrosion,
            container_id,
            data_dir,
        } = replicating(Duration::from_millis(20)).await;
        let sink = runtime.sink.clone().unwrap();
        let mut synced = runtime.first_synced();
        let run_awhile = async |watcher: tokio::task::JoinHandle<()>,
                                shutdown: CancellationToken| {
            wall_clock(Duration::from_millis(200)).await;
            shutdown.cancel();
            watcher.await.unwrap();
        };
        let volumes = |shutdown: CancellationToken| {
            let (runtime, sink) = (runtime.clone(), sink.clone());
            tokio::spawn(async move {
                runtime
                    .watch_volume_observations(&sink, &shutdown)
                    .await
                    .unwrap();
            })
        };
        let containers = |shutdown: CancellationToken| {
            let (runtime, sink) = (runtime.clone(), sink.clone());
            tokio::spawn(async move {
                runtime.watch_observations(&sink, &shutdown).await.unwrap();
            })
        };

        let shutdown = CancellationToken::new();
        run_awhile(volumes(shutdown.clone()), shutdown).await;
        assert!(
            !*synced.borrow_and_update(),
            "the Volume observer never signals"
        );

        let shutdown = CancellationToken::new();
        let first = containers(shutdown.clone());
        tokio::time::timeout(Duration::from_secs(5), synced.changed())
            .await
            .expect("the first sync signals")
            .unwrap();
        assert!(*synced.borrow_and_update());
        assert!(
            replicated.container(&container_id).await.unwrap().is_some(),
            "the signal follows the published observation"
        );
        run_awhile(first, shutdown).await;

        let shutdown = CancellationToken::new();
        run_awhile(containers(shutdown.clone()), shutdown).await;
        assert!(
            !synced.has_changed().unwrap(),
            "rescans and a retried watch leave the signal as it was"
        );
        assert!(*synced.borrow());

        corrosion.abort();
        let _ = std::fs::remove_dir_all(data_dir);
    }
}
