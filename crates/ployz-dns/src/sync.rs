use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::DnsError;
use crate::snapshot::{SharedDnsSnapshot, project_dns};

// ---------------------------------------------------------------------------
// DnsStore trait — consumer contract
// ---------------------------------------------------------------------------

pub trait DnsStore: Send + Sync {
    fn subscribe_routing_events(
        &self,
    ) -> impl Future<
        Output = Result<
            (
                ployz_types::model::RoutingState,
                mpsc::Receiver<ployz_types::model::RoutingEvent>,
            ),
            DnsError,
        >,
    > + Send
    + '_;
}

// ---------------------------------------------------------------------------
// Sync logic
// ---------------------------------------------------------------------------

pub async fn run_sync_loop<S>(store: S, snapshot: SharedDnsSnapshot) -> Result<(), DnsError>
where
    S: DnsStore + Send + Sync + 'static,
{
    loop {
        let (mut state, mut routing_rx) = store.subscribe_routing_events().await?;
        replace_dns_snapshot(&state, &snapshot);

        while let Some(event) = routing_rx.recv().await {
            apply_routing_event(&mut state, event);
            while let Ok(event) = routing_rx.try_recv() {
                apply_routing_event(&mut state, event);
            }
            replace_dns_snapshot(&state, &snapshot);
        }
        warn!("dns routing event stream closed; resubscribing");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn replace_dns_snapshot(state: &ployz_types::model::RoutingState, snapshot: &SharedDnsSnapshot) {
    let next = project_dns(state);
    let service_count: usize = next.services.values().map(HashMap::len).sum();
    snapshot.replace(next);
    info!(service_count, "dns snapshot refreshed");
}

fn apply_routing_event(
    state: &mut ployz_types::model::RoutingState,
    event: ployz_types::model::RoutingEvent,
) {
    use ployz_types::model::RoutingEvent;
    match event {
        RoutingEvent::MachineAdded(record) => state.machines.push(record),
        RoutingEvent::MachineUpdated { old, new } => {
            replace_by(&mut state.machines, |record| record.id == old.id, new);
        }
        RoutingEvent::MachineRemoved(record) => {
            state.machines.retain(|machine| machine.id != record.id);
        }
        RoutingEvent::RevisionAdded(record) => state.revisions.push(record),
        RoutingEvent::RevisionUpdated { old, new } => {
            replace_by(
                &mut state.revisions,
                |record| {
                    record.namespace == old.namespace
                        && record.service == old.service
                        && record.revision_hash == old.revision_hash
                },
                new,
            );
        }
        RoutingEvent::RevisionRemoved(record) => {
            state.revisions.retain(|revision| {
                !(revision.namespace == record.namespace
                    && revision.service == record.service
                    && revision.revision_hash == record.revision_hash)
            });
        }
        RoutingEvent::ReleaseAdded(record) => state.releases.push(record),
        RoutingEvent::ReleaseUpdated { old, new } => {
            replace_by(
                &mut state.releases,
                |record| record.namespace == old.namespace && record.service == old.service,
                new,
            );
        }
        RoutingEvent::ReleaseRemoved(record) => {
            state.releases.retain(|release| {
                !(release.namespace == record.namespace && release.service == record.service)
            });
        }
        RoutingEvent::InstanceAdded(record) => state.instances.push(record),
        RoutingEvent::InstanceUpdated { old, new } => {
            replace_by(
                &mut state.instances,
                |record| record.instance_id == old.instance_id,
                new,
            );
        }
        RoutingEvent::InstanceRemoved(record) => {
            state
                .instances
                .retain(|instance| instance.instance_id != record.instance_id);
        }
    }
}

fn replace_by<T>(values: &mut Vec<T>, mut matches: impl FnMut(&T) -> bool, replacement: T) {
    if let Some(value) = values.iter_mut().find(|value| matches(value)) {
        *value = replacement;
    } else {
        values.push(replacement);
    }
}

pub fn spawn_sync_thread_with_store<S>(
    store: S,
    snapshot: SharedDnsSnapshot,
) -> Result<(), DnsError>
where
    S: DnsStore + Send + Sync + 'static,
{
    std::thread::Builder::new()
        .name("ployz-dns-sync".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    warn!(?err, "failed to create dns sync runtime");
                    return;
                }
            };
            runtime.block_on(async move {
                if let Err(err) = run_sync_loop(store, snapshot).await {
                    warn!(?err, "dns sync loop exited");
                }
            });
        })
        .map_err(|err| DnsError::Runtime(err.to_string()))?;
    Ok(())
}
