use std::{collections::BTreeMap, io, time::Duration};

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

pub async fn run_machine_publisher(
    replicated: Option<ReplicatedStore>,
    local: RecordOwner,
    shutdown: CancellationToken,
) -> io::Result<()> {
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
            let machine = publication.publishable_machine(&local.record());
            if let Some(machine) = machine
                && let Err(error) = publication.publish(&machine).await
            {
                eprintln!("failed to publish local Machine: {error}");
            }
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(60)) => {}
            () = shutdown.cancelled() => {
                return Ok(());
            }
        }
    }
}
