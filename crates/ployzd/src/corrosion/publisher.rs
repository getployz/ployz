use std::{
    collections::BTreeMap,
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use ployz_core::{LocalMachinePhase, MachineId};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{Error, ReplicatedStore};
use crate::machine::{LocalMachineBody, LocalMachineRecord, LocalMachineStore};

pub async fn wait_for_catch_up(
    store: &ReplicatedStore,
    target: &BTreeMap<String, i64>,
) -> Result<(), Error> {
    if target.is_empty() {
        return Ok(());
    }
    let warning_interval = Duration::from_secs(5 * 60);
    let mut warning_at = tokio::time::Instant::now() + warning_interval;
    loop {
        let status = match store.version().await {
            Ok(local) => {
                let lagging = target
                    .iter()
                    .filter(|(actor, target)| {
                        local.get(*actor).copied().unwrap_or_default() < **target
                    })
                    .count();
                if lagging == 0 {
                    match store.has_known_missing_changes().await {
                        Ok(false) => return Ok(()),
                        Ok(true) => "known bookkeeping gaps remain".to_owned(),
                        Err(error) => error.to_string(),
                    }
                } else {
                    format!("{lagging} actor(s) remain behind the target")
                }
            }
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
    local: Arc<Mutex<LocalMachineStore>>,
    participating: watch::Sender<bool>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    if let Some(replicated) = &replicated {
        let (joining, target) = {
            let local = local
                .lock()
                .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
            match local.record().body() {
                LocalMachineBody::Joining {
                    min_store_version, ..
                } => (true, min_store_version.clone()),
                LocalMachineBody::Uninitialized { .. }
                | LocalMachineBody::Participating { .. }
                | LocalMachineBody::Resetting { .. } => (false, BTreeMap::new()),
            }
        };
        if joining {
            tokio::select! {
                result = wait_for_catch_up(replicated, &target) => {
                    result.map_err(io::Error::other)?;
                }
                () = shutdown.cancelled() => {
                    return Ok(());
                }
            }
            let publication = replicated.machine_publication().await;
            let completed = {
                let mut local = local
                    .lock()
                    .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
                let completed = publication
                    .complete_catch_up(&mut local)
                    .map_err(io::Error::other)?;
                if completed {
                    participating.send_replace(true);
                }
                completed
            };
            if completed {
                // Join already restarted into Joining. Flip Participating
                // in-process so DNS/ingress start; another process restart
                // kills an in-flight Ingress Proxy Deploy against this Machine.
                tracing::info!("catch-up complete");
            }
        }
    }
    loop {
        if let Some(replicated) = &replicated {
            let (cluster_network, founder_id) = {
                let local = local
                    .lock()
                    .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
                let record = local.record();
                (record.cluster_network(), founder_allocator_id(record))
            };
            if let Some(network) = cluster_network
                && let Err(error) = replicated.publish_cluster_network(network).await
            {
                eprintln!("failed to publish Cluster network: {error}");
            }
            if let Some(id) = founder_id
                && let Err(error) = replicated.publish_founder_allocator(&id).await
            {
                eprintln!("failed to publish Allocator: {error}");
            }
            let publication = replicated.machine_publication().await;
            let machine = {
                let local = local
                    .lock()
                    .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
                publication.publishable_machine(local.record())
            };
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

#[must_use]
pub(super) fn founder_allocator_id(record: &LocalMachineRecord) -> Option<MachineId> {
    (record.phase() == LocalMachinePhase::Participating && record.cluster_network().is_some())
        .then_some(record.id())
}
