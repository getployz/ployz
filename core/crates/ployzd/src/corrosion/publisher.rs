use std::{collections::BTreeMap, io, sync::Arc, time::Duration};

use tokio_util::sync::CancellationToken;

use super::{Error, MachineView, MachinesSnapshot, ReplicatedStore, store::RuntimeOverlay};
use crate::machine::{LocalMachineBody, LocalMachineRecord, RecordOwner};
use crate::sealing_key::KeyFile;

/// What this Machine observes of itself live: published on its row, never held in
/// its record.
pub struct LiveRuntime {
    running_builds: tokio::sync::watch::Receiver<u32>,
    pool_committable_bytes: tokio::sync::watch::Receiver<Option<u64>>,
}

impl LiveRuntime {
    #[must_use]
    pub fn new(
        running_builds: tokio::sync::watch::Receiver<u32>,
        pool_committable_bytes: tokio::sync::watch::Receiver<Option<u64>>,
    ) -> Self {
        Self {
            running_builds,
            pool_committable_bytes,
        }
    }

    #[cfg(test)]
    pub(crate) fn unobserved() -> Self {
        Self::new(
            tokio::sync::watch::channel(0).1,
            tokio::sync::watch::channel(None).1,
        )
    }

    fn mark_seen(&mut self, sealing: Option<ployz_core::Sealing>) -> RuntimeOverlay {
        RuntimeOverlay {
            running_builds: *self.running_builds.borrow_and_update(),
            pool_committable_bytes: *self.pool_committable_bytes.borrow_and_update(),
            sealing,
        }
    }

    fn current(&self, sealing: Option<ployz_core::Sealing>) -> RuntimeOverlay {
        RuntimeOverlay {
            running_builds: *self.running_builds.borrow(),
            pool_committable_bytes: *self.pool_committable_bytes.borrow(),
            sealing,
        }
    }

    async fn changed(&mut self) {
        // A closed channel (no Build runner, no storage observer) never changes again.
        tokio::select! {
            Ok(()) = self.running_builds.changed() => {}
            Ok(()) = self.pool_committable_bytes.changed() => {}
            else => std::future::pending().await,
        }
    }
}

/// Waits for replication through the target, retrying store failures.
///
/// # Errors
/// Returns immediately if the target contains an invalid actor ID or negative version.
pub async fn wait_for_catch_up(
    store: &ReplicatedStore,
    target: &BTreeMap<String, i64>,
) -> Result<(), Error> {
    let warning_interval = Duration::from_secs(5 * 60);
    let mut warning_at = tokio::time::Instant::now() + warning_interval;
    loop {
        let status = match store.has_reached_version(target).await {
            Ok(true) => return Ok(()),
            Ok(false) => "target replication is incomplete".to_owned(),
            Err(error @ Error::InvalidCatchUpTarget(_)) => return Err(error),
            Err(error) => ployz_core::error_chain::inline(&error),
        };
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(500)) => {}
            () = tokio::time::sleep_until(warning_at) => {
                eprintln!("cluster store catch-up is still pending: {status}");
                warning_at = tokio::time::Instant::now() + warning_interval;
            }
        }
    }
}

