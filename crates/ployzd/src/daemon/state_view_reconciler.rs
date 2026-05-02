//! Background task that maintains the local view-stream `sources` set in
//! sync with the cluster's `machine_directory` KV.
//!
//! Audience for failures (per `AGENTS.md` failure-audience rule): the
//! consumers that read `machine_state_view` / `instance_state_view` for
//! their snapshot of cluster state. If we let the source list drift the
//! view goes stale silently — so on a reconcile failure we record the
//! error and the next reconcile retries; persistent failures need to
//! surface through `ployzctl status` (added in the mirror-lag work).
use std::collections::BTreeSet;

use ployz_nats::NatsStore;
use ployz_nats::buckets::reconcile_view_streams;
use ployz_nats::store::state_directory::{self, DirectoryEvent, MachineDirectoryEntry};
use ployz_types::error::{Error, Result};
use ployz_types::model::MachineId;
use std::future::Future;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Cancellable owner for the state-view reconciler task. Mirrors the
/// shape of `CertificateRenewalTask` so the daemon can shut it down
/// alongside the other background workers.
pub(crate) struct StateViewReconcilerTask {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl StateViewReconcilerTask {
    pub(crate) fn spawn<F>(run: impl FnOnce(CancellationToken) -> F) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(cancel.clone()));
        Self { cancel, task }
    }

    pub(crate) async fn shutdown(self) {
        self.cancel.cancel();
        if let Err(error) = self.task.await {
            warn!(?error, "state view reconciler shutdown error");
        }
    }
}

/// Open the `machine_directory` subscription, reconcile view streams from
/// the snapshot, then drive an event loop that re-reconciles on every
/// directory change.
///
/// Errors during the snapshot phase fail the spawn (the daemon's `mesh up`
/// surfaces this loudly to the operator). Errors during steady-state
/// reconciliation are logged and the task continues — the next event
/// will drive a retry. Persistent staleness is surfaced through
/// `mirror_lag` in the status work.
pub(crate) async fn spawn(store: NatsStore) -> Result<StateViewReconcilerTask> {
    let (snapshot, mut events) = state_directory::subscribe(&store).await.map_err(|error| {
        Error::operation(
            "state_view_reconciler_subscribe",
            format!("subscribe directory: {error}"),
        )
    })?;
    let mut tracked: BTreeSet<MachineId> = snapshot
        .iter()
        .map(|entry| entry.machine_id.clone())
        .collect();
    apply(&store, &tracked).await?;
    info!(count = tracked.len(), "state view reconciler initialised");

    Ok(StateViewReconcilerTask::spawn(|cancel| async move {
        loop {
            let event = tokio::select! {
                () = cancel.cancelled() => break,
                next = events.recv() => next,
            };
            let Some(event) = event else {
                warn!("state view reconciler event stream closed");
                break;
            };
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    warn!(?error, "state view reconciler subscription error");
                    continue;
                }
            };
            let changed = apply_event(&mut tracked, event);
            if !changed {
                continue;
            }
            if let Err(error) = apply(&store, &tracked).await {
                warn!(?error, "state view reconcile after event failed");
            }
        }
    }))
}

fn apply_event(tracked: &mut BTreeSet<MachineId>, event: DirectoryEvent) -> bool {
    match event {
        DirectoryEvent::Added(MachineDirectoryEntry { machine_id, .. }) => {
            tracked.insert(machine_id)
        }
        DirectoryEvent::Updated(_) => false,
        DirectoryEvent::Removed(machine_id) => tracked.remove(&machine_id),
    }
}

async fn apply(store: &NatsStore, tracked: &BTreeSet<MachineId>) -> Result<()> {
    let ids: Vec<MachineId> = tracked.iter().cloned().collect();
    reconcile_view_streams(store, &ids).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(id: &str) -> MachineId {
        MachineId(id.into())
    }

    fn entry(id: &str) -> MachineDirectoryEntry {
        MachineDirectoryEntry::for_machine(machine(id))
    }

    #[test]
    fn added_event_inserts_into_tracked_set() {
        let mut tracked = BTreeSet::new();
        let changed = apply_event(&mut tracked, DirectoryEvent::Added(entry("m1")));
        assert!(changed);
        assert!(tracked.contains(&machine("m1")));
    }

    #[test]
    fn duplicate_added_event_is_noop() {
        let mut tracked = BTreeSet::new();
        tracked.insert(machine("m1"));
        let changed = apply_event(&mut tracked, DirectoryEvent::Added(entry("m1")));
        assert!(!changed);
    }

    #[test]
    fn updated_event_does_not_change_tracked_set() {
        let mut tracked = BTreeSet::new();
        tracked.insert(machine("m1"));
        let changed = apply_event(&mut tracked, DirectoryEvent::Updated(entry("m1")));
        assert!(!changed);
        assert!(tracked.contains(&machine("m1")));
    }

    #[test]
    fn removed_event_drops_machine() {
        let mut tracked = BTreeSet::new();
        tracked.insert(machine("m1"));
        let changed = apply_event(&mut tracked, DirectoryEvent::Removed(machine("m1")));
        assert!(changed);
        assert!(tracked.is_empty());
    }

    #[test]
    fn removed_event_for_unknown_machine_is_noop() {
        let mut tracked = BTreeSet::new();
        let changed = apply_event(&mut tracked, DirectoryEvent::Removed(machine("ghost")));
        assert!(!changed);
    }
}
