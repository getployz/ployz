use std::{collections::BTreeMap, io, sync::Arc, time::Duration};

use tokio_util::sync::CancellationToken;

use super::{Error, ReplicatedStore};
use crate::machine::{LocalMachineBody, RecordOwner};

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
            Err(error) => error.to_string(),
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
/// policy edit) or the number of Builds it runs changes: the one path that
/// publishes this Machine.
pub async fn run_machine_publisher(
    replicated: Option<ReplicatedStore>,
    local: RecordOwner,
    mut running_builds: tokio::sync::watch::Receiver<u32>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let mut records = local.watch();
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
                eprintln!("failed to publish Cluster network: {error}");
            }
            let publication = replicated.machine_publication().await;
            let running = *running_builds.borrow_and_update();
            let record = Arc::clone(&records.borrow_and_update());
            let machine = publication.publishable_machine(&record, running);
            if let Some(machine) = machine
                && let Err(error) = publication.publish(&machine).await
            {
                eprintln!("failed to publish local Machine: {error}");
            }
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(60)) => {}
            // A closed channel (no Build runner) never changes again.
            Ok(()) = running_builds.changed() => {}
            Ok(()) = records.changed() => {}
            () = shutdown.cancelled() => {
                return Ok(());
            }
        }
    }
}
