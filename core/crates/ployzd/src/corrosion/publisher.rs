use std::{collections::BTreeMap, io, sync::Arc, time::Duration};

use tokio_util::sync::CancellationToken;

use super::{Error, MachineView, MachinesSnapshot, ReplicatedStore, store::RuntimeOverlay};
use crate::machine::{LocalMachineBody, LocalMachineRecord, RecordOwner};

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

    fn mark_seen(&mut self) -> RuntimeOverlay {
        RuntimeOverlay {
            running_builds: *self.running_builds.borrow_and_update(),
            pool_committable_bytes: *self.pool_committable_bytes.borrow_and_update(),
        }
    }

    fn current(&self) -> RuntimeOverlay {
        RuntimeOverlay {
            running_builds: *self.running_builds.borrow(),
            pool_committable_bytes: *self.pool_committable_bytes.borrow(),
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
    // A closed channel (no Cluster store) never changes again.
    let mut machines = cluster.as_ref().map_or_else(
        || tokio::sync::watch::channel(None).1,
        |(_, view)| view.watch(),
    );
    let replicated = cluster.map(|(replicated, _)| replicated);
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
        if let Some(replicated) = &replicated {
            let record = local.record();
            if let Some(network) = record.cluster_network()
                && let Err(error) = replicated.publish_cluster_network(network).await
            {
                eprintln!(
                    "failed to publish Cluster network: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
            }
            let publication = replicated.machine_publication().await;
            let overlay = live.mark_seen();
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
                Ok(()) = records.changed() => break,
                Ok(()) = machines.changed() => {
                    let snapshot = machines.borrow_and_update().clone();
                    if let Some(replicated) = &replicated
                        && row_is_stale(replicated, &snapshot, &records, &live).await
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

/// Whether the snapshot holds this Machine's row unlike what it would publish.
/// Other Machines' rows changing leaves the publisher waiting.
async fn row_is_stale(
    replicated: &ReplicatedStore,
    snapshot: &MachinesSnapshot,
    records: &tokio::sync::watch::Receiver<Arc<LocalMachineRecord>>,
    live: &LiveRuntime,
) -> bool {
    let Some(snapshot) = snapshot else {
        return false;
    };
    let record = Arc::clone(&records.borrow());
    let Some(machine) = replicated
        .machine_publication()
        .await
        .publishable_machine(&record, live.current())
    else {
        return false;
    };
    !snapshot.observations.contains(&machine)
}