/// Publish this Machine now and every minute, and whenever its record (a
/// policy edit) or what it observes of itself live changes: the one path that
/// publishes this Machine. Another writer (an Entry's Register, a Forget) can
/// replace this Machine's row, so a Machines view change that leaves the row
/// unlike what this Machine would publish restores the row at once.
pub async fn run_machine_publisher(
    cluster: Option<(ReplicatedStore, MachineView)>,
    local: RecordOwner,
    mut live: LiveRuntime,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let mut records = local.watch();
    let mut sealing = local.sealing();
    // A closed channel (no Cluster store) never changes again.
    let mut machines = cluster.as_ref().map_or_else(
        || tokio::sync::watch::channel(None).1,
        |(_, view)| view.watch(),
    );
    let replicated = cluster.map(|(replicated, _)| replicated);
    let mut published_sealing = None;
    if let Some(replicated) = &replicated {
        let target = match local.record().body() {
            LocalMachineBody::Joining {
                min_store_version, ..
            } => Some(min_store_version.clone()),
            LocalMachineBody::Uninitialized { .. }
            | LocalMachineBody::Participating { .. }
            | LocalMachineBody::Resetting { .. } => None,
        };
        if let Some(target) = target {
            tokio::select! {
                result = wait_for_catch_up(replicated, &target) => {
                    result.map_err(io::Error::other)?;
                }
                () = shutdown.cancelled() => {
                    return Ok(());
                }
            }
            let publication = replicated.machine_publication().await;
            let completed = publication
                .complete_catch_up(&local)
                .await
                .map_err(io::Error::other)?;
            if completed {
                // Join already restarted into Joining. The published Participating
                // record starts DNS/ingress in-process; another process restart
                // kills an in-flight Ingress Proxy Deploy against this Machine.
                tracing::info!("catch-up complete");
            }
        }
    }
    loop {
        if let Some(replicated) = &replicated
            && awaited_sealing_key_published(replicated, &local.record()).await
        {
            let record = local.record();
            if let Some(network) = record.cluster_network()
                && let Err(error) = replicated.publish_cluster_network(network).await
            {
                eprintln!(
                    "failed to publish Cluster network: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
            }
            let observed = *sealing.borrow_and_update();
            published_sealing = awaiting_recovery(replicated, observed, &record).await;
            let overlay = live.mark_seen(published_sealing);
            if let Some(ployz_core::Sealing::Held { public_key }) = overlay.sealing
                && held_key_publishable(replicated, public_key, &record).await
                && let Err(error) = replicated.publish_sealing_public_key(public_key).await
            {
                eprintln!(
                    "failed to publish Sealing public key: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
            }
            let publication = replicated.machine_publication().await;
            let record = Arc::clone(&records.borrow_and_update());
            let machine = publication.publishable_machine(&record, overlay);
            if let Some(machine) = machine
                && let Err(error) = publication.publish(&machine).await
            {
                eprintln!(
                    "failed to publish local Machine: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
            }
        }
        let tick = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(tick) => break,
                () = live.changed() => break,
                Ok(()) = sealing.changed() => break,
                Ok(()) = records.changed() => break,
                Ok(()) = machines.changed() => {
                    let snapshot = machines.borrow_and_update().clone();
                    let overlay = live.current(published_sealing);
                    if let Some(replicated) = &replicated
                        && row_is_stale(replicated, &snapshot, &records, overlay).await
                    {
                        break;
                    }
                }
                () = shutdown.cancelled() => {
                    return Ok(());
                }
            }
        }
    }
}

/// This Machine's Sealing from the key file it observed: the key it holds, or,
/// when it holds none, the key a recovery awaits as it reads its Cluster's
/// Sealing, so Cloud can tell a lost key from a missing grant. `None` when the
/// file or the Cluster's Sealing could not be read.
async fn awaiting_recovery(
    replicated: &ReplicatedStore,
    observed: Option<KeyFile<ployz_core::SealingPublicKey>>,
    record: &LocalMachineRecord,
) -> Option<ployz_core::Sealing> {
    observed?
        .sealing(async {
            replicated
                .cluster_sealing(record.recovery_seed())
                .await
                .map(Some)
        })
        .await
        .inspect_err(|error| {
            eprintln!(
                "failed to read whether the Cluster awaits recovery: {error}",
                error = ployz_core::error_chain::inline(error),
            );
        })
        .ok()
}

/// Whether to insert the held Sealing public key as the Cluster's: not while
/// this Machine reads its Cluster awaiting recovery of another key, as
/// [`ClusterSealing::awaits_another`](super::ClusterSealing::awaits_another)
/// answers. [`KeyFile::sealing`] reads no Cluster row for a held key, so this
/// read narrows only the insert, never what the row reports. A failed read is
/// logged and the key is inserted.
async fn held_key_publishable(
    replicated: &ReplicatedStore,
    held: ployz_core::SealingPublicKey,
    record: &LocalMachineRecord,
) -> bool {
    match replicated.cluster_sealing(record.recovery_seed()).await {
        Ok(cluster) => match cluster.awaits_another(held) {
            None => true,
            Some(awaited) => {
                eprintln!(
                    "not publishing Sealing public key {held}: this Machine reads that its Cluster awaits recovery of {awaited}",
                );
                false
            }
        },
        Err(error) => {
            eprintln!(
                "failed to read whether the Cluster awaits recovery of another Sealing public key; publishing {held}: {error}",
                error = ployz_core::error_chain::inline(&error),
            );
            true
        }
    }
}

/// A founder awaiting recovery names the Sealing public key before it publishes
/// anything else, so no Machine that sees its Cluster misses it.
async fn awaited_sealing_key_published(
    replicated: &ReplicatedStore,
    record: &LocalMachineRecord,
) -> bool {
    let Some(seed) = record.recovery_seed() else {
        return true;
    };
    match replicated
        .publish_sealing_awaits_recovery(seed.expected_public_key)
        .await
    {
        Ok(()) => true,
        Err(error) => {
            eprintln!(
                "failed to publish the Sealing public key awaiting recovery: {error}",
                error = ployz_core::error_chain::inline(&error),
            );
            false
        }
    }
}

/// Whether the snapshot holds this Machine's row unlike what it would publish.
/// Other Machines' rows changing leaves the publisher waiting.
async fn row_is_stale(
    replicated: &ReplicatedStore,
    snapshot: &MachinesSnapshot,
    records: &tokio::sync::watch::Receiver<Arc<LocalMachineRecord>>,
    overlay: RuntimeOverlay,
) -> bool {
    let Some(snapshot) = snapshot else {
        return false;
    };
    let record = Arc::clone(&records.borrow());
    let Some(machine) = replicated
        .machine_publication()
        .await
        .publishable_machine(&record, overlay)
    else {
        return false;
    };
    !snapshot.observations.contains(&machine)
}
